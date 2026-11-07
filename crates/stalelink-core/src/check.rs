use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use httpdate::parse_http_date;
use tokio::sync::{Mutex, OnceCell, Semaphore};
use url::Url;

use crate::model::{Evidence, NetKind, Reason, Verdict};

const SOFT_404_SIMILARITY: f64 = 0.9;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_PROBE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const RETRY_WAIT_TIME: Duration = Duration::from_secs(1);
// Retry waits are capped so untrusted response headers and large retry counts
// cannot indefinitely stall a scan or overflow Tokio's timer deadline.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const FAR_PAST: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 2);
const STALENESS_PHRASES: &[&str] = &[
    "this page is deprecated",
    "this page has been archived",
    "this documentation is archived",
    "has been superseded",
];

pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = Option<Verdict>> + Send + 'a>>;
pub trait Checker: Send + Sync {
    fn check(&self, url: Url) -> CheckFuture<'_>;
}

#[derive(Clone)]
enum SiblingProbe {
    Body(Url, Arc<str>),
    NoBody,
    Transient,
}

#[derive(Default)]
struct SiblingProbes {
    cells: HashMap<String, Arc<OnceCell<SiblingProbe>>>,
    body_bytes: usize,
}
impl SiblingProbes {
    fn cell(&mut self, directory: &str) -> Arc<OnceCell<SiblingProbe>> {
        self.cells.entry(directory.to_owned()).or_default().clone()
    }
    // Only a conclusive result within the byte budget stays for later links;
    // links already waiting on a dropped cell still see its result.
    fn settle(&mut self, directory: &str, result: &SiblingProbe) {
        match result {
            SiblingProbe::Body(_, body)
                if self.body_bytes + body.len() <= MAX_PROBE_CACHE_BYTES =>
            {
                self.body_bytes += body.len();
            }
            SiblingProbe::NoBody => {}
            SiblingProbe::Body(..) | SiblingProbe::Transient => {
                self.cells.remove(directory);
            }
        }
    }
}

pub struct HttpChecker {
    raw: reqwest::Client,
    retries: u8,
    retry_wait_time: Duration,
    per_host: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    // A sync lock leaves no await between caching a probe and accounting for
    // it, so a cancelled check cannot skip the byte budget.
    siblings: std::sync::Mutex<SiblingProbes>,
}
impl HttpChecker {
    pub fn new(
        timeout: Duration,
        retries: u8,
        per_host: usize,
        user_agent: String,
    ) -> Result<Self, reqwest::Error> {
        Self::with_retry_wait_time(timeout, retries, per_host, user_agent, RETRY_WAIT_TIME)
    }
    fn with_retry_wait_time(
        timeout: Duration,
        retries: u8,
        per_host: usize,
        user_agent: String,
        retry_wait_time: Duration,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            raw: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(user_agent)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            retries,
            retry_wait_time,
            per_host,
            hosts: Mutex::new(HashMap::new()),
            siblings: std::sync::Mutex::new(SiblingProbes::default()),
        })
    }
    async fn host_permit(&self, url: &Url) -> tokio::sync::OwnedSemaphorePermit {
        let host = host_key(url);
        // The checker is scan-scoped, so retaining host entries avoids churn during a scan.
        let semaphore = self
            .hosts
            .lock()
            .await
            .entry(host)
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_host)))
            .clone();
        semaphore
            .acquire_owned()
            .await
            .expect("per-host semaphore is never closed")
    }
    async fn walk(&self, method: reqwest::Method, url: Url) -> Result<Walk, reqwest::Error> {
        let origin = url.clone();
        let mut current = url;
        let mut redirects = Vec::new();
        let mut visited = HashSet::from([current.clone()]);
        loop {
            let (response, permit) = self.request(method.clone(), &current).await?;
            let next = response
                .status()
                .is_redirection()
                .then(|| response.headers().get(reqwest::header::LOCATION))
                .flatten()
                .and_then(|location| location.to_str().ok())
                .and_then(|location| current.join(location).ok());
            let unterminated = match next {
                None => None,
                Some(next) => {
                    redirects.push((response.status(), next.clone()));
                    if redirects.len() > MAX_REDIRECTS {
                        Some("redirect limit exhausted")
                    } else if !visited.insert(next.clone()) {
                        Some("redirect loop detected")
                    } else {
                        drop(response);
                        drop(permit);
                        current = next;
                        continue;
                    }
                }
            };
            return Ok(Walk {
                response,
                permit,
                origin,
                url: current,
                redirects,
                unterminated,
            });
        }
    }
    async fn status(
        &self,
        method: reqwest::Method,
        url: Url,
    ) -> Result<StatusResult, reqwest::Error> {
        Ok(self.walk(method, url).await?.status())
    }
    async fn raw_get(&self, url: Url) -> Result<RawResponse, reqwest::Error> {
        self.walk(reqwest::Method::GET, url).await?.read().await
    }
    async fn request(
        &self,
        method: reqwest::Method,
        url: &Url,
    ) -> Result<(reqwest::Response, tokio::sync::OwnedSemaphorePermit), reqwest::Error> {
        for attempt in 0..=self.retries {
            let permit = self.host_permit(url).await;
            match self.raw.request(method.clone(), url.clone()).send().await {
                Ok(response) if attempt < self.retries && transient_status(response.status()) => {
                    let delay = retry_delay(&response, attempt, self.retry_wait_time);
                    drop(response);
                    drop(permit);
                    tokio::time::sleep(delay).await;
                }
                Ok(response) => return Ok((response, permit)),
                Err(error) if attempt < self.retries && retryable_request_error(&error) => {
                    drop(permit);
                    tokio::time::sleep(retry_backoff(attempt, self.retry_wait_time)).await;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("the retry loop always returns")
    }
    fn siblings(&self) -> std::sync::MutexGuard<'_, SiblingProbes> {
        self.siblings
            .lock()
            .expect("sibling probe lock is never poisoned")
    }
    async fn probe_sibling(&self, probe: Url) -> SiblingProbe {
        match self.raw_get(probe.clone()).await {
            Ok(response) if !response.terminated => SiblingProbe::Transient,
            Ok(response) if response.status.is_success() => {
                SiblingProbe::Body(probe, response.body.into())
            }
            Ok(response) if !transient_status(response.status) => SiblingProbe::NoBody,
            _ => SiblingProbe::Transient,
        }
    }
    // Every probe requests a random missing path, so one conclusive probe
    // answers for every link in its directory.
    async fn sibling(&self, url: &Url) -> Option<(Url, Arc<str>)> {
        let probe = random_sibling(url);
        let directory = probe
            .as_str()
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory)
            .to_owned();
        let cell = self.siblings().cell(&directory);
        let mut initialized = false;
        let mut result = cell
            .get_or_init(|| {
                initialized = true;
                self.probe_sibling(probe.clone())
            })
            .await
            .clone();
        if initialized {
            self.siblings().settle(&directory, &result);
        } else if matches!(result, SiblingProbe::Transient) {
            result = self.probe_sibling(probe).await;
        }
        match result {
            SiblingProbe::Body(probe, body) => Some((probe, body)),
            SiblingProbe::NoBody | SiblingProbe::Transient => None,
        }
    }
    async fn heuristic(&self, response: RawResponse) -> Option<Verdict> {
        if !response.terminated {
            return None;
        }
        if login_wall(&response) {
            return Some(verdict(
                Reason::LoginWall,
                "redirect-chain".into(),
                std::iter::once(response.origin.as_str())
                    .chain(response.redirects.iter().map(|(_, target)| target.as_str()))
                    .collect::<Vec<_>>()
                    .join(" -> "),
            ));
        }
        if response.status.is_success()
            && response.redirects.iter().all(|(status, _)| {
                matches!(
                    *status,
                    reqwest::StatusCode::MOVED_PERMANENTLY
                        | reqwest::StatusCode::PERMANENT_REDIRECT
                )
            })
            && !response.redirects.is_empty()
        {
            return Some(verdict(
                Reason::PermanentRedirect,
                "redirect-target".into(),
                response.url.to_string(),
            ));
        }
        if !response.status.is_success() {
            return None;
        }
        if let Some(last_modified) = response.headers.get(reqwest::header::LAST_MODIFIED)
            && let Ok(last_modified) = last_modified.to_str()
            && let Ok(last_modified) = DateTime::parse_from_rfc2822(last_modified)
            && SystemTime::now()
                .duration_since(last_modified.with_timezone(&Utc).into())
                .is_ok_and(|age| age > FAR_PAST)
        {
            return Some(verdict(
                Reason::FarPastLastModified,
                "last-modified".into(),
                last_modified.to_rfc2822(),
            ));
        }
        let lower = response.body.to_ascii_lowercase();
        if let Some(phrase) = STALENESS_PHRASES
            .iter()
            .find(|phrase| lower.contains(**phrase))
        {
            return Some(verdict(
                Reason::StalenessBanner,
                "staleness-phrase".into(),
                (*phrase).into(),
            ));
        }
        if let Some(upgrade) = version_upgrade(&response.origin)
            && self
                .raw_get(upgrade.clone())
                .await
                .is_ok_and(|response| response.status.is_success())
        {
            return Some(verdict(
                Reason::VersionDrift,
                "version-upgrade".into(),
                upgrade.to_string(),
            ));
        }
        if let Some((sibling, sibling_body)) = self.sibling(&response.origin).await
            && similarity(&response.body, &sibling_body) >= SOFT_404_SIMILARITY
        {
            return Some(verdict(
                Reason::Soft404,
                "sibling-similarity".into(),
                format!(
                    "{} ({:.2})",
                    sibling,
                    similarity(&response.body, &sibling_body)
                ),
            ));
        }
        None
    }
}
impl Checker for HttpChecker {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            let finding = outcome(self.status(reqwest::Method::HEAD, url.clone()).await);
            // Retry with GET when the server signals HEAD is unsupported or
            // disallowed: 403 Forbidden, 405 Method Not Allowed, 501 Not
            // Implemented. A subsequent GET may well return 200.
            let response = if matches!(
                finding.as_ref().map(|verdict| &verdict.reason),
                Some(Reason::HttpStatus(403 | 405 | 501))
            ) {
                // Heuristics need exactly this GET walk from the link, so its
                // body is read instead of requesting the chain a second time.
                let walk = match self.walk(reqwest::Method::GET, url).await {
                    Ok(walk) => walk,
                    Err(error) => return outcome(Err(error)),
                };
                if let Some(fallback) = outcome(Ok(walk.status())) {
                    return Some(fallback);
                }
                walk.read().await
            } else if finding.is_some() {
                return finding;
            } else {
                self.raw_get(url).await
            };
            self.heuristic(response.ok()?).await
        })
    }
}
fn outcome(status: Result<StatusResult, reqwest::Error>) -> Option<Verdict> {
    let (reason, detail) = match status {
        Ok(StatusResult::Terminal(status)) if status.is_success() => return None,
        Ok(StatusResult::Terminal(status)) => {
            (Reason::HttpStatus(status.as_u16()), status.to_string())
        }
        Ok(StatusResult::Unterminated(detail)) => {
            return Some(verdict(
                Reason::NetworkError(NetKind::Other),
                "redirect-traversal".into(),
                detail.into(),
            ));
        }
        Err(error) => (
            Reason::NetworkError(network_kind(&error)),
            error.to_string(),
        ),
    };
    Some(verdict(reason, "status".into(), detail))
}
fn verdict(reason: Reason, kind: String, detail: String) -> Verdict {
    Verdict {
        confidence: reason.confidence(),
        reason,
        evidence: vec![Evidence { kind, detail }],
        checked_at: Utc::now(),
        tier: 1,
    }
}
fn network_kind(error: &reqwest::Error) -> NetKind {
    if error.is_timeout() {
        return NetKind::Timeout;
    }
    // Prefer a typed io::Error in the source chain: it distinguishes a genuine
    // connection refusal from other connect-phase failures without relying on
    // platform-specific message wording.
    if let Some(io_error) = io_error_in_chain(error)
        && io_error.kind() == std::io::ErrorKind::ConnectionRefused
    {
        return NetKind::ConnRefused;
    }
    // TLS and resolver failures are not surfaced as typed values by reqwest, so
    // fall back to inspecting the lowercased error chain for their wording.
    let chain = error_chain(error);
    if chain.contains("certificate")
        || chain.contains("tls")
        || chain.contains("ssl")
        || chain.contains("handshake")
    {
        NetKind::Tls
    } else if chain.contains("dns")
        || chain.contains("failed to lookup")
        || chain.contains("name resolution")
        || chain.contains("name or service not known")
        || chain.contains("no such host")
    {
        NetKind::Dns
    } else {
        NetKind::Other
    }
}
fn transient_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            reqwest::StatusCode::REQUEST_TIMEOUT | reqwest::StatusCode::TOO_MANY_REQUESTS
        )
}
fn retryable_request_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || (error.is_request()
            && (hyper_error_in_chain(error)
                .is_some_and(|error| error.is_incomplete_message() || error.is_canceled())
                || io_error_in_chain(error).is_some_and(retryable_io_error)))
}
fn retryable_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::TimedOut
    )
}
fn hyper_error_in_chain(error: &reqwest::Error) -> Option<&hyper::Error> {
    use std::error::Error as _;
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.source();
    while let Some(current) = source {
        if let Some(hyper_error) = current.downcast_ref::<hyper::Error>() {
            return Some(hyper_error);
        }
        source = current.source();
    }
    None
}
fn retry_delay(response: &reqwest::Response, attempt: u8, base: Duration) -> Duration {
    let backoff = retry_backoff(attempt, base);
    if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return backoff;
    }
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| retry_after_delay(value, SystemTime::now()))
        .map_or(backoff, |retry_after| retry_after.max(backoff))
}

fn retry_after_delay(value: &str, now: SystemTime) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| parse_http_date(value).ok()?.duration_since(now).ok())
        .map(|delay| delay.min(MAX_RETRY_DELAY))
}

fn retry_backoff(attempt: u8, base: Duration) -> Duration {
    let mut delay = base.min(MAX_RETRY_DELAY);
    for _ in 0..attempt {
        delay = delay.checked_mul(2).unwrap_or(MAX_RETRY_DELAY);
        if delay >= MAX_RETRY_DELAY {
            return MAX_RETRY_DELAY;
        }
    }
    delay
}
fn host_key(url: &Url) -> String {
    match (url.host_str(), url.port_or_known_default()) {
        (Some(host), Some(port)) => format!("{}:{port}", host.to_ascii_lowercase()),
        (Some(host), None) => host.to_ascii_lowercase(),
        (None, _) => String::new(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StatusResult {
    Terminal(reqwest::StatusCode),
    Unterminated(&'static str),
}
fn io_error_in_chain(error: &reqwest::Error) -> Option<&std::io::Error> {
    use std::error::Error as _;
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.source();
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            return Some(io_error);
        }
        source = current.source();
    }
    None
}
fn error_chain(error: &reqwest::Error) -> String {
    let mut message = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        message.push_str(&current.to_string().to_ascii_lowercase());
        message.push(' ');
        source = current.source();
    }
    message
}

struct RawResponse {
    status: reqwest::StatusCode,
    origin: Url,
    url: Url,
    redirects: Vec<(reqwest::StatusCode, Url)>,
    headers: reqwest::header::HeaderMap,
    body: String,
    terminated: bool,
}

// A walk ends at the first response that is not a followable redirect, or at
// the redirect where the chain was abandoned (`unterminated`: limit or loop).
struct Walk {
    response: reqwest::Response,
    permit: tokio::sync::OwnedSemaphorePermit,
    origin: Url,
    url: Url,
    redirects: Vec<(reqwest::StatusCode, Url)>,
    unterminated: Option<&'static str>,
}
impl Walk {
    fn status(&self) -> StatusResult {
        match self.unterminated {
            Some(reason) => StatusResult::Unterminated(reason),
            None => StatusResult::Terminal(self.response.status()),
        }
    }
    async fn read(self) -> Result<RawResponse, reqwest::Error> {
        let status = self.response.status();
        let headers = self.response.headers().clone();
        let terminated = !status.is_redirection();
        let body = if terminated {
            read_body(self.response, self.permit).await?
        } else {
            String::new()
        };
        Ok(RawResponse {
            status,
            origin: self.origin,
            url: self.url,
            redirects: self.redirects,
            headers,
            body,
            terminated,
        })
    }
}

async fn read_body(
    mut response: reqwest::Response,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<String, reqwest::Error> {
    let mut body = Vec::with_capacity(MAX_BODY_BYTES);
    while let Some(chunk) = response.chunk().await? {
        let remaining = MAX_BODY_BYTES.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == MAX_BODY_BYTES {
            break;
        }
    }
    drop(response);
    drop(permit);
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn login_wall(response: &RawResponse) -> bool {
    if response.redirects.is_empty() {
        return false;
    }
    let has_auth_component = response
        .redirects
        .iter()
        .map(|(_, target)| target)
        .any(auth_component);
    let has_return_url = response
        .redirects
        .iter()
        .map(|(_, target)| target)
        .any(|target| {
            target
                .query_pairs()
                .any(|(key, _)| key.eq_ignore_ascii_case("returnurl"))
        });
    has_auth_component || has_return_url
}

fn auth_component(target: &Url) -> bool {
    let path_matches = target.path_segments().is_some_and(|mut segments| {
        segments.any(|segment| matches!(segment.to_ascii_lowercase().as_str(), "login" | "signin"))
    });
    let host = target.host_str().unwrap_or_default().to_ascii_lowercase();
    let host_matches = host
        .split('.')
        .any(|label| matches!(label, "auth" | "sso" | "login"));
    path_matches || host_matches
}

fn version_upgrade(url: &Url) -> Option<Url> {
    let mut upgraded = url.clone();
    let segments = url.path_segments()?.collect::<Vec<_>>();
    let mut replacement = None;
    for (index, segment) in segments.iter().enumerate() {
        if let Some(version) = segment.strip_prefix('v')
            && let Ok(version) = version.parse::<u32>()
            && let Some(next) = version.checked_add(1)
        {
            replacement = Some((index, format!("v{next}")));
            break;
        }
    }
    let (index, replacement) = replacement?;
    let path = segments
        .iter()
        .enumerate()
        .map(|(segment_index, segment)| {
            if segment_index == index {
                replacement.as_str()
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    upgraded.set_path(&format!("/{path}"));
    Some(upgraded)
}

fn random_sibling(url: &Url) -> Url {
    let mut sibling = url.clone();
    let path = url.path().rsplit_once('/').map_or("", |(parent, _)| parent);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    sibling.set_path(&format!("{path}/stalelink-probe-{nonce:x}"));
    sibling.set_query(None);
    sibling.set_fragment(None);
    sibling
}

fn similarity(left: &str, right: &str) -> f64 {
    let left = normalized_words(left);
    let right = normalized_words(right);
    if left.len() < 3 || right.len() < 3 {
        return 0.0;
    }
    let common = left.intersection(&right).count();
    common as f64 / left.union(&right).count() as f64
}

fn normalized_words(value: &str) -> std::collections::BTreeSet<String> {
    value
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use crate::model::Confidence;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, path_regex},
    };

    use super::*;

    async fn checker() -> HttpChecker {
        HttpChecker::new(Duration::from_secs(2), 0, 2, "stalelink-test".into()).unwrap()
    }

    async fn retrying_checker(retries: u8) -> HttpChecker {
        HttpChecker::with_retry_wait_time(
            Duration::from_secs(2),
            retries,
            2,
            "stalelink-test".into(),
            Duration::from_millis(1),
        )
        .unwrap()
    }

    async fn mount(server: &MockServer, route: &str, response: ResponseTemplate) {
        Mock::given(path(route))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn mount_method(
        server: &MockServer,
        verb: &str,
        route: &str,
        response: ResponseTemplate,
    ) {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn redirect(status: u16, location: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).insert_header("location", location)
    }

    async fn check_route(server: &MockServer, route: &str) -> Option<Verdict> {
        checker()
            .await
            .check(format!("{}{route}", server.uri()).parse().unwrap())
            .await
    }

    #[tokio::test]
    async fn detects_soft_404_from_a_matching_sibling() {
        let server = MockServer::start().await;
        Mock::given(path_regex("/docs/stalelink-probe-.*"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("Not found: choose another document"),
            )
            .mount(&server)
            .await;
        mount(
            &server,
            "/docs/missing",
            ResponseTemplate::new(200).set_body_string("Not found: choose another document"),
        )
        .await;
        let verdict = checker()
            .await
            .check(format!("{}/docs/missing", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::Soft404);
        assert_eq!(verdict.confidence, Confidence::LikelyDead);
        assert_eq!(verdict.evidence[0].kind, "sibling-similarity");
        assert!(
            verdict.evidence[0]
                .detail
                .starts_with(&format!("{}/docs/", server.uri()))
        );
        assert!(verdict.evidence[0].detail.ends_with("(1.00)"));
    }

    #[tokio::test]
    async fn probes_each_directory_once_per_checker() {
        let server = MockServer::start().await;
        Mock::given(path_regex("/docs/stalelink-probe-.*"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path_regex("/blog/stalelink-probe-.*"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        for route in ["/docs/a", "/docs/b", "/blog/c"] {
            mount(
                &server,
                route,
                ResponseTemplate::new(200).set_body_string(format!("Current page {route}")),
            )
            .await;
        }
        let checker = checker().await;
        for route in ["/docs/a", "/docs/b", "/blog/c"] {
            assert!(
                checker
                    .check(format!("{}{route}", server.uri()).parse().unwrap())
                    .await
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn shares_one_in_flight_successful_probe() {
        let server = MockServer::start().await;
        let soft = "Not found: choose another document";
        Mock::given(path_regex("/docs/stalelink-probe-.*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(soft)
                    .set_delay(Duration::from_millis(200)),
            )
            .expect(1)
            .mount(&server)
            .await;
        for route in ["/docs/a", "/docs/b", "/docs/c"] {
            mount(
                &server,
                route,
                ResponseTemplate::new(200).set_body_string(soft),
            )
            .await;
        }
        let checker = checker().await;
        let check =
            |route: &str| checker.check(format!("{}{route}", server.uri()).parse().unwrap());
        let (a, b, c) = tokio::join!(check("/docs/a"), check("/docs/b"), check("/docs/c"));
        for verdict in [a, b, c] {
            assert_eq!(verdict.unwrap().reason, Reason::Soft404);
        }
    }

    #[tokio::test]
    async fn retries_a_probe_after_a_transient_failure() {
        let server = MockServer::start().await;
        let soft = "Not found: choose another document";
        let probes = Arc::new(AtomicUsize::new(0));
        let probes_for_responder = probes.clone();
        Mock::given(path_regex("/docs/stalelink-probe-.*"))
            .respond_with(move |_: &wiremock::Request| {
                if probes_for_responder.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(200).set_body_string(soft)
                }
            })
            .mount(&server)
            .await;
        for route in ["/docs/a", "/docs/b", "/docs/c"] {
            mount(
                &server,
                route,
                ResponseTemplate::new(200).set_body_string(soft),
            )
            .await;
        }
        let checker = checker().await;
        let check =
            |route: &str| checker.check(format!("{}{route}", server.uri()).parse().unwrap());
        assert!(check("/docs/a").await.is_none());
        assert_eq!(check("/docs/b").await.unwrap().reason, Reason::Soft404);
        assert_eq!(check("/docs/c").await.unwrap().reason, Reason::Soft404);
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_a_probe_after_an_unterminated_redirect() {
        let server = MockServer::start().await;
        let soft = "Not found: choose another document";
        let probes = Arc::new(AtomicUsize::new(0));
        let probes_for_responder = probes.clone();
        Mock::given(path_regex("/docs/stalelink-probe-.*"))
            .respond_with(move |_: &wiremock::Request| {
                if probes_for_responder.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(301)
                } else {
                    ResponseTemplate::new(200).set_body_string(soft)
                }
            })
            .mount(&server)
            .await;
        for route in ["/docs/a", "/docs/b"] {
            mount(
                &server,
                route,
                ResponseTemplate::new(200).set_body_string(soft),
            )
            .await;
        }
        let checker = checker().await;
        let check =
            |route: &str| checker.check(format!("{}{route}", server.uri()).parse().unwrap());
        assert!(check("/docs/a").await.is_none());
        assert_eq!(check("/docs/b").await.unwrap().reason, Reason::Soft404);
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn links_waiting_on_a_failed_probe_retry_it_concurrently() {
        let server = MockServer::start().await;
        let delay = Duration::from_secs(1);
        Mock::given(path_regex("/docs/stalelink-probe-.*"))
            .respond_with(ResponseTemplate::new(503).set_delay(delay))
            .expect(3)
            .mount(&server)
            .await;
        for route in ["/docs/a", "/docs/b", "/docs/c"] {
            mount(&server, route, ResponseTemplate::new(200)).await;
        }
        let checker =
            HttpChecker::new(Duration::from_secs(2), 0, 4, "stalelink-test".into()).unwrap();
        let check =
            |route: &str| checker.check(format!("{}{route}", server.uri()).parse().unwrap());
        let started = std::time::Instant::now();
        let (a, b, c) = tokio::join!(check("/docs/a"), check("/docs/b"), check("/docs/c"));
        assert!(a.is_none() && b.is_none() && c.is_none());
        // One shared failed probe, then the two waiting links probe in parallel,
        // rather than three probes one after another.
        assert!(started.elapsed() < delay * 5 / 2);
    }

    #[tokio::test]
    async fn bounds_retained_probe_bodies_across_directories() {
        let server = MockServer::start().await;
        let probes = Arc::new(AtomicUsize::new(0));
        let probes_for_responder = probes.clone();
        Mock::given(path_regex("/dir-[0-9]+/stalelink-probe-.*"))
            .respond_with(move |_: &wiremock::Request| {
                probes_for_responder.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string("x".repeat(MAX_BODY_BYTES))
            })
            .mount(&server)
            .await;
        Mock::given(path_regex("/dir-[0-9]+/page-[ab]"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Current page"))
            .mount(&server)
            .await;
        let retained = MAX_PROBE_CACHE_BYTES / MAX_BODY_BYTES;
        let overflow = 8;
        let directories = retained + overflow;
        let checker = checker().await;
        for page in ["a", "b"] {
            for directory in 0..directories {
                let url = format!("{}/dir-{directory}/page-{page}", server.uri());
                assert!(checker.check(url.parse().unwrap()).await.is_none());
            }
        }
        let siblings = checker.siblings.lock().unwrap();
        assert_eq!(siblings.body_bytes, retained * MAX_BODY_BYTES);
        assert_eq!(probes.load(Ordering::SeqCst), directories + overflow);
    }

    #[tokio::test]
    async fn detects_login_redirect_chain() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/private",
            ResponseTemplate::new(302).insert_header("location", "/login?returnUrl=%2Fprivate"),
        )
        .await;
        mount(
            &server,
            "/login",
            ResponseTemplate::new(200).set_body_string("Sign in"),
        )
        .await;
        let verdict = checker()
            .await
            .check(format!("{}/private", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::LoginWall);
        assert_eq!(verdict.confidence, Confidence::AuthWalled);
        assert_eq!(verdict.evidence[0].kind, "redirect-chain");
        assert_eq!(
            verdict.evidence[0].detail,
            format!(
                "{}/private -> {}/login?returnUrl=%2Fprivate",
                server.uri(),
                server.uri(),
            )
        );
    }

    #[tokio::test]
    async fn detects_permanent_redirect_target() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/old",
            ResponseTemplate::new(301).insert_header("location", "/new"),
        )
        .await;
        mount(
            &server,
            "/new",
            ResponseTemplate::new(200).set_body_string("Current page"),
        )
        .await;
        let verdict = checker()
            .await
            .check(format!("{}/old", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::PermanentRedirect);
        assert_eq!(verdict.confidence, Confidence::Outdated);
        assert_eq!(verdict.evidence[0].kind, "redirect-target");
        assert_eq!(verdict.evidence[0].detail, format!("{}/new", server.uri()));
    }

    #[tokio::test]
    async fn detects_staleness_banner() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/old-docs",
            ResponseTemplate::new(200)
                .set_body_string("This page is deprecated. Read current documentation."),
        )
        .await;
        let verdict = checker()
            .await
            .check(format!("{}/old-docs", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::StalenessBanner);
        assert_eq!(verdict.confidence, Confidence::Outdated);
        assert_eq!(verdict.evidence[0].kind, "staleness-phrase");
    }

    #[tokio::test]
    async fn detects_version_drift() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/api/v1/users",
            ResponseTemplate::new(200).set_body_string("v1 users"),
        )
        .await;
        mount(
            &server,
            "/api/v2/users",
            ResponseTemplate::new(200).set_body_string("v2 users"),
        )
        .await;
        let verdict = checker()
            .await
            .check(format!("{}/api/v1/users", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::VersionDrift);
        assert_eq!(verdict.confidence, Confidence::Outdated);
        assert_eq!(verdict.evidence[0].kind, "version-upgrade");
        assert_eq!(
            verdict.evidence[0].detail,
            format!("{}/api/v2/users", server.uri())
        );
    }

    #[tokio::test]
    async fn detects_far_past_last_modified() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/old",
            ResponseTemplate::new(200)
                .insert_header("last-modified", "Wed, 01 Jan 2020 00:00:00 GMT")
                .set_body_string("Old content"),
        )
        .await;
        let verdict = checker()
            .await
            .check(format!("{}/old", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::FarPastLastModified);
        assert_eq!(verdict.confidence, Confidence::Suspect);
        assert_eq!(verdict.evidence[0].kind, "last-modified");
    }

    #[tokio::test]
    async fn plain_200_is_clean() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/clean",
            ResponseTemplate::new(200).set_body_string("Welcome to the current documentation."),
        )
        .await;
        Mock::given(path_regex("/stalelink-probe-.*"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(
            checker()
                .await
                .check(format!("{}/clean", server.uri()).parse().unwrap())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn runs_heuristics_after_a_clean_get_fallback() {
        let server = MockServer::start().await;
        mount_method(&server, "HEAD", "/deprecated", ResponseTemplate::new(405)).await;
        Mock::given(method("GET"))
            .and(path("/deprecated"))
            .respond_with(ResponseTemplate::new(200).set_body_string("This page is deprecated."))
            .expect(1)
            .mount(&server)
            .await;
        let verdict = check_route(&server, "/deprecated").await.unwrap();
        assert_eq!(verdict.reason, Reason::StalenessBanner);
    }

    #[tokio::test]
    async fn get_fallback_failures_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;
        mount_method(&server, "GET", "/gone", ResponseTemplate::new(404)).await;
        mount_method(&server, "GET", "/loop", redirect(302, "/loop")).await;
        let gone = check_route(&server, "/gone").await.unwrap();
        assert_eq!(gone.reason, Reason::HttpStatus(404));
        let looped = check_route(&server, "/loop").await.unwrap();
        assert_eq!(looped.reason, Reason::NetworkError(NetKind::Other));
        assert_eq!(looped.evidence[0].detail, "redirect loop detected");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn base_redirects_share_the_destination_host_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = format!("http://{}", listener.local_addr().unwrap());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let active = active.clone();
            let max_active = max_active.clone();
            async move {
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let active = active.clone();
                    let max_active = max_active.clone();
                    tokio::spawn(async move {
                        let mut request = [0; 1024];
                        assert!(stream.read(&mut request).await.unwrap() > 0);
                        let in_flight = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(in_flight, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap();
                    });
                }
            }
        });
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        for server in [&first, &second] {
            Mock::given(method("HEAD"))
                .respond_with(
                    ResponseTemplate::new(302)
                        .insert_header("location", format!("{destination}/target")),
                )
                .mount(server)
                .await;
        }
        let checker =
            HttpChecker::new(Duration::from_secs(2), 0, 1, "stalelink-test".into()).unwrap();
        let (first_status, second_status) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                checker.status(
                    reqwest::Method::HEAD,
                    format!("{}/start", first.uri()).parse().unwrap()
                ),
                checker.status(
                    reqwest::Method::HEAD,
                    format!("{}/start", second.uri()).parse().unwrap()
                ),
            )
        })
        .await
        .expect("redirect checks should not hang");
        assert_eq!(
            first_status.unwrap(),
            StatusResult::Terminal(reqwest::StatusCode::OK)
        );
        assert_eq!(
            second_status.unwrap(),
            StatusResult::Terminal(reqwest::StatusCode::OK)
        );
        server.await.unwrap();
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ignores_unterminated_redirect_chains() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/loop",
            ResponseTemplate::new(301).insert_header("location", "/login"),
        )
        .await;
        mount(
            &server,
            "/login",
            ResponseTemplate::new(301).insert_header("location", "/login"),
        )
        .await;
        mount(&server, "/missing", ResponseTemplate::new(301)).await;
        mount(
            &server,
            "/invalid",
            ResponseTemplate::new(301).insert_header("location", "http://[::1"),
        )
        .await;
        mount(
            &server,
            "/one",
            ResponseTemplate::new(301).insert_header("location", "/two"),
        )
        .await;
        mount(
            &server,
            "/two",
            ResponseTemplate::new(301).insert_header("location", "/three"),
        )
        .await;
        mount(
            &server,
            "/three",
            ResponseTemplate::new(301).insert_header("location", "/four"),
        )
        .await;
        mount(
            &server,
            "/four",
            ResponseTemplate::new(301).insert_header("location", "/five"),
        )
        .await;
        mount(
            &server,
            "/five",
            ResponseTemplate::new(301).insert_header("location", "/six"),
        )
        .await;
        mount(
            &server,
            "/six",
            ResponseTemplate::new(301).insert_header("location", "/seven"),
        )
        .await;
        mount(
            &server,
            "/seven",
            ResponseTemplate::new(301).insert_header("location", "/eight"),
        )
        .await;
        mount(
            &server,
            "/eight",
            ResponseTemplate::new(301).insert_header("location", "/nine"),
        )
        .await;
        mount(
            &server,
            "/nine",
            ResponseTemplate::new(301).insert_header("location", "/ten"),
        )
        .await;
        mount(
            &server,
            "/ten",
            ResponseTemplate::new(301).insert_header("location", "/login"),
        )
        .await;
        for route in ["/loop", "/missing", "/invalid", "/one"] {
            let checker = checker().await;
            let url: Url = format!("{}{route}", server.uri()).parse().unwrap();
            let response = checker.raw_get(url).await.unwrap();
            assert!(checker.heuristic(response).await.is_none());
        }
    }

    #[tokio::test]
    async fn limits_buffered_response_bodies() {
        let server = MockServer::start().await;
        let body = format!("{}This page is deprecated.", "x".repeat(MAX_BODY_BYTES));
        mount(
            &server,
            "/large",
            ResponseTemplate::new(200).set_body_string(body),
        )
        .await;
        Mock::given(path_regex("/stalelink-probe-.*"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(
            checker()
                .await
                .check(format!("{}/large", server.uri()).parse().unwrap())
                .await
                .is_none()
        );
    }

    #[test]
    fn login_wall_requires_redirect_and_whole_auth_components() {
        let direct_login = RawResponse {
            status: reqwest::StatusCode::OK,
            origin: "https://example.test/login".parse().unwrap(),
            url: "https://example.test/login".parse().unwrap(),
            redirects: vec![],
            headers: reqwest::header::HeaderMap::new(),
            body: String::new(),
            terminated: true,
        };
        let author = RawResponse {
            status: reqwest::StatusCode::OK,
            origin: "https://author.example/start".parse().unwrap(),
            url: "https://author.example/target".parse().unwrap(),
            redirects: vec![(
                reqwest::StatusCode::FOUND,
                "https://author.example/target".parse().unwrap(),
            )],
            headers: reqwest::header::HeaderMap::new(),
            body: String::new(),
            terminated: true,
        };
        assert!(!login_wall(&direct_login));
        assert!(!login_wall(&author));
    }

    #[test]
    fn login_wall_ignores_bare_redirect_query_parameters() {
        let response = RawResponse {
            status: reqwest::StatusCode::OK,
            origin: "https://example.test/start".parse().unwrap(),
            url: "https://example.test/target?redirect=%2Fnext"
                .parse()
                .unwrap(),
            redirects: vec![(
                reqwest::StatusCode::FOUND,
                "https://example.test/target?redirect=%2Fnext"
                    .parse()
                    .unwrap(),
            )],
            headers: reqwest::header::HeaderMap::new(),
            body: String::new(),
            terminated: true,
        };
        assert!(!login_wall(&response));
    }

    #[test]
    fn version_upgrade_changes_only_the_first_segment_without_overflowing() {
        assert_eq!(
            version_upgrade(&"https://example.test/v9/".parse().unwrap())
                .unwrap()
                .path(),
            "/v10/"
        );
        assert_eq!(
            version_upgrade(&"https://example.test/v1/archive/v9/item".parse().unwrap())
                .unwrap()
                .path(),
            "/v2/archive/v9/item"
        );
        assert!(version_upgrade(&"https://example.test/v4294967295/".parse().unwrap()).is_none());
    }

    #[test]
    fn host_key_normalizes_case_and_default_ports() {
        assert_eq!(
            host_key(&"http://EXAMPLE.test/".parse().unwrap()),
            host_key(&"http://example.test:80/".parse().unwrap())
        );
        assert_eq!(
            host_key(&"https://example.test/".parse().unwrap()),
            host_key(&"https://example.test:443/".parse().unwrap())
        );
    }

    #[test]
    fn retry_classes_match_transient_io_errors() {
        assert!(retryable_io_error(&std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        )));
        assert!(retryable_io_error(&std::io::Error::from(
            std::io::ErrorKind::ConnectionAborted
        )));
        assert!(retryable_io_error(&std::io::Error::from(
            std::io::ErrorKind::TimedOut
        )));
        assert!(!retryable_io_error(&std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        )));
    }

    #[test]
    fn retry_backoff_doubles_from_its_base() {
        let base = Duration::from_millis(3);
        assert_eq!(retry_backoff(0, base), base);
        assert_eq!(retry_backoff(1, base), Duration::from_millis(6));
        assert_eq!(retry_backoff(2, base), Duration::from_millis(12));
    }

    #[test]
    fn retry_backoff_is_capped_for_large_retry_counts() {
        assert_eq!(
            retry_backoff(u8::MAX, Duration::from_secs(1)),
            MAX_RETRY_DELAY
        );
        assert_eq!(retry_backoff(1, Duration::MAX), MAX_RETRY_DELAY);
    }

    #[test]
    fn retry_after_extends_a_rate_limit_backoff() {
        let response = reqwest::Response::from(
            hyper::http::Response::builder()
                .status(reqwest::StatusCode::TOO_MANY_REQUESTS)
                .header(reqwest::header::RETRY_AFTER, "7")
                .body(reqwest::Body::default())
                .unwrap(),
        );
        assert_eq!(
            retry_delay(&response, 0, Duration::from_secs(1)),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn retry_after_parses_and_bounds_delay_seconds() {
        assert_eq!(
            retry_after_delay("7", UNIX_EPOCH),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after_delay(&u64::MAX.to_string(), UNIX_EPOCH),
            Some(MAX_RETRY_DELAY)
        );
    }

    #[test]
    fn retry_after_parses_future_http_dates() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let future = now + Duration::from_secs(7);
        assert_eq!(
            retry_after_delay(&httpdate::fmt_http_date(future), now),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn retry_after_ignores_past_and_malformed_http_dates() {
        let now = SystemTime::now();
        assert_eq!(
            retry_after_delay("Wed, 01 Jan 2020 00:00:00 GMT", now),
            None
        );
        assert_eq!(retry_after_delay("not a retry delay", now), None);
    }

    #[tokio::test]
    async fn retries_transient_responses_and_returns_the_final_response() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_responder = attempts.clone();
        Mock::given(method("HEAD"))
            .respond_with(move |_: &wiremock::Request| {
                let attempt = attempts_for_responder.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(if attempt < 2 { 503 } else { 200 })
            })
            .mount(&server)
            .await;
        let result = retrying_checker(2)
            .await
            .status(reqwest::Method::HEAD, server.uri().parse().unwrap())
            .await
            .unwrap();
        assert_eq!(result, StatusResult::Terminal(reqwest::StatusCode::OK));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_non_transient_responses() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let result = retrying_checker(2)
            .await
            .status(reqwest::Method::HEAD, server.uri().parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            result,
            StatusResult::Terminal(reqwest::StatusCode::NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn redirect_limits_count_followed_redirects_in_both_traversals() {
        for redirects in [9, 10, 11] {
            let server = MockServer::start().await;
            for index in 0..redirects {
                mount(
                    &server,
                    &format!("/{index}"),
                    ResponseTemplate::new(302).insert_header("location", format!("/{}", index + 1)),
                )
                .await;
            }
            mount(
                &server,
                &format!("/{redirects}"),
                ResponseTemplate::new(200),
            )
            .await;
            let url: Url = format!("{}/0", server.uri()).parse().unwrap();
            let status = checker()
                .await
                .status(reqwest::Method::HEAD, url.clone())
                .await
                .unwrap();
            let raw = checker().await.raw_get(url.clone()).await.unwrap();
            if redirects <= MAX_REDIRECTS {
                assert_eq!(status, StatusResult::Terminal(reqwest::StatusCode::OK));
                assert!(raw.terminated);
            } else {
                assert_eq!(
                    status,
                    StatusResult::Unterminated("redirect limit exhausted")
                );
                assert!(!raw.terminated);
                let verdict = checker().await.check(url).await.unwrap();
                assert_eq!(verdict.reason, Reason::NetworkError(NetKind::Other));
                assert_eq!(verdict.evidence[0].kind, "redirect-traversal");
                assert_eq!(verdict.evidence[0].detail, "redirect limit exhausted");
            }
        }
    }

    #[tokio::test]
    async fn redirect_loops_are_unterminated_in_both_traversals() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/loop",
            ResponseTemplate::new(302).insert_header("location", "/loop"),
        )
        .await;
        let url: Url = format!("{}/loop", server.uri()).parse().unwrap();
        assert_eq!(
            checker()
                .await
                .status(reqwest::Method::HEAD, url.clone())
                .await
                .unwrap(),
            StatusResult::Unterminated("redirect loop detected")
        );
        assert!(!checker().await.raw_get(url).await.unwrap().terminated);
    }

    #[tokio::test]
    async fn heuristics_reuse_the_get_fallback_walk() {
        let server = MockServer::start().await;
        mount_method(&server, "HEAD", "/a", ResponseTemplate::new(405)).await;
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(redirect(301, "/b"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let verdict = check_route(&server, "/a").await.unwrap();
        assert_eq!(verdict.reason, Reason::PermanentRedirect);
        assert_eq!(verdict.evidence[0].detail, format!("{}/b", server.uri()));
    }

    #[tokio::test]
    async fn heuristics_follow_get_redirects_not_head_redirects() {
        let server = MockServer::start().await;
        mount_method(&server, "HEAD", "/old", redirect(301, "/new")).await;
        mount_method(&server, "GET", "/old", redirect(302, "/actual")).await;
        mount_method(&server, "HEAD", "/moved", redirect(302, "/new")).await;
        mount_method(&server, "GET", "/moved", redirect(301, "/actual")).await;
        mount(&server, "/new", ResponseTemplate::new(200)).await;
        mount(&server, "/actual", ResponseTemplate::new(200)).await;
        assert!(check_route(&server, "/old").await.is_none());
        let verdict = check_route(&server, "/moved").await.unwrap();
        assert_eq!(verdict.reason, Reason::PermanentRedirect);
        assert_eq!(
            verdict.evidence[0].detail,
            format!("{}/actual", server.uri())
        );
    }

    #[tokio::test]
    async fn head_only_login_redirects_are_not_login_walls() {
        let server = MockServer::start().await;
        mount_method(&server, "HEAD", "/start", redirect(302, "/login")).await;
        mount_method(&server, "GET", "/start", ResponseTemplate::new(200)).await;
        mount(&server, "/login", ResponseTemplate::new(200)).await;
        assert!(check_route(&server, "/start").await.is_none());
    }

    #[tokio::test]
    async fn get_login_redirects_are_found_when_head_redirects_elsewhere() {
        let server = MockServer::start().await;
        mount_method(&server, "HEAD", "/start", redirect(301, "/moved")).await;
        mount_method(&server, "GET", "/start", redirect(302, "/login")).await;
        mount(&server, "/moved", ResponseTemplate::new(200)).await;
        mount(&server, "/login", ResponseTemplate::new(200)).await;
        let verdict = check_route(&server, "/start").await.unwrap();
        assert_eq!(verdict.reason, Reason::LoginWall);
        assert_eq!(
            verdict.evidence[0].detail,
            format!("{0}/start -> {0}/login", server.uri())
        );
    }

    #[tokio::test]
    async fn head_redirects_do_not_count_toward_get_loop_detection() {
        let server = MockServer::start().await;
        mount_method(&server, "HEAD", "/a", redirect(302, "/b")).await;
        mount_method(&server, "HEAD", "/b", ResponseTemplate::new(200)).await;
        mount_method(&server, "GET", "/b", redirect(302, "/a")).await;
        mount_method(
            &server,
            "GET",
            "/a",
            ResponseTemplate::new(200).set_body_string("This page is deprecated."),
        )
        .await;
        let verdict = check_route(&server, "/a").await.unwrap();
        assert_eq!(verdict.reason, Reason::StalenessBanner);
    }
}
