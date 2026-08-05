use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use crate::model::{Evidence, NetKind, Reason, Verdict};

const SOFT_404_SIMILARITY: f64 = 0.9;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_REDIRECTS: usize = 10;
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

pub struct HttpChecker {
    raw: reqwest::Client,
    retries: u8,
    per_host: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}
impl HttpChecker {
    pub fn new(
        timeout: Duration,
        retries: u8,
        per_host: usize,
        user_agent: String,
    ) -> Result<Self, lychee_lib::ErrorKind> {
        Ok(Self {
            raw: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(user_agent)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(lychee_lib::ErrorKind::NetworkRequest)?,
            retries,
            per_host,
            hosts: Mutex::new(HashMap::new()),
        })
    }
    async fn host_permit(&self, url: &Url) -> tokio::sync::OwnedSemaphorePermit {
        let host = match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            (None, _) => String::new(),
        };
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
    async fn status(
        &self,
        method: reqwest::Method,
        url: Url,
    ) -> Result<reqwest::StatusCode, reqwest::Error> {
        let mut current = url;
        let mut visited = HashSet::from([current.clone()]);
        for _ in 0..MAX_REDIRECTS {
            let (response, permit) = self.request(method.clone(), &current).await?;
            let status = response.status();
            if !status.is_redirection() {
                drop(response);
                drop(permit);
                return Ok(status);
            }
            let next = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|location| location.to_str().ok())
                .and_then(|location| current.join(location).ok());
            drop(response);
            drop(permit);
            let Some(next) = next else {
                return Ok(status);
            };
            if !visited.insert(next.clone()) {
                return Ok(reqwest::StatusCode::LOOP_DETECTED);
            }
            current = next;
        }
        Ok(reqwest::StatusCode::LOOP_DETECTED)
    }
    async fn raw_get(&self, url: Url) -> Result<RawResponse, reqwest::Error> {
        let origin = url.clone();
        let mut current = url;
        let mut redirects = Vec::new();
        let mut visited = HashSet::from([current.clone()]);
        for _ in 0..MAX_REDIRECTS {
            let (response, permit) = self.request(reqwest::Method::GET, &current).await?;
            if !response.status().is_redirection() {
                let headers = response.headers().clone();
                let status = response.status();
                let body = read_body(response, permit).await?;
                return Ok(RawResponse {
                    status,
                    origin,
                    url: current,
                    redirects,
                    headers,
                    body,
                    terminated: true,
                });
            }
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return Ok(unterminated_response(
                    response, permit, current, origin, redirects,
                ));
            };
            let Ok(location) = location.to_str() else {
                return Ok(unterminated_response(
                    response, permit, current, origin, redirects,
                ));
            };
            let Ok(next) = current.join(location) else {
                return Ok(unterminated_response(
                    response, permit, current, origin, redirects,
                ));
            };
            redirects.push((response.status(), next.clone()));
            if !visited.insert(next.clone()) {
                return Ok(unterminated_response(
                    response, permit, current, origin, redirects,
                ));
            }
            drop(response);
            drop(permit);
            current = next;
        }
        Ok(RawResponse {
            status: reqwest::StatusCode::LOOP_DETECTED,
            origin,
            url: current,
            redirects,
            headers: reqwest::header::HeaderMap::new(),
            body: String::new(),
            terminated: false,
        })
    }
    async fn request(
        &self,
        method: reqwest::Method,
        url: &Url,
    ) -> Result<(reqwest::Response, tokio::sync::OwnedSemaphorePermit), reqwest::Error> {
        for attempt in 0..=self.retries {
            let permit = self.host_permit(url).await;
            match self.raw.request(method.clone(), url.clone()).send().await {
                Ok(response)
                    if attempt < self.retries
                        && (response.status().is_server_error()
                            || matches!(
                                response.status(),
                                reqwest::StatusCode::REQUEST_TIMEOUT
                                    | reqwest::StatusCode::TOO_MANY_REQUESTS
                            )) =>
                {
                    drop(response);
                    drop(permit);
                }
                Ok(response) => return Ok((response, permit)),
                Err(error) if attempt < self.retries && retryable_request_error(&error) => {
                    drop(permit);
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("the retry loop always returns")
    }
    async fn heuristic(&self, url: Url) -> Option<Verdict> {
        let response = self.raw_get(url.clone()).await.ok()?;
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
        if let Some(upgrade) = version_upgrade(&url)
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
        let sibling = random_sibling(&url);
        if let Ok(sibling_response) = self.raw_get(sibling.clone()).await
            && sibling_response.status.is_success()
            && similarity(&response.body, &sibling_response.body) >= SOFT_404_SIMILARITY
        {
            return Some(verdict(
                Reason::Soft404,
                "sibling-similarity".into(),
                format!(
                    "{} ({:.2})",
                    sibling,
                    similarity(&response.body, &sibling_response.body)
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
            if matches!(
                finding.as_ref().map(|verdict| &verdict.reason),
                Some(Reason::HttpStatus(403 | 405 | 501))
            ) {
                let fallback = outcome(self.status(reqwest::Method::GET, url.clone()).await);
                return if fallback.is_some() {
                    fallback
                } else {
                    self.heuristic(url).await
                };
            }
            if finding.is_some() {
                finding
            } else {
                self.heuristic(url).await
            }
        })
    }
}
fn outcome(status: Result<reqwest::StatusCode, reqwest::Error>) -> Option<Verdict> {
    let (reason, detail) = match status {
        Ok(status) if status.is_success() => return None,
        Ok(status) => (Reason::HttpStatus(status.as_u16()), status.to_string()),
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
fn retryable_request_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || (error.is_request()
            && io_error_in_chain(error).is_some_and(|io_error| {
                matches!(
                    io_error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::TimedOut
                )
            }))
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

fn unterminated_response(
    response: reqwest::Response,
    permit: tokio::sync::OwnedSemaphorePermit,
    url: Url,
    origin: Url,
    redirects: Vec<(reqwest::StatusCode, Url)>,
) -> RawResponse {
    let status = response.status();
    let headers = response.headers().clone();
    drop(response);
    drop(permit);
    RawResponse {
        status,
        origin,
        url,
        redirects,
        headers,
        body: String::new(),
        terminated: false,
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
    use std::time::{Duration, Instant};

    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, path_regex},
    };

    use super::*;
    use crate::model::Confidence;

    async fn checker() -> HttpChecker {
        HttpChecker::new(Duration::from_secs(2), 0, 2, "stalelink-test".into()).unwrap()
    }

    async fn mount(server: &MockServer, route: &str, response: ResponseTemplate) {
        Mock::given(path(route))
            .respond_with(response)
            .mount(server)
            .await;
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
        Mock::given(method("HEAD"))
            .and(path("/deprecated"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/deprecated"))
            .respond_with(ResponseTemplate::new(200).set_body_string("This page is deprecated."))
            .mount(&server)
            .await;
        let verdict = checker()
            .await
            .check(format!("{}/deprecated", server.uri()).parse().unwrap())
            .await
            .unwrap();
        assert_eq!(verdict.reason, Reason::StalenessBanner);
    }

    #[tokio::test]
    async fn base_redirects_share_the_destination_host_limit() {
        let destination = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
            .mount(&destination)
            .await;
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        for server in [&first, &second] {
            Mock::given(method("HEAD"))
                .respond_with(
                    ResponseTemplate::new(302)
                        .insert_header("location", format!("{}/target", destination.uri())),
                )
                .mount(server)
                .await;
        }
        let checker =
            HttpChecker::new(Duration::from_secs(2), 0, 1, "stalelink-test".into()).unwrap();
        let started = Instant::now();
        let (first_status, second_status) = tokio::join!(
            checker.status(
                reqwest::Method::HEAD,
                format!("{}/start", first.uri()).parse().unwrap()
            ),
            checker.status(
                reqwest::Method::HEAD,
                format!("{}/start", second.uri()).parse().unwrap()
            ),
        );
        assert_eq!(first_status.unwrap(), reqwest::StatusCode::OK);
        assert_eq!(second_status.unwrap(), reqwest::StatusCode::OK);
        assert!(started.elapsed() >= Duration::from_millis(180));
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
        mount(
            &server,
            "/missing",
            ResponseTemplate::new(301).insert_header("location", "/login"),
        )
        .await;
        mount(
            &server,
            "/invalid",
            ResponseTemplate::new(301).insert_header("location", "/login?returnUrl=x"),
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
            assert!(
                checker()
                    .await
                    .heuristic(format!("{}{route}", server.uri()).parse().unwrap())
                    .await
                    .is_none()
            );
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
}
