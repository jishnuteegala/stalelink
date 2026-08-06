use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use stalelink_core::{
    check::{CheckFuture, Checker},
    model::{Confidence, Verdict},
};
use url::Url;

const SCHEMA_VERSION: i32 = 1;

pub struct VerdictCache {
    path: PathBuf,
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: u64,
    pub size: u64,
}

impl VerdictCache {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("creating cache directory: {error}"))?;
        }
        let connection =
            Connection::open(&path).map_err(|error| format!("opening cache: {error}"))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| error.to_string())?;
        let version: i32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if version != SCHEMA_VERSION {
            connection
                .execute_batch("DROP TABLE IF EXISTS verdicts; DROP TABLE IF EXISTS cache_stats;")
                .map_err(|error| error.to_string())?;
        }
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS verdicts (
                url TEXT PRIMARY KEY NOT NULL,
                verdict_json TEXT,
                checked_at INTEGER NOT NULL,
                tier INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cache_stats (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                hits INTEGER NOT NULL DEFAULT 0,
                misses INTEGER NOT NULL DEFAULT 0
            );
            INSERT OR IGNORE INTO cache_stats (singleton) VALUES (1);",
            )
            .map_err(|error| error.to_string())?;
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub fn clear(path: &Path) -> Result<(), String> {
        // No process-local connection is retained by cache subcommands, so Windows can delete it.
        for suffix in ["", "-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{}{}", path.display(), suffix));
            if candidate.exists() {
                fs::remove_file(candidate).map_err(|error| format!("clearing cache: {error}"))?;
            }
        }
        Ok(())
    }

    pub fn stats(&self) -> Result<CacheStats, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "cache lock poisoned".to_owned())?;
        let (hits, misses) = connection
            .query_row(
                "SELECT hits, misses FROM cache_stats WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let entries = connection
            .query_row("SELECT COUNT(*) FROM verdicts", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        let size = ["", "-wal", "-shm"]
            .iter()
            .map(|suffix| fs::metadata(format!("{}{}", self.path.display(), suffix)))
            .filter_map(Result::ok)
            .map(|metadata| metadata.len())
            .sum();
        Ok(CacheStats {
            hits,
            misses,
            entries,
            size,
        })
    }

    fn get(
        &self,
        url: &Url,
        ttl: Duration,
        current_cap: u8,
    ) -> Result<Option<Option<Verdict>>, String> {
        let key = normalised_url(url);
        let now = unix_seconds();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "cache lock poisoned".to_owned())?;
        let row: Option<(Option<String>, i64, u8)> = connection
            .query_row(
                "SELECT verdict_json, checked_at, tier FROM verdicts WHERE url = ?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("reading cache: {error}"))?;
        let result = match row {
            Some((Some(json), checked_at, tier)) => {
                let age = now.saturating_sub(checked_at as u64);
                if age > ttl.as_secs() {
                    None
                } else {
                    match serde_json::from_str::<Verdict>(&json) {
                        Ok(verdict) if reusable(&verdict, tier, current_cap) => Some(Some(verdict)),
                        Ok(_) => None,
                        Err(_) => {
                            // A row from an older or interrupted writer is not reusable.
                            connection
                                .execute("DELETE FROM verdicts WHERE url = ?1", [&key])
                                .map_err(|error| {
                                    format!("removing malformed cache row: {error}")
                                })?;
                            None
                        }
                    }
                }
            }
            Some((None, checked_at, _)) => {
                let age = now.saturating_sub(checked_at as u64);
                if age <= ttl.as_secs() {
                    Some(None)
                } else {
                    None
                }
            }
            None => None,
        };
        let column = if result.is_some() { "hits" } else { "misses" };
        connection
            .execute(
                &format!("UPDATE cache_stats SET {column} = {column} + 1 WHERE singleton = 1"),
                [],
            )
            .map_err(|error| format!("updating cache statistics: {error}"))?;
        Ok(result)
    }

    fn put(&self, url: &Url, verdict: &Option<Verdict>) -> Result<(), String> {
        let json = verdict
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| format!("serializing cache verdict: {error}"))?;
        let tier = verdict.as_ref().map_or(1, |verdict| verdict.tier);
        let connection = self
            .connection
            .lock()
            .map_err(|_| "cache lock poisoned".to_owned())?;
        connection.execute(
            "INSERT INTO verdicts (url, verdict_json, checked_at, tier) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(url) DO UPDATE SET verdict_json = excluded.verdict_json, checked_at = excluded.checked_at, tier = excluded.tier",
            params![normalised_url(url), json, unix_seconds() as i64, tier],
        )
        .map_err(|error| format!("writing cache: {error}"))?;
        Ok(())
    }
}

pub struct CachingChecker<C> {
    inner: C,
    cache: Arc<VerdictCache>,
    ttl: Duration,
    current_cap: u8,
    refresh: bool,
    error: Arc<Mutex<Option<String>>>,
}
impl<C> CachingChecker<C> {
    pub fn new(
        inner: C,
        cache: VerdictCache,
        ttl: Duration,
        current_cap: u8,
        refresh: bool,
    ) -> Self {
        Self {
            inner,
            cache: Arc::new(cache),
            ttl,
            current_cap,
            refresh,
            error: Arc::new(Mutex::new(None)),
        }
    }
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }
    fn record_error(&self, error: String) {
        if let Ok(mut recorded) = self.error.lock()
            && recorded.is_none()
        {
            *recorded = Some(error);
        }
    }
}
impl<C: Checker> Checker for CachingChecker<C> {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            if !self.refresh {
                let cache = Arc::clone(&self.cache);
                let key = url.clone();
                let ttl = self.ttl;
                let current_cap = self.current_cap;
                match tokio::task::spawn_blocking(move || cache.get(&key, ttl, current_cap)).await {
                    Ok(Ok(Some(verdict))) => return verdict,
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => self.record_error(error),
                    Err(error) => self.record_error(format!("cache worker failed: {error}")),
                }
            }
            let verdict = self.inner.check(url.clone()).await;
            let cache = Arc::clone(&self.cache);
            let value = verdict.clone();
            let key = url.clone();
            match tokio::task::spawn_blocking(move || cache.put(&key, &value)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.record_error(error),
                Err(error) => self.record_error(format!("cache worker failed: {error}")),
            }
            verdict
        })
    }
}

pub fn reusable(verdict: &Verdict, cached_tier: u8, current_cap: u8) -> bool {
    cached_tier >= current_cap || verdict.confidence != Confidence::AuthWalled
}

pub fn normalised_url(url: &Url) -> String {
    let mut url = url.clone();
    let _ = url.set_scheme(&url.scheme().to_ascii_lowercase());
    if let Some(host) = url.host_str() {
        let _ = url.set_host(Some(&host.to_ascii_lowercase()));
    }
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        let _ = url.set_port(None);
    }
    url.set_fragment(None);
    url.to_string()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use stalelink_core::model::{Evidence, Reason};
    fn verdict(confidence: Confidence, tier: u8) -> Verdict {
        Verdict {
            confidence,
            reason: Reason::LoginWall,
            evidence: vec![Evidence {
                kind: "test".into(),
                detail: "test".into(),
            }],
            checked_at: Utc::now(),
            tier,
        }
    }
    #[test]
    fn cache_normalises_authority_without_changing_path_or_query() {
        assert_eq!(
            normalised_url(
                &"HTTPS://EXAMPLE.TEST:443/Path?Key=Value#fragment"
                    .parse()
                    .unwrap()
            ),
            "https://example.test/Path?Key=Value"
        );
    }
    #[test]
    fn tier_one_auth_walled_verdict_is_not_reused_for_higher_cap() {
        assert!(!reusable(&verdict(Confidence::AuthWalled, 1), 1, 2));
        assert!(reusable(&verdict(Confidence::DeadCertain, 1), 1, 2));
        assert!(reusable(&verdict(Confidence::AuthWalled, 2), 2, 2));
    }
}
