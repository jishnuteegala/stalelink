use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use stalelink_core::{
    check::{CheckFuture, Checker},
    model::{Confidence, Evidence, Reason, Verdict},
};
use url::Url;

use rookie::enums::Cookie as RookieCookie;
#[cfg(any(test, feature = "live-browser"))]
use std::sync::atomic::AtomicUsize;
#[cfg(any(test, feature = "live-browser"))]
use tokio::sync::Mutex;

const MAX_REDIRECTS: usize = 10;

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
    path: String,
    secure: bool,
    expires: Option<u64>,
    name: String,
    value: String,
}

pub fn snapshot(browser: Browser) -> Result<Vec<RookieCookie>, String> {
    if let Some(directory) = std::env::var_os("STALELINK_COOKIE_STORE_DIR") {
        let path = std::path::PathBuf::from(directory).join("cookies.json");
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        if let Ok(legacy) = serde_json::from_str::<Vec<(String, String, String)>>(&source) {
            return Ok(legacy
                .into_iter()
                .map(|(domain, name, value)| RookieCookie {
                    domain,
                    path: "/".into(),
                    secure: false,
                    expires: None,
                    name,
                    value,
                    http_only: false,
                    same_site: 0,
                })
                .collect());
        }
        return serde_json::from_str(&source)
            .map_err(|error| format!("parsing {}: {error}", path.display()));
    }
    match browser {
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
    .map_err(|error| error.to_string())
}

fn cookies(snapshot: Vec<RookieCookie>) -> Vec<Cookie> {
    snapshot
        .into_iter()
        .map(|cookie| Cookie {
            domain: cookie.domain,
            path: cookie.path,
            secure: cookie.secure,
            expires: cookie.expires,
            name: cookie.name,
            value: cookie.value,
        })
        .collect()
}

pub enum Attempt {
    Verdict(Option<Verdict>),
    Unavailable,
}

pub trait AuthTier: Send + Sync {
    fn attempt(&self, url: Url) -> CheckFuture<'_>;
}

impl<T: Checker> AuthTier for T {
    fn attempt(&self, url: Url) -> CheckFuture<'_> {
        self.check(url)
    }
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
        snapshot: Vec<RookieCookie>,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: client(timeout, user_agent)?,
            cookies: CookieSource::Snapshot(cookies(snapshot)),
        })
    }

    pub fn from_browser(
        timeout: Duration,
        user_agent: String,
        browser: Browser,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: client(timeout, user_agent)?,
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
                    warn_cookie_once(
                        reported,
                        *browser,
                        "has no readable cookie store; check its profile or use --auth off",
                    );
                    None
                }
                Err(error) => {
                    warn_cookie_once(
                        reported,
                        *browser,
                        &format!(
                            "cookie store is unavailable ({error}); close the browser or choose another browser"
                        ),
                    );
                    None
                }
            },
        }
    }

    pub async fn attempt(&self, url: Url) -> Attempt {
        let Some(cookies) = self.cookies() else {
            return Attempt::Unavailable;
        };
        let mut current = url;
        let mut redirects = Vec::new();
        for _ in 0..=MAX_REDIRECTS {
            let header = cookie_header(cookies, &current);
            let request = self.client.get(current.clone());
            let request = if header.is_empty() {
                request
            } else {
                request.header(reqwest::header::COOKIE, header)
            };
            let Ok(response) = request.send().await else {
                return Attempt::Unavailable;
            };
            if !response.status().is_redirection() {
                if login_wall(&redirects) {
                    return Attempt::Verdict(Some(auth_verdict(
                        Reason::LoginWall,
                        "cookie-redirect-chain",
                        redirects,
                    )));
                }
                return Attempt::Verdict(
                    (!response.status().is_success())
                        .then(|| status_verdict(response.status().as_u16())),
                );
            }
            let Some(next) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| current.join(value).ok())
            else {
                return Attempt::Verdict(Some(status_verdict(response.status().as_u16())));
            };
            redirects.push(next.clone());
            current = next;
        }
        Attempt::Verdict(Some(status_verdict(310)))
    }
}

fn client(timeout: Duration, user_agent: String) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn warn_cookie_once(reported: &AtomicBool, browser: Browser, cause: &str) {
    if !reported.swap(true, Ordering::Relaxed) {
        #[cfg(windows)]
        eprintln!(
            "warning: {} {cause}; Chrome app-bound cookies may require elevation. Run from an elevated prompt or choose another browser",
            browser.name()
        );
        #[cfg(not(windows))]
        eprintln!("warning: {} {cause}", browser.name());
    }
}

fn cookie_header(cookies: &[Cookie], url: &Url) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let host = url.host_str().unwrap_or_default();
    cookies
        .iter()
        .filter(|cookie| {
            domain_matches(host, &cookie.domain)
                && path_matches(url.path(), &cookie.path)
                && (!cookie.secure || url.scheme() == "https")
                && cookie.expires.is_none_or(|expires| expires > now)
        })
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

fn domain_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim_start_matches('.');
    host.eq_ignore_ascii_case(domain)
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || request
            .strip_prefix(cookie)
            .is_some_and(|rest| cookie.ends_with('/') || rest.starts_with('/'))
}

fn login_wall(redirects: &[Url]) -> bool {
    redirects.iter().any(|url| {
        url.path_segments().is_some_and(|segments| {
            segments
                .into_iter()
                .any(|segment| matches!(segment.to_ascii_lowercase().as_str(), "login" | "signin"))
        }) || url.host_str().unwrap_or_default().split('.').any(|label| {
            matches!(
                label.to_ascii_lowercase().as_str(),
                "auth" | "sso" | "login"
            )
        }) || url
            .query_pairs()
            .any(|(key, _)| key.eq_ignore_ascii_case("returnurl"))
    })
}

fn status_verdict(status: u16) -> Verdict {
    let reason = Reason::HttpStatus(status);
    Verdict {
        confidence: reason.confidence(),
        reason,
        evidence: vec![Evidence {
            kind: "cookie-status".into(),
            detail: status.to_string(),
        }],
        checked_at: Utc::now(),
        tier: 2,
    }
}

fn auth_verdict(reason: Reason, kind: &str, redirects: Vec<Url>) -> Verdict {
    Verdict {
        confidence: reason.confidence(),
        reason,
        evidence: vec![Evidence {
            kind: kind.into(),
            detail: redirects
                .iter()
                .map(Url::as_str)
                .collect::<Vec<_>>()
                .join(" -> "),
        }],
        checked_at: Utc::now(),
        tier: 2,
    }
}

#[derive(Clone, Copy)]
pub enum AuthCap {
    Off,
    Cookies,
    Browser,
}
impl AuthCap {
    pub const fn tier(self) -> u8 {
        match self {
            Self::Off => 1,
            Self::Cookies => 2,
            Self::Browser => 3,
        }
    }
}

pub struct AuthChecker<T1, T2, T3> {
    tier1: T1,
    tier2: Option<T2>,
    tier3: Option<T3>,
    cap: AuthCap,
}
impl<T1, T2, T3> AuthChecker<T1, T2, T3> {
    pub fn new(tier1: T1, tier2: Option<T2>, tier3: Option<T3>, cap: AuthCap) -> Self {
        Self {
            tier1,
            tier2,
            tier3,
            cap,
        }
    }
}
impl<T1: AuthTier, T2: CookieTier, T3: AuthTier> Checker for AuthChecker<T1, T2, T3> {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            let first = self.tier1.attempt(url.clone()).await;
            if self.cap.tier() < 2 || !escalates(&first, 1, known_auth_fenced_host(&url)) {
                return first;
            }
            let Some(tier2) = &self.tier2 else {
                return first;
            };
            let second = match tier2.attempt_cookie(url.clone()).await {
                Attempt::Verdict(verdict) => verdict,
                Attempt::Unavailable => return first,
            };
            if self.cap.tier() < 3 || !escalates(&second, 2, false) {
                return second;
            }
            let Some(tier3) = &self.tier3 else {
                return second;
            };
            tier3.attempt(url).await.or(second)
        })
    }
}

pub trait CookieTier: Send + Sync {
    fn attempt_cookie(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Attempt> + Send + '_>>;
}
impl CookieTier for CookieChecker {
    fn attempt_cookie(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Attempt> + Send + '_>> {
        Box::pin(self.attempt(url))
    }
}

impl AuthTier for CookieChecker {
    fn attempt(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            match CookieChecker::attempt(self, url).await {
                Attempt::Verdict(verdict) => verdict,
                Attempt::Unavailable => None,
            }
        })
    }
}

fn known_auth_fenced_host(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("github.com" | "gitlab.com" | "login.microsoftonline.com")
    )
}
fn bot_fence(verdict: &Verdict) -> bool {
    matches!(verdict.reason, Reason::HttpStatus(403 | 429 | 503))
        && verdict.evidence.iter().any(|e| {
            let text = format!("{} {}", e.kind, e.detail).to_ascii_lowercase();
            text.contains("captcha") || text.contains("challenge") || text.contains("bot")
        })
}
fn escalates(verdict: &Option<Verdict>, tier: u8, known_fence: bool) -> bool {
    let Some(verdict) = verdict else { return false };
    match tier {
        1 => {
            matches!(
                verdict.reason,
                Reason::HttpStatus(401 | 403) | Reason::LoginWall
            ) || (known_fence && bot_fence(verdict))
        }
        2 => verdict.confidence == Confidence::AuthWalled || bot_fence(verdict),
        _ => false,
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
    warned: AtomicBool,
    next: AtomicUsize,
    slots: [Mutex<()>; 4],
}
#[cfg(any(test, feature = "live-browser"))]
impl<D> BrowserChecker<D> {
    pub fn new(drivers: [D; 4]) -> Self {
        Self {
            drivers,
            used: AtomicUsize::new(0),
            warned: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            slots: std::array::from_fn(|_| Mutex::new(())),
        }
    }
}
#[cfg(any(test, feature = "live-browser"))]
impl<D: PageDriver> Checker for BrowserChecker<D> {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            if self.used.fetch_add(1, Ordering::Relaxed) >= 25 {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    eprintln!("warning: tier-3 browser budget exhausted after 25 links");
                }
                return Some(Verdict {
                    confidence: Confidence::AuthWalled,
                    reason: Reason::LoginWall,
                    evidence: vec![Evidence {
                        kind: "tier-3-budget-exhausted".into(),
                        detail: "tier 3 is limited to 25 links per run".into(),
                    }],
                    checked_at: Utc::now(),
                    tier: 3,
                });
            }
            let slot = self.next.fetch_add(1, Ordering::Relaxed) % self.drivers.len();
            let _guard = self.slots[slot].lock().await;
            self.drivers[slot].check_page(url).await.map(|mut verdict| {
                verdict.tier = 3;
                verdict
            })
        })
    }
}

#[cfg(feature = "live-browser")]
#[derive(Debug)]
pub struct CdpPageDriver {
    page: Mutex<chromiumoxide::Page>,
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
                .map_err(|e| e.to_string())?
        } else {
            Browser::launch(
                BrowserConfig::builder()
                    .user_data_dir(profile)
                    .build()
                    .map_err(|e| e.to_string())?,
            )
            .await
            .map_err(|e| e.to_string())?
        };
        tokio::spawn(async move { while handler.next().await.is_some() {} });
        let mut pages = Vec::new();
        for _ in 0..4 {
            pages.push(Self {
                page: Mutex::new(
                    browser
                        .new_page("about:blank")
                        .await
                        .map_err(|e| e.to_string())?,
                ),
            });
        }
        Ok(pages.try_into().expect("exactly four pages"))
    }
}
#[cfg(feature = "live-browser")]
impl PageDriver for CdpPageDriver {
    fn check_page(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            let page = self.page.lock().await;
            tokio::time::timeout(Duration::from_secs(30), page.goto(url.as_str()))
                .await
                .ok()?
                .ok()?;
            let final_url: Url = page.url().await.ok()??.parse().ok()?;
            let content = page.content().await.ok()?.to_ascii_lowercase();
            if login_wall(&[final_url.clone()])
                || content.contains("captcha")
                || content.contains("challenge")
                || content.contains("sign in")
            {
                let reason = Reason::LoginWall;
                return Some(Verdict {
                    confidence: reason.confidence(),
                    reason,
                    evidence: vec![Evidence {
                        kind: "browser-page".into(),
                        detail: final_url.to_string(),
                    }],
                    checked_at: Utc::now(),
                    tier: 3,
                });
            }
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookies_obey_domain_path_secure_and_expiry() {
        let base = Cookie {
            domain: "example.test".into(),
            path: "/private".into(),
            secure: true,
            expires: None,
            name: "s".into(),
            value: "v".into(),
        };
        assert_eq!(
            cookie_header(
                std::slice::from_ref(&base),
                &"https://example.test/private/a".parse().unwrap()
            ),
            "s=v"
        );
        assert!(
            cookie_header(
                std::slice::from_ref(&base),
                &"http://example.test/private".parse().unwrap()
            )
            .is_empty()
        );
        assert!(
            cookie_header(
                std::slice::from_ref(&base),
                &"https://evil-example.test/private".parse().unwrap()
            )
            .is_empty()
        );
        assert!(cookie_header(&[base], &"https://example.test/public".parse().unwrap()).is_empty());
    }

    struct CleanDriver;
    impl PageDriver for CleanDriver {
        fn check_page(&self, _: Url) -> CheckFuture<'_> {
            Box::pin(async { None })
        }
    }

    #[tokio::test]
    async fn browser_budget_reports_every_overflow() {
        let checker = BrowserChecker::new([CleanDriver, CleanDriver, CleanDriver, CleanDriver]);
        for index in 0..25 {
            assert!(
                checker
                    .check(format!("https://example.test/{index}").parse().unwrap())
                    .await
                    .is_none()
            );
        }
        for index in 25..27 {
            let verdict = checker
                .check(format!("https://example.test/{index}").parse().unwrap())
                .await
                .unwrap();
            assert_eq!(verdict.evidence[0].kind, "tier-3-budget-exhausted");
        }
    }
}
