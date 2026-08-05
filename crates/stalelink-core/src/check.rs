use std::{future::Future, pin::Pin, time::Duration};

use chrono::Utc;
use lychee_lib::{Client, ClientBuilder, ErrorKind, Status, Uri, ratelimit::RateLimitConfig};
use url::Url;

use crate::model::{Evidence, NetKind, Reason, Verdict};

pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = Option<Verdict>> + Send + 'a>>;
pub trait Checker: Send + Sync {
    fn check(&self, url: Url) -> CheckFuture<'_>;
}

pub struct HttpChecker {
    head: Client,
    get: Client,
}
impl HttpChecker {
    pub fn new(
        timeout: Duration,
        retries: u8,
        per_host: usize,
        user_agent: String,
    ) -> Result<Self, ErrorKind> {
        // lychee uses one HTTP method per client, so HEAD-first with GET
        // fallback needs a client per method.
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
        })
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
            let finding = outcome(&Self::status(&self.head, url.clone()).await);
            if matches!(
                finding.as_ref().map(|verdict| &verdict.reason),
                Some(Reason::HttpStatus(403 | 405))
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
        NetKind::Timeout
    } else if error.is_connect() {
        NetKind::ConnRefused
    } else {
        NetKind::Other
    }
}
