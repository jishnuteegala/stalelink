use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use chrono::Utc;
use reqwest::{Client, StatusCode};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use crate::model::{Evidence, NetKind, Reason, Verdict};

pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = Option<Verdict>> + Send + 'a>>;
pub trait Checker: Send + Sync {
    fn check(&self, url: Url) -> CheckFuture<'_>;
}

pub struct HttpChecker {
    client: Client,
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
    ) -> Result<Self, reqwest::Error> {
        let _lychee = lychee_lib::ClientBuilder::builder()
            .max_retries(retries)
            .timeout(timeout)
            .user_agent(user_agent.clone())
            .build()
            .client();
        Client::builder()
            .timeout(timeout)
            .user_agent(user_agent)
            .build()
            .map(|client| Self {
                client,
                retries,
                per_host,
                hosts: Mutex::new(HashMap::new()),
            })
    }
    async fn request(&self, url: &Url) -> Result<StatusCode, reqwest::Error> {
        let host = url.host_str().unwrap_or_default().to_owned();
        let semaphore = self
            .hosts
            .lock()
            .await
            .entry(host)
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_host)))
            .clone();
        let _permit = semaphore
            .acquire()
            .await
            .expect("per-host semaphore is never closed");
        let mut attempts = 0;
        loop {
            let result = async {
                let response = self.client.head(url.clone()).send().await?;
                if matches!(
                    response.status(),
                    StatusCode::METHOD_NOT_ALLOWED | StatusCode::FORBIDDEN
                ) {
                    Ok(self.client.get(url.clone()).send().await?.status())
                } else {
                    Ok(response.status())
                }
            }
            .await;
            if result.is_ok() || attempts == self.retries {
                return result;
            }
            attempts += 1;
            tokio::time::sleep(Duration::from_millis(100 * u64::from(attempts))).await;
        }
    }
}
impl Checker for HttpChecker {
    fn check(&self, url: Url) -> CheckFuture<'_> {
        Box::pin(async move {
            match self.request(&url).await {
                Ok(status) if status.is_success() => None,
                Ok(status) => Some(verdict(
                    Reason::HttpStatus(status.as_u16()),
                    status.to_string(),
                )),
                Err(error) => Some(verdict(
                    Reason::NetworkError(network_kind(&error)),
                    error.to_string(),
                )),
            }
        })
    }
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
