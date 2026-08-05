use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub ignore: Ignore,
    #[serde(default)]
    pub fix: Fix,
    #[serde(default)]
    pub output: Output,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Network {
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u16,
    #[serde(default = "default_per_host")]
    pub per_host: u16,
    #[serde(
        default = "default_timeout",
        deserialize_with = "duration",
        serialize_with = "serialize_duration"
    )]
    pub timeout: Duration,
    #[serde(default = "default_retries")]
    pub retries: u8,
    #[serde(default)]
    pub user_agent: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Cache {
    #[serde(
        default = "default_ttl",
        deserialize_with = "duration",
        serialize_with = "serialize_duration"
    )]
    pub ttl: Duration,
    #[serde(default)]
    pub dir: Option<PathBuf>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Auth {
    #[serde(default = "default_auth")]
    pub auth: String,
    #[serde(default = "default_browser")]
    pub browser: String,
}
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Ignore {
    #[serde(default)]
    pub local_links: bool,
    #[serde(default)]
    pub exclude_url: Vec<String>,
    #[serde(default)]
    pub exclude_domain: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Fix {
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub backup: bool,
    #[serde(default)]
    pub copy: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Output {
    #[serde(default = "default_confidence")]
    pub fail_on: String,
}
impl Default for Network {
    fn default() -> Self {
        Self {
            max_concurrency: default_concurrency(),
            per_host: default_per_host(),
            timeout: default_timeout(),
            retries: default_retries(),
            user_agent: None,
        }
    }
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            ttl: default_ttl(),
            dir: None,
        }
    }
}
impl Default for Auth {
    fn default() -> Self {
        Self {
            auth: default_auth(),
            browser: default_browser(),
        }
    }
}
impl Default for Output {
    fn default() -> Self {
        Self {
            fail_on: default_confidence(),
        }
    }
}
fn default_concurrency() -> u16 {
    128
}
fn default_per_host() -> u16 {
    4
}
fn default_timeout() -> Duration {
    Duration::from_secs(20)
}
fn default_retries() -> u8 {
    2
}
fn default_ttl() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}
fn default_auth() -> String {
    "cookies".into()
}
fn default_browser() -> String {
    "auto".into()
}
fn default_confidence() -> String {
    "suspect".into()
}
fn duration<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let value = String::deserialize(deserializer)?;
    humantime::parse_duration(&value).map_err(serde::de::Error::custom)
}
fn serialize_duration<S: serde::Serializer>(
    value: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&humantime::format_duration(*value).to_string())
}

pub fn resolve(scan_path: &Path) -> Result<Settings, String> {
    let toml = discover(scan_path)?;
    let mut figment = Figment::from(Serialized::defaults(Settings::default()));
    if let Some(path) = toml {
        figment = figment.merge(Toml::file(path));
    }
    // STALELINK_NETWORK_TIMEOUT maps to network.timeout. Figment's default
    // lowercase split makes the documented uppercase convention portable.
    figment = figment.merge(Env::prefixed("STALELINK_").split("_").lowercase(true));
    let mut settings: Settings = figment.extract().map_err(|error| error.to_string())?;
    apply_env(&mut settings)?;
    Ok(settings)
}

fn apply_env(settings: &mut Settings) -> Result<(), String> {
    let value = |name: &str| std::env::var(name).ok();
    if let Some(value) = value("STALELINK_NETWORK_MAX_CONCURRENCY") {
        settings.network.max_concurrency = value
            .parse()
            .map_err(|_| "invalid STALELINK_NETWORK_MAX_CONCURRENCY".to_owned())?;
    }
    if let Some(value) = value("STALELINK_NETWORK_PER_HOST") {
        settings.network.per_host = value
            .parse()
            .map_err(|_| "invalid STALELINK_NETWORK_PER_HOST".to_owned())?;
    }
    if let Some(value) = value("STALELINK_NETWORK_TIMEOUT") {
        settings.network.timeout = humantime::parse_duration(&value)
            .map_err(|error| format!("invalid STALELINK_NETWORK_TIMEOUT: {error}"))?;
    }
    if let Some(value) = value("STALELINK_NETWORK_RETRIES") {
        settings.network.retries = value
            .parse()
            .map_err(|_| "invalid STALELINK_NETWORK_RETRIES".to_owned())?;
    }
    if let Some(value) = value("STALELINK_NETWORK_USER_AGENT") {
        settings.network.user_agent = Some(value);
    }
    if let Some(value) = value("STALELINK_CACHE_TTL") {
        settings.cache.ttl = humantime::parse_duration(&value)
            .map_err(|error| format!("invalid STALELINK_CACHE_TTL: {error}"))?;
    }
    if let Some(value) = value("STALELINK_CACHE_DIR") {
        settings.cache.dir = Some(value.into());
    }
    if let Some(value) = value("STALELINK_IGNORE_LOCAL_LINKS") {
        settings.ignore.local_links = value
            .parse()
            .map_err(|_| "invalid STALELINK_IGNORE_LOCAL_LINKS".to_owned())?;
    }
    if let Some(value) = value("STALELINK_OUTPUT_FAIL_ON") {
        settings.output.fail_on = value;
    }
    if let Some(value) = value("STALELINK_AUTH_AUTH") {
        settings.auth.auth = value;
    }
    if let Some(value) = value("STALELINK_AUTH_BROWSER") {
        settings.auth.browser = value;
    }
    Ok(())
}

fn discover(scan_path: &Path) -> Result<Option<PathBuf>, String> {
    let mut directory = if scan_path.is_dir() {
        scan_path.to_path_buf()
    } else {
        scan_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    loop {
        let candidate = directory.join("stalelink.toml");
        if candidate.is_file() {
            validate_toml(&candidate)?;
            return Ok(Some(candidate));
        }
        if !directory.pop() {
            return Ok(None);
        }
    }
}

fn validate_toml(path: &Path) -> Result<(), String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let value: toml::Value = source
        .parse()
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    let known = [
        (
            "network",
            [
                "max-concurrency",
                "per-host",
                "timeout",
                "retries",
                "user-agent",
            ]
            .as_slice(),
        ),
        ("cache", ["ttl", "dir"].as_slice()),
        ("auth", ["auth", "browser"].as_slice()),
        (
            "ignore",
            ["local-links", "exclude-url", "exclude-domain", "exclude"].as_slice(),
        ),
        ("fix", ["write", "backup", "copy"].as_slice()),
        ("output", ["fail-on"].as_slice()),
    ];
    let Some(table) = value.as_table() else {
        return Err("config must be a TOML table".into());
    };
    for (section, content) in table {
        let Some((_, keys)) = known.iter().find(|(name, _)| *name == section) else {
            return Err(unknown(section, known.iter().map(|(name, _)| *name)));
        };
        let Some(values) = content.as_table() else {
            return Err(format!("config section [{section}] must be a table"));
        };
        for key in values.keys() {
            if !keys.contains(&key.as_str()) {
                return Err(unknown(
                    &format!("{section}.{key}"),
                    keys.iter().copied().map(|key| format!("{section}.{key}")),
                ));
            }
        }
    }
    Ok(())
}
fn unknown<I: IntoIterator<Item = S>, S: AsRef<str>>(value: &str, known: I) -> String {
    let suggestion = known
        .into_iter()
        .min_by_key(|candidate| distance(value, candidate.as_ref()))
        .map(|candidate| candidate.as_ref().to_owned());
    format!(
        "unknown configuration key `{value}`{}",
        suggestion.map_or_else(String::new, |key| format!("; did you mean `{key}`?"))
    )
}
fn distance(left: &str, right: &str) -> usize {
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.bytes().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.bytes().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(a != b))
                .min(row[j] + 1)
                .min(above + 1);
            diagonal = above;
        }
    }
    row[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_distance_finds_nearest_key() {
        assert!(
            unknown("network.timout", ["network.timeout", "network.retries"])
                .contains("network.timeout")
        );
    }

    #[test]
    fn nearest_config_is_discovered_upward() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(
            directory.path().join("stalelink.toml"),
            "[network]\nretries = 7\n",
        )
        .unwrap();
        assert_eq!(resolve(&nested).unwrap().network.retries, 7);
    }
}
