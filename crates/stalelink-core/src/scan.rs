use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt, stream};
use tokio::sync::Semaphore;
use url::Url;

use crate::{
    check::Checker,
    extract::{SourceDocument, extract},
    model::{Finding, FixOrigin, Fixability, FoundLink, SuggestedFix, Verdict},
    walk::{WalkOptions, detect_format, walk},
};

pub struct ScanInput {
    pub paths: Vec<PathBuf>,
    pub walk: WalkOptions,
    pub max_concurrency: usize,
    pub exclude_urls: Vec<regex::Regex>,
    pub exclude_domains: Vec<String>,
}
pub struct ScanReport {
    pub findings: Vec<Finding>,
    pub files_scanned: usize,
    pub links_checked: usize,
    pub links_unique: usize,
    pub duration: Duration,
}
pub trait Progress {
    fn files_walked(&self, _: usize) {}
    fn links_found(&self, _: usize) {}
    fn checks_done(&self, _: usize) {}
}
pub struct NoProgress;
impl Progress for NoProgress {}

pub async fn scan(
    input: ScanInput,
    checker: &dyn Checker,
    progress: &impl Progress,
) -> Result<ScanReport, String> {
    let started = Instant::now();
    if input.max_concurrency == 0 {
        return Err("max_concurrency must be at least 1".into());
    }
    // The walk, file reads, UTF-8 decode, and parser extraction are blocking
    // CPU/IO work; run them on a blocking thread so a large corpus cannot
    // starve the async checking runtime (see architecture decision in #8).
    let paths_input = input.paths.clone();
    let walk_opts = input.walk.clone();
    let (paths, mut links) =
        tokio::task::spawn_blocking(move || collect_links(&paths_input, &walk_opts))
            .await
            .map_err(|e| e.to_string())??;
    progress.files_walked(paths.len());
    links.retain(|link| allowed(link, &input));
    progress.links_found(links.len());
    let mut grouped: HashMap<String, Vec<FoundLink>> = HashMap::new();
    for link in links {
        grouped.entry(link.url.clone()).or_default().push(link);
    }
    let semaphore = Arc::new(Semaphore::new(input.max_concurrency));
    let unique = grouped.len();
    let checks = stream::iter(grouped)
        .map(|(url, occurrences)| {
            let semaphore = semaphore.clone();
            async move {
                let permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
                let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
                let verdict = checker.check(parsed).await;
                drop(permit);
                Ok::<_, String>((occurrences, verdict))
            }
        })
        .buffer_unordered(input.max_concurrency);
    let mut findings = Vec::new();
    let mut checked = 0;
    let mut checks = std::pin::pin!(checks);
    while let Some(result) = checks.next().await {
        let (occurrences, verdict) = result?;
        checked += 1;
        progress.checks_done(checked);
        if let Some(verdict) = verdict {
            findings.extend(occurrences.into_iter().map(|link| Finding {
                url: link.url,
                resolved_url: None,
                source: link.source,
                verdict: verdict.clone(),
                fix: suggested_fix(&verdict),
            }));
        }
    }
    Ok(ScanReport {
        findings,
        files_scanned: paths.len(),
        links_checked: checked,
        links_unique: unique,
        duration: started.elapsed(),
    })
}
fn suggested_fix(verdict: &Verdict) -> Option<SuggestedFix> {
    let (kind, origin) = match verdict.reason {
        crate::model::Reason::PermanentRedirect => ("redirect-target", FixOrigin::RedirectTarget),
        crate::model::Reason::VersionDrift => ("version-upgrade", FixOrigin::VersionUpgrade),
        _ => return None,
    };
    let replacement_url = verdict
        .evidence
        .iter()
        .find(|evidence| evidence.kind == kind)?
        .detail
        .parse::<Url>()
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))?
        .to_string();
    Some(SuggestedFix {
        replacement_url,
        origin,
        fixable: Fixability::Auto,
    })
}
fn collect_links(
    paths_input: &[PathBuf],
    walk_opts: &WalkOptions,
) -> Result<(Vec<PathBuf>, Vec<FoundLink>), String> {
    let paths = walk(paths_input, walk_opts)?;
    let mut links = Vec::new();
    for path in &paths {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let format = detect_format(path).expect("walker filters formats");
        links.extend(
            extract(&SourceDocument {
                path: path.clone(),
                format,
                bytes,
            })
            .map_err(|e| e.0)?,
        );
    }
    Ok((paths, links))
}
fn allowed(link: &FoundLink, input: &ScanInput) -> bool {
    if input
        .exclude_urls
        .iter()
        .any(|regex| regex.is_match(&link.url))
    {
        return false;
    }
    Url::parse(&link.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_none_or(|host| {
            !input
                .exclude_domains
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        check::{CheckFuture, Checker},
        model::{Confidence, Evidence, Reason, Verdict},
    };
    use chrono::Utc;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Fake(Arc<AtomicUsize>);
    impl Checker for Fake {
        fn check(&self, _: Url) -> CheckFuture<'_> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Some(Verdict {
                    confidence: Confidence::DeadCertain,
                    reason: Reason::HttpStatus(404),
                    evidence: vec![Evidence {
                        kind: "status".into(),
                        detail: "404".into(),
                    }],
                    checked_at: Utc::now(),
                    tier: 1,
                })
            })
        }
    }

    struct VerdictChecker(Verdict);
    impl Checker for VerdictChecker {
        fn check(&self, _: Url) -> CheckFuture<'_> {
            let verdict = self.0.clone();
            Box::pin(async move { Some(verdict) })
        }
    }

    #[tokio::test]
    async fn deduplicates_checks_but_reports_each_occurrence() {
        let file = tempfile::NamedTempFile::with_suffix(".txt").unwrap();
        std::fs::write(file.path(), "https://bad.test/x https://bad.test/x").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let report = scan(
            ScanInput {
                paths: vec![file.path().into()],
                walk: WalkOptions::default(),
                max_concurrency: 2,
                exclude_urls: vec![],
                exclude_domains: vec![],
            },
            &Fake(calls.clone()),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.links_unique, 1);
    }

    #[tokio::test]
    async fn scan_creates_complete_automatic_fixes_for_valid_evidence() {
        let redirect = Verdict {
            confidence: Confidence::Outdated,
            reason: Reason::PermanentRedirect,
            evidence: vec![Evidence {
                kind: "redirect-target".into(),
                detail: "https://example.test/current".into(),
            }],
            checked_at: Utc::now(),
            tier: 1,
        };
        let version = Verdict {
            confidence: Confidence::Outdated,
            reason: Reason::VersionDrift,
            evidence: vec![Evidence {
                kind: "version-upgrade".into(),
                detail: "https://example.test/v2/items".into(),
            }],
            checked_at: Utc::now(),
            tier: 1,
        };
        let file = tempfile::NamedTempFile::with_suffix(".txt").unwrap();
        std::fs::write(file.path(), "https://example.test/old").unwrap();
        for (verdict, replacement_url, origin) in [
            (
                redirect,
                "https://example.test/current",
                FixOrigin::RedirectTarget,
            ),
            (
                version,
                "https://example.test/v2/items",
                FixOrigin::VersionUpgrade,
            ),
        ] {
            let report = scan(
                ScanInput {
                    paths: vec![file.path().into()],
                    walk: WalkOptions::default(),
                    max_concurrency: 1,
                    exclude_urls: vec![],
                    exclude_domains: vec![],
                },
                &VerdictChecker(verdict),
                &NoProgress,
            )
            .await
            .unwrap();
            assert_eq!(
                report.findings[0].fix,
                Some(SuggestedFix {
                    replacement_url: replacement_url.into(),
                    origin,
                    fixable: Fixability::Auto,
                })
            );
        }
    }

    #[test]
    fn malformed_or_non_http_fix_evidence_is_rejected() {
        for detail in ["not a url", "file:///local/path"] {
            let verdict = Verdict {
                confidence: Confidence::Outdated,
                reason: Reason::PermanentRedirect,
                evidence: vec![Evidence {
                    kind: "redirect-target".into(),
                    detail: detail.into(),
                }],
                checked_at: Utc::now(),
                tier: 1,
            };
            assert!(suggested_fix(&verdict).is_none());
        }
        let no_fix = Verdict {
            confidence: Confidence::Outdated,
            reason: Reason::StalenessBanner,
            evidence: vec![Evidence {
                kind: "staleness-phrase".into(),
                detail: "deprecated".into(),
            }],
            checked_at: Utc::now(),
            tier: 1,
        };
        assert!(suggested_fix(&no_fix).is_none());
    }
}
