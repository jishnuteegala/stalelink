use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use chrono::Utc;
use lychee_lib::{Client, ClientBuilder, ErrorKind, Status, Uri, ratelimit::RateLimitConfig};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use crate::model::{Evidence, NetKind, Reason, Verdict};

pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = Option<Verdict>> + Send + 'a>>;
pub trait Checker: Send + Sync {
    fn check(&self, url: Url) -> CheckFuture<'_>;
}

pub struct HttpChecker {
    head: Client,
    get: Client,
    per_host: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}
impl HttpChecker {
    pub fn new(
        timeout: Duration,
        retries: u8,
        per_host: usize,
        user_agent: String,
    ) -> Result<Self, ErrorKind> {
        // lychee uses one HTTP method per client, so HEAD-first with GET
        // fallback needs a client per method. lychee's own rate limiter cannot
        // span the two clients, so a shared keyed semaphore enforces the
        // per-host ceiling across both HEAD and GET phases instead.
        let build = |method: reqwest::Method| {
            ClientBuilder::builder()
                .max_retries(retries)
                .timeout(timeout)
                .user_agent(user_agent.clone())
                .method(method)
                .rate_limit_config(RateLimitConfig::from_options(Some(per_host), None))
                .build()
                .client()
        };
        Ok(Self {
            head: build(reqwest::Method::HEAD)?,
            get: build(reqwest::Method::GET)?,
            per_host,
            hosts: Mutex::new(HashMap::new()),
        })
    }
    async fn host_permit(&self, url: &Url) -> tokio::sync::OwnedSemaphorePermit {
        let host = url.host_str().unwrap_or_default().to_owned();
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
    async fn status(client: &Client, url: Url) -> Status {
        match client.check(Uri::from(url)).await {
            Ok(response) => response.into_body().status,
            Err(kind) => Status::Error(kind),
        }
    }
}
impl Checker for HttpChecker {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            let _permit = self.host_permit(&url).await;
            let finding = outcome(&Self::status(&self.head, url.clone()).await);
            // Retry with GET when the server signals HEAD is unsupported or
            // disallowed: 403 Forbidden, 405 Method Not Allowed, 501 Not
            // Implemented. A subsequent GET may well return 200.
            if matches!(
                finding.as_ref().map(|verdict| &verdict.reason),
                Some(Reason::HttpStatus(403 | 405 | 501))
            ) {
                return outcome(&Self::status(&self.get, url).await);
            }
            finding
        })
    }
}
fn outcome(status: &Status) -> Option<Verdict> {
    let reason = match status {
        Status::Ok(_) | Status::Excluded => return None,
        Status::Timeout(_) => Reason::NetworkError(NetKind::Timeout),
        Status::Error(ErrorKind::NetworkRequest(error)) => {
            Reason::NetworkError(network_kind(error))
        }
        other => match other.code() {
            Some(code) => Reason::HttpStatus(code.as_u16()),
            None => Reason::NetworkError(NetKind::Other),
        },
    };
    Some(verdict(reason, status.to_string()))
}
fn verdict(reason: Reason, detail: String) -> Verdict {
    Verdict {
        confidence: reason.confidence(),
        reason,
        evidence: vec![Evidence {
            kind: "status".into(),
            detail,
        }],
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
