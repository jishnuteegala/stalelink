#[cfg(any(test, feature = "live-browser"))]
use std::path::{Path, PathBuf};
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

    #[cfg(any(test, feature = "live-browser"))]
    const fn profile_name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Brave => "brave",
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }
}

#[cfg(any(test, feature = "live-browser"))]
#[derive(Debug)]
pub enum BrowserLaunch {
    Attach,
    Launch,
}

#[cfg(any(test, feature = "live-browser"))]
pub fn browser_launch(browser: Browser, debug_url: Option<&str>) -> Result<BrowserLaunch, String> {
    if debug_url.is_some() {
        return Ok(BrowserLaunch::Attach);
    }
    if matches!(browser, Browser::Firefox) {
        return Err("Firefox does not support the Chrome DevTools Protocol used for tier-3 checking; use --browser chrome|edge|brave|chromium, or attach via --cdp-url to an endpoint that speaks CDP".into());
    }
    Ok(BrowserLaunch::Launch)
}

#[cfg(any(test, feature = "live-browser"))]
pub fn browser_profile(cache_dir: &Path, browser: Browser) -> PathBuf {
    cache_dir
        .join("browser-profiles")
        .join(browser.profile_name())
}

#[cfg(feature = "live-browser")]
pub fn discover_executable(browser: Browser) -> Result<PathBuf, String> {
    discover_executable_with(browser, |path| path.is_file())
}

#[cfg(any(test, feature = "live-browser"))]
fn discover_executable_with(
    browser: Browser,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    let browsers = match browser {
        Browser::Auto => vec![
            Browser::Chrome,
            Browser::Edge,
            Browser::Brave,
            Browser::Chromium,
        ],
        browser => vec![browser],
    };
    browsers
        .iter()
        .flat_map(|browser| executable_candidates(*browser))
        .find(|path| exists(path))
        .ok_or_else(|| format!("could not find a {} executable", browser.name()))
}

#[cfg(any(test, feature = "live-browser"))]
fn executable_candidates(browser: Browser) -> Vec<PathBuf> {
    let (windows, macos, unix, commands): (&[&str], &[&str], &[&str], &[&str]) = match browser {
        Browser::Chrome => (
            &["Google/Chrome/Application/chrome.exe"],
            &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"],
            &["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable"],
            &["google-chrome", "google-chrome-stable"],
        ),
        Browser::Edge => (
            &["Microsoft/Edge/Application/msedge.exe"],
            &["/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"],
            &["/usr/bin/microsoft-edge", "/usr/bin/microsoft-edge-stable"],
            &["microsoft-edge", "microsoft-edge-stable"],
        ),
        Browser::Brave => (
            &["BraveSoftware/Brave-Browser/Application/brave.exe"],
            &["/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"],
            &["/usr/bin/brave-browser", "/usr/bin/brave"],
            &["brave-browser", "brave"],
        ),
        Browser::Chromium => (
            &["Chromium/Application/chrome.exe"],
            &["/Applications/Chromium.app/Contents/MacOS/Chromium"],
            &["/usr/bin/chromium", "/usr/bin/chromium-browser"],
            &["chromium", "chromium-browser"],
        ),
        Browser::Firefox => (
            &["Mozilla Firefox/firefox.exe"],
            &["/Applications/Firefox.app/Contents/MacOS/firefox"],
            &["/usr/bin/firefox"],
            &["firefox"],
        ),
        Browser::Auto => unreachable!("resolved before looking up executable candidates"),
    };
    let mut paths = Vec::new();
    for root in ["PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(root) = std::env::var_os(root) {
            paths.extend(windows.iter().map(|path| PathBuf::from(&root).join(path)));
        }
    }
    paths.extend(macos.iter().map(PathBuf::from));
    paths.extend(unix.iter().map(PathBuf::from));
    paths.extend(commands.iter().map(PathBuf::from));
    paths
}

#[derive(Clone)]
struct Cookie {
    domain: String,
    host_only: bool,
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
            host_only: !cookie.domain.starts_with('.'),
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

#[allow(dead_code)]
pub enum PageAttempt {
    Clean,
    Verdict(Verdict),
    Unavailable,
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
            domain_matches(host, &cookie.domain, cookie.host_only)
                && path_matches(url.path(), &cookie.path)
                && (!cookie.secure || url.scheme() == "https")
                && cookie.expires.is_none_or(|expires| expires > now)
        })
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

fn domain_matches(host: &str, domain: &str, host_only: bool) -> bool {
    let domain = domain.trim_start_matches('.');
    if host_only {
        return host.eq_ignore_ascii_case(domain);
    }
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
impl<T1: AuthTier, T2: CookieTier, T3: PageTier> Checker for AuthChecker<T1, T2, T3> {
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
            match tier3.attempt_page(url).await {
                PageAttempt::Clean => None,
                PageAttempt::Verdict(verdict) => Some(verdict),
                PageAttempt::Unavailable => second,
            }
        })
    }
}

pub trait CookieTier: Send + Sync {
    fn attempt_cookie(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Attempt> + Send + '_>>;
}
pub trait PageTier: Send + Sync {
    fn attempt_page(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>>;
}

#[cfg(not(feature = "live-browser"))]
pub struct UnavailablePageTier;
#[cfg(not(feature = "live-browser"))]
impl PageTier for UnavailablePageTier {
    fn attempt_page(
        &self,
        _: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>> {
        Box::pin(async { PageAttempt::Unavailable })
    }
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
            ) || (known_fence && matches!(verdict.reason, Reason::HttpStatus(429 | 503)))
        }
        2 => verdict.confidence == Confidence::AuthWalled || bot_fence(verdict),
        _ => false,
    }
}

#[cfg(any(test, feature = "live-browser"))]
pub trait PageDriver: Send + Sync {
    fn check_page(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>>;
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
impl<D: PageDriver> PageTier for BrowserChecker<D> {
    fn attempt_page(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>> {
        Box::pin(async move {
            if self.used.fetch_add(1, Ordering::Relaxed) >= 25 {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    eprintln!("warning: tier-3 browser budget exhausted after 25 links");
                }
                return PageAttempt::Verdict(Verdict {
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
            match self.drivers[slot].check_page(url).await {
                PageAttempt::Verdict(mut verdict) => {
                    verdict.tier = 3;
                    PageAttempt::Verdict(verdict)
                }
                PageAttempt::Clean => PageAttempt::Clean,
                PageAttempt::Unavailable => PageAttempt::Unavailable,
            }
        })
    }
}
#[cfg(any(test, feature = "live-browser"))]
impl<D: PageDriver> Checker for BrowserChecker<D> {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            match self.attempt_page(url).await {
                PageAttempt::Verdict(verdict) => Some(verdict),
                PageAttempt::Clean | PageAttempt::Unavailable => None,
            }
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
        executable: Option<&std::path::Path>,
    ) -> Result<[Self; 4], String> {
        use chromiumoxide::browser::{Browser, BrowserConfig};
        use futures::StreamExt;
        let (browser, mut handler) = if let Some(url) = debug_url {
            Browser::connect(url.to_owned())
                .await
                .map_err(|e| e.to_string())?
        } else {
            let mut config = BrowserConfig::builder().user_data_dir(profile);
            if let Some(executable) = executable {
                config = config.chrome_executable(executable);
            }
            Browser::launch(config.build().map_err(|e| e.to_string())?)
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
    fn check_page(
        &self,
        url: Url,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>> {
        Box::pin(async move {
            let page = self.page.lock().await;
            let navigation = page.wait_for_navigation_response();
            if tokio::time::timeout(Duration::from_secs(30), page.goto(url.as_str()))
                .await
                .ok()
                .and_then(Result::ok)
                .is_none()
            {
                return PageAttempt::Unavailable;
            }
            let status = tokio::time::timeout(Duration::from_secs(30), navigation)
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|request| {
                    request
                        .response
                        .as_ref()
                        .map(|response| response.status as u16)
                });
            let Some(final_url) = page
                .url()
                .await
                .ok()
                .flatten()
                .and_then(|url| url.parse::<Url>().ok())
            else {
                return PageAttempt::Unavailable;
            };
            let Some(content) = page.content().await.ok() else {
                return PageAttempt::Unavailable;
            };
            let content = content.to_ascii_lowercase();
            if let Some(status) = status.filter(|status| !(200..300).contains(status)) {
                return PageAttempt::Verdict(status_verdict(status));
            }
            if login_wall(&[final_url.clone()])
                || content.contains("captcha")
                || content.contains("challenge")
                || (content.contains("sign in")
                    && (content.contains("type=\"password\"")
                        || content.contains("type='password'")
                        || content.contains("name=\"password\"")
                        || content.contains("name='password'")))
            {
                let reason = Reason::LoginWall;
                return PageAttempt::Verdict(Verdict {
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
            PageAttempt::Clean
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
            host_only: true,
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

    #[test]
    fn host_only_cookies_do_not_reach_subdomains() {
        let cookie = Cookie {
            domain: "example.test".into(),
            host_only: true,
            path: "/".into(),
            secure: false,
            expires: None,
            name: "session".into(),
            value: "secret".into(),
        };
        assert!(cookie_header(&[cookie], &"https://sub.example.test/".parse().unwrap()).is_empty());
    }

    struct CleanDriver;
    impl PageDriver for CleanDriver {
        fn check_page(
            &self,
            _: Url,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>> {
            Box::pin(async { PageAttempt::Clean })
        }
    }

    struct FakeTier(Option<Verdict>);
    impl AuthTier for FakeTier {
        fn attempt(&self, _: Url) -> CheckFuture<'_> {
            let verdict = self.0.clone();
            Box::pin(async move { verdict })
        }
    }

    struct FakeCookie(Attempt);
    impl CookieTier for FakeCookie {
        fn attempt_cookie(
            &self,
            _: Url,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Attempt> + Send + '_>> {
            let attempt = match &self.0 {
                Attempt::Verdict(verdict) => Attempt::Verdict(verdict.clone()),
                Attempt::Unavailable => Attempt::Unavailable,
            };
            Box::pin(async move { attempt })
        }
    }

    struct FakePage(PageAttempt);
    impl PageTier for FakePage {
        fn attempt_page(
            &self,
            _: Url,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>> {
            let attempt = match &self.0 {
                PageAttempt::Clean => PageAttempt::Clean,
                PageAttempt::Verdict(verdict) => PageAttempt::Verdict(verdict.clone()),
                PageAttempt::Unavailable => PageAttempt::Unavailable,
            };
            Box::pin(async move { attempt })
        }
    }

    #[derive(Clone, Default)]
    struct TierCalls {
        tier1: std::sync::Arc<AtomicUsize>,
        tier2: std::sync::Arc<AtomicUsize>,
        tier3: std::sync::Arc<AtomicUsize>,
    }

    struct RecordingTier {
        result: Option<Verdict>,
        calls: TierCalls,
    }
    impl AuthTier for RecordingTier {
        fn attempt(&self, _: Url) -> CheckFuture<'_> {
            self.calls.tier1.fetch_add(1, Ordering::Relaxed);
            let result = self.result.clone();
            Box::pin(async move { result })
        }
    }

    struct RecordingCookie {
        result: Attempt,
        calls: TierCalls,
    }
    impl CookieTier for RecordingCookie {
        fn attempt_cookie(
            &self,
            _: Url,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Attempt> + Send + '_>> {
            self.calls.tier2.fetch_add(1, Ordering::Relaxed);
            let result = match &self.result {
                Attempt::Verdict(verdict) => Attempt::Verdict(verdict.clone()),
                Attempt::Unavailable => Attempt::Unavailable,
            };
            Box::pin(async move { result })
        }
    }

    struct RecordingPage {
        result: PageAttempt,
        calls: TierCalls,
    }
    impl PageTier for RecordingPage {
        fn attempt_page(
            &self,
            _: Url,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PageAttempt> + Send + '_>> {
            self.calls.tier3.fetch_add(1, Ordering::Relaxed);
            let result = match &self.result {
                PageAttempt::Clean => PageAttempt::Clean,
                PageAttempt::Verdict(verdict) => PageAttempt::Verdict(verdict.clone()),
                PageAttempt::Unavailable => PageAttempt::Unavailable,
            };
            Box::pin(async move { result })
        }
    }

    fn fake_verdict(reason: Reason) -> Verdict {
        Verdict {
            confidence: reason.confidence(),
            reason,
            evidence: vec![],
            checked_at: Utc::now(),
            tier: 1,
        }
    }

    #[tokio::test]
    async fn auth_decision_table_covers_every_trigger_and_no_trigger() {
        async fn assert_row(
            name: &str,
            url: &str,
            tier1: Option<Verdict>,
            tier2: Attempt,
            expected: (usize, usize, usize),
        ) {
            let calls = TierCalls::default();
            let checker = AuthChecker::new(
                RecordingTier {
                    result: tier1,
                    calls: calls.clone(),
                },
                Some(RecordingCookie {
                    result: tier2,
                    calls: calls.clone(),
                }),
                Some(RecordingPage {
                    result: PageAttempt::Clean,
                    calls: calls.clone(),
                }),
                AuthCap::Browser,
            );
            let _ = checker.check(url.parse().unwrap()).await;
            assert_eq!(
                (
                    calls.tier1.load(Ordering::Relaxed),
                    calls.tier2.load(Ordering::Relaxed),
                    calls.tier3.load(Ordering::Relaxed),
                ),
                expected,
                "{name}"
            );
        }

        assert_row(
            "tier-1 401 triggers tier 2",
            "https://example.test/private",
            Some(fake_verdict(Reason::HttpStatus(401))),
            Attempt::Verdict(None),
            (1, 1, 0),
        )
        .await;
        assert_row(
            "tier-1 403 triggers tier 2",
            "https://example.test/private",
            Some(fake_verdict(Reason::HttpStatus(403))),
            Attempt::Verdict(None),
            (1, 1, 0),
        )
        .await;
        assert_row(
            "tier-1 login wall triggers tier 2",
            "https://example.test/private",
            Some(fake_verdict(Reason::LoginWall)),
            Attempt::Verdict(None),
            (1, 1, 0),
        )
        .await;
        assert_row(
            "known-host 429 triggers tier 2",
            "https://github.com/private",
            Some(fake_verdict(Reason::HttpStatus(429))),
            Attempt::Verdict(None),
            (1, 1, 0),
        )
        .await;
        assert_row(
            "known-host 503 triggers tier 2",
            "https://github.com/private",
            Some(fake_verdict(Reason::HttpStatus(503))),
            Attempt::Verdict(None),
            (1, 1, 0),
        )
        .await;
        assert_row(
            "unknown-host 429 does not trigger",
            "https://example.test/private",
            Some(fake_verdict(Reason::HttpStatus(429))),
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "unknown-host 503 does not trigger",
            "https://example.test/private",
            Some(fake_verdict(Reason::HttpStatus(503))),
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "ordinary 404 does not trigger",
            "https://example.test/private",
            Some(fake_verdict(Reason::HttpStatus(404))),
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "ordinary 200 does not trigger",
            "https://example.test/private",
            None,
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "soft 404 does not trigger",
            "https://example.test/private",
            Some(fake_verdict(Reason::Soft404)),
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "DNS failure does not trigger",
            "https://example.test/private",
            Some(fake_verdict(Reason::NetworkError(
                stalelink_core::model::NetKind::Dns,
            ))),
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "clean tier-1 result does not trigger",
            "https://example.test/private",
            None,
            Attempt::Verdict(None),
            (1, 0, 0),
        )
        .await;
        assert_row(
            "tier-2 ordinary verdict does not trigger tier 3",
            "https://example.test/private",
            Some(fake_verdict(Reason::LoginWall)),
            Attempt::Verdict(Some(fake_verdict(Reason::HttpStatus(404)))),
            (1, 1, 0),
        )
        .await;
        assert_row(
            "tier-2 auth wall triggers tier 3",
            "https://example.test/private",
            Some(fake_verdict(Reason::LoginWall)),
            Attempt::Verdict(Some(fake_verdict(Reason::LoginWall))),
            (1, 1, 1),
        )
        .await;
        let mut challenge = fake_verdict(Reason::HttpStatus(403));
        challenge.evidence.push(Evidence {
            kind: "body".into(),
            detail: "bot challenge".into(),
        });
        assert_row(
            "tier-2 bot challenge triggers tier 3",
            "https://example.test/private",
            Some(fake_verdict(Reason::LoginWall)),
            Attempt::Verdict(Some(challenge)),
            (1, 1, 1),
        )
        .await;
    }

    #[tokio::test]
    async fn clean_browser_page_clears_cookie_auth_wall_but_unavailable_preserves_it() {
        let wall = fake_verdict(Reason::LoginWall);
        let clean = AuthChecker::new(
            FakeTier(Some(wall.clone())),
            Some(FakeCookie(Attempt::Verdict(Some(wall.clone())))),
            Some(FakePage(PageAttempt::Clean)),
            AuthCap::Browser,
        );
        assert!(
            clean
                .check("https://example.test/private".parse().unwrap())
                .await
                .is_none()
        );
        let unavailable = AuthChecker::new(
            FakeTier(Some(wall.clone())),
            Some(FakeCookie(Attempt::Verdict(Some(wall.clone())))),
            Some(FakePage(PageAttempt::Unavailable)),
            AuthCap::Browser,
        );
        assert_eq!(
            unavailable
                .check("https://example.test/private".parse().unwrap())
                .await
                .unwrap()
                .reason,
            Reason::LoginWall
        );
    }

    #[test]
    fn browser_discovery_and_profiles_are_browser_specific() {
        let found = discover_executable_with(Browser::Edge, |path| {
            path.ends_with("Microsoft/Edge/Application/msedge.exe")
        })
        .unwrap();
        assert!(found.ends_with("Microsoft/Edge/Application/msedge.exe"));
        let cache = Path::new("cache");
        assert_ne!(
            browser_profile(cache, Browser::Chrome),
            browser_profile(cache, Browser::Brave)
        );
    }

    #[test]
    fn firefox_launch_is_refused_but_cdp_attach_is_browser_agnostic() {
        let error = browser_launch(Browser::Firefox, None).unwrap_err();
        assert!(error.contains("Firefox"));
        assert!(error.contains("does not support the Chrome DevTools Protocol"));
        assert!(error.contains("--browser chrome|edge|brave|chromium"));
        assert!(error.contains("--cdp-url"));
        assert!(matches!(
            browser_launch(Browser::Firefox, Some("http://fake-cdp.test:9222")),
            Ok(BrowserLaunch::Attach)
        ));
        assert!(matches!(
            browser_launch(Browser::Chrome, Some("http://fake-cdp.test:9222")),
            Ok(BrowserLaunch::Attach)
        ));
    }

    #[tokio::test]
    async fn browser_budget_reports_every_overflow() {
        if std::env::var_os("STALELINK_CAPTURE_BUDGET_WARNING").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "auth::tests::browser_budget_reports_every_overflow",
                    "--nocapture",
                ])
                .env("STALELINK_CAPTURE_BUDGET_WARNING", "1")
                .output()
                .unwrap();
            assert!(output.status.success());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(stderr.matches("tier-3 browser budget exhausted").count(), 1);
            return;
        }
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
