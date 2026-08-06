#[cfg(any(test, feature = "live-browser"))]
use std::sync::atomic::AtomicUsize;
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use stalelink_core::{
    check::{CheckFuture, Checker},
    model::{Evidence, Reason, Verdict},
};
use url::Url;

#[derive(Debug, Clone, Copy)]
pub enum Browser {
    Auto,
    Chrome,
    Edge,
    Brave,
    Chromium,
    Firefox,
}

impl Browser {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Chrome => "Chrome",
            Self::Edge => "Edge",
            Self::Brave => "Brave",
            Self::Chromium => "Chromium",
            Self::Firefox => "Firefox",
        }
    }
}

#[derive(Clone)]
struct Cookie {
    domain: String,
    name: String,
    value: String,
}

pub fn snapshot(browser: Browser) -> Result<Vec<(String, String, String)>, String> {
    if let Some(directory) = std::env::var_os("STALELINK_COOKIE_STORE_DIR") {
        let path = std::path::PathBuf::from(directory).join("cookies.json");
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        return serde_json::from_str(&source)
            .map_err(|error| format!("parsing {}: {error}", path.display()));
    }
    let cookies = match browser {
        Browser::Auto => rookie::chrome(None)
            .or_else(|_| rookie::edge(None))
            .or_else(|_| rookie::brave(None))
            .or_else(|_| rookie::chromium(None))
            .or_else(|_| rookie::firefox(None)),
        Browser::Chrome => rookie::chrome(None),
        Browser::Edge => rookie::edge(None),
        Browser::Brave => rookie::brave(None),
        Browser::Chromium => rookie::chromium(None),
        Browser::Firefox => rookie::firefox(None),
    }
    .map_err(|error| error.to_string())?;
    Ok(cookies
        .into_iter()
        .map(|cookie| (cookie.domain, cookie.name, cookie.value))
        .collect())
}

pub struct CookieChecker {
    client: reqwest::Client,
    cookies: CookieSource,
}

enum CookieSource {
    Snapshot(Vec<Cookie>),
    Browser {
        browser: Browser,
        snapshot: OnceLock<Result<Vec<Cookie>, String>>,
        reported: AtomicBool,
    },
}

impl CookieChecker {
    pub fn new(
        timeout: Duration,
        user_agent: String,
        snapshot: Vec<(String, String, String)>,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(user_agent)
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()?,
            cookies: CookieSource::Snapshot(cookies(snapshot)),
        })
    }

    pub fn from_browser(
        timeout: Duration,
        user_agent: String,
        browser: Browser,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(user_agent)
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()?,
            cookies: CookieSource::Browser {
                browser,
                snapshot: OnceLock::new(),
                reported: AtomicBool::new(false),
            },
        })
    }

    fn cookies(&self) -> Option<&[Cookie]> {
        match &self.cookies {
            CookieSource::Snapshot(cookies) => Some(cookies),
            CookieSource::Browser {
                browser,
                snapshot: store,
                reported,
            } => match store.get_or_init(|| snapshot(*browser).map(cookies)) {
                Ok(cookies) if !cookies.is_empty() => {
                    if !reported.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "notice: reading {} browser-profile cookies for escalated links",
                            browser.name()
                        );
                    }
                    Some(cookies)
                }
                Ok(_) => {
                    if !reported.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "warning: {} has no readable cookie store; check its profile or use --auth off",
                            browser.name()
                        );
                    }
                    None
                }
                Err(error) => {
                    if !reported.swap(true, Ordering::Relaxed) {
                        #[cfg(windows)]
                        eprintln!(
                            "warning: {} cookie store is unavailable ({error}); Chrome app-bound cookies may require elevation. Run from an elevated prompt or choose another browser",
                            browser.name()
                        );
                        #[cfg(not(windows))]
                        eprintln!(
                            "warning: {} cookie store is unavailable ({error}); close the browser or choose another browser",
                            browser.name()
                        );
                    }
                    None
                }
            },
        }
    }
}

fn cookies(snapshot: Vec<(String, String, String)>) -> Vec<Cookie> {
    snapshot
        .into_iter()
        .map(|(domain, name, value)| Cookie {
            domain,
            name,
            value,
        })
        .collect()
}

impl Checker for CookieChecker {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            let host = url.host_str().unwrap_or_default();
            let cookie = self
                .cookies()?
                .iter()
                .filter(|cookie| host.ends_with(cookie.domain.trim_start_matches('.')))
                .map(|cookie| format!("{}={}", cookie.name, cookie.value))
                .collect::<Vec<_>>()
                .join("; ");
            let response = self
                .client
                .get(url)
                .header(reqwest::header::COOKIE, cookie)
                .send()
                .await
                .ok()?;
            if response.status().is_success() {
                return None;
            }
            let reason = Reason::HttpStatus(response.status().as_u16());
            Some(Verdict {
                confidence: reason.confidence(),
                reason,
                evidence: vec![Evidence {
                    kind: "cookie-status".into(),
                    detail: response.status().to_string(),
                }],
                checked_at: Utc::now(),
                tier: 2,
            })
        })
    }
}

#[cfg(any(test, feature = "live-browser"))]
pub trait PageDriver: Send + Sync {
    fn check_page(&self, url: Url) -> CheckFuture<'_>;
}

#[cfg(any(test, feature = "live-browser"))]
pub struct BrowserChecker<D> {
    drivers: [D; 4],
    used: AtomicUsize,
}

#[cfg(any(test, feature = "live-browser"))]
impl<D> BrowserChecker<D> {
    pub fn new(drivers: [D; 4]) -> Self {
        Self {
            drivers,
            used: AtomicUsize::new(0),
        }
    }
}

#[cfg(any(test, feature = "live-browser"))]
impl<D: PageDriver> Checker for BrowserChecker<D> {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            if self.used.fetch_add(1, Ordering::Relaxed) >= 25 {
                eprintln!("warning: tier-3 browser budget exhausted after 25 links");
                let reason = Reason::LoginWall;
                return Some(Verdict {
                    confidence: reason.confidence(),
                    reason,
                    evidence: vec![Evidence {
                        kind: "tier-3-budget-exhausted".into(),
                        detail: "tier 3 is limited to 25 links per run".into(),
                    }],
                    checked_at: Utc::now(),
                    tier: 3,
                });
            }
            let slot = self.used.load(Ordering::Relaxed).saturating_sub(1) % self.drivers.len();
            self.drivers[slot].check_page(url).await.map(|mut verdict| {
                verdict.tier = 3;
                verdict
            })
        })
    }
}

#[cfg(feature = "live-browser")]
pub struct CdpPageDriver {
    browser: std::sync::Arc<chromiumoxide::browser::Browser>,
}

#[cfg(feature = "live-browser")]
impl CdpPageDriver {
    pub async fn launch(
        profile: &std::path::Path,
        debug_url: Option<&str>,
    ) -> Result<[Self; 4], String> {
        use chromiumoxide::browser::{Browser, BrowserConfig};
        use futures::StreamExt;

        let (browser, mut handler) = if let Some(url) = debug_url {
            Browser::connect(url.to_owned())
                .await
                .map_err(|error| error.to_string())?
        } else {
            let config = BrowserConfig::builder()
                .user_data_dir(profile)
                .build()
                .map_err(|error| error.to_string())?;
            Browser::launch(config)
                .await
                .map_err(|error| error.to_string())?
        };
        tokio::spawn(async move { while handler.next().await.is_some() {} });
        let browser = std::sync::Arc::new(browser);
        Ok(std::array::from_fn(|_| Self {
            browser: std::sync::Arc::clone(&browser),
        }))
    }
}

#[cfg(feature = "live-browser")]
impl PageDriver for CdpPageDriver {
    fn check_page(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            let page = self.browser.new_page("about:blank").await.ok()?;
            tokio::time::timeout(Duration::from_secs(30), page.goto(url.as_str()))
                .await
                .ok()?
                .ok()?;
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stalelink_core::{check::Checker, model::Confidence};

    struct AuthWall;
    impl PageDriver for AuthWall {
        fn check_page(&self, _: Url) -> CheckFuture<'_> {
            Box::pin(async {
                let reason = Reason::LoginWall;
                Some(Verdict {
                    confidence: reason.confidence(),
                    reason,
                    evidence: vec![],
                    checked_at: Utc::now(),
                    tier: 3,
                })
            })
        }
    }

    #[tokio::test]
    async fn browser_budget_returns_auth_wall_evidence_after_twenty_five_links() {
        let checker = BrowserChecker::new([AuthWall, AuthWall, AuthWall, AuthWall]);
        for index in 0..25 {
            assert!(
                checker
                    .check(format!("https://example.test/{index}").parse().unwrap())
                    .await
                    .is_some()
            );
        }
        let verdict = checker
            .check("https://example.test/overflow".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.confidence, Confidence::AuthWalled);
        assert_eq!(verdict.evidence[0].kind, "tier-3-budget-exhausted");
    }

    #[tokio::test]
    async fn browser_pool_uses_all_four_drivers() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Driver(AtomicBool);
        impl PageDriver for Driver {
            fn check_page(&self, _: Url) -> CheckFuture<'_> {
                self.0.store(true, Ordering::SeqCst);
                Box::pin(async { None })
            }
        }
        let checker = BrowserChecker::new([
            Driver(AtomicBool::new(false)),
            Driver(AtomicBool::new(false)),
            Driver(AtomicBool::new(false)),
            Driver(AtomicBool::new(false)),
        ]);
        for index in 0..4 {
            assert!(
                checker
                    .check(format!("https://example.test/{index}").parse().unwrap())
                    .await
                    .is_none()
            );
        }
        assert!(
            checker
                .drivers
                .iter()
                .all(|driver| driver.0.load(Ordering::SeqCst))
        );
    }
}
