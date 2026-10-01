use reqwest::Client;
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT,
};
use std::error::Error;
use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::time::sleep;

/// Default initial wait before each fetch attempt (milliseconds).
pub const DEFAULT_WAIT_MILLIS: u64 = 100;
/// Stop doubling the backoff once the wait exceeds this many milliseconds.
const WAIT_DOUBLE_LIMIT_MILLIS: u64 = 5_000;

const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    build_client().unwrap_or_else(|err| panic!("failed to build HTTP client: {err}"))
});

#[derive(Debug)]
pub struct RetryLimitReached {
    pub url: String,
    pub attempts: u32,
}

impl fmt::Display for RetryLimitReached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fetch failed after {} attempts: {}",
            self.attempts, self.url
        )
    }
}

impl Error for RetryLimitReached {}

/// How a text GET should present itself to the remote host.
#[derive(Debug, Clone, Copy)]
pub enum TextFetchKind<'a> {
    /// Full page navigation (HTML document).
    Document,
    /// XHR/AJAX request, typically with a profile page as referer.
    Ajax { referer: &'a str },
}

fn build_client() -> Result<Client, Box<dyn Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_USER_AGENT));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );

    Ok(Client::builder()
        .default_headers(headers)
        .cookie_store(true)
        .gzip(true)
        .brotli(true)
        .build()?)
}

fn apply_text_headers(
    request: reqwest::RequestBuilder,
    kind: TextFetchKind<'_>,
) -> Result<reqwest::RequestBuilder, Box<dyn Error>> {
    match kind {
        TextFetchKind::Document => Ok(request
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
            )
            .header("Upgrade-Insecure-Requests", "1")
            .header("Sec-Fetch-Dest", "document")
            .header("Sec-Fetch-Mode", "navigate")
            .header("Sec-Fetch-Site", "none")
            .header("Sec-Fetch-User", "?1")),
        TextFetchKind::Ajax { referer } => Ok(request
            .header(ACCEPT, "text/html, */*;q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .header(REFERER, HeaderValue::from_str(referer)?)
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-origin")),
    }
}

/// Wait, then GET `url`. On transport/HTTP failure or rejected body, double the wait
/// (until over 5s) and retry up to `max_attempts` times until `accept` returns true.
pub async fn fetch_text_with_retry(
    url: &str,
    max_attempts: u32,
    initial_wait_millis: u64,
    kind: TextFetchKind<'_>,
    mut accept: impl FnMut(&str) -> bool,
) -> Result<String, Box<dyn Error>> {
    let mut wait_millis = initial_wait_millis;
    for _ in 0..max_attempts {
        sleep(Duration::from_millis(wait_millis)).await;
        match try_fetch_text(url, kind).await {
            Ok(text) if accept(&text) => return Ok(text),
            _ => {
                if wait_millis <= WAIT_DOUBLE_LIMIT_MILLIS {
                    wait_millis = wait_millis.saturating_mul(2);
                }
            }
        }
    }
    Err(Box::new(RetryLimitReached {
        url: url.to_string(),
        attempts: max_attempts,
    }))
}

async fn try_fetch_text(url: &str, kind: TextFetchKind<'_>) -> Result<String, Box<dyn Error>> {
    let request = apply_text_headers(CLIENT.get(url), kind)?;
    Ok(request.send().await?.error_for_status()?.text().await?)
}

async fn try_fetch_bytes(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(CLIENT
        .get(url)
        .header(ACCEPT, "*/*")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

pub async fn fetch_bytes_with_retry(
    url: &str,
    max_attempts: u32,
    initial_wait_millis: u64,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut wait_millis = initial_wait_millis;
    for _ in 0..max_attempts {
        sleep(Duration::from_millis(wait_millis)).await;
        match try_fetch_bytes(url).await {
            Ok(bytes) if accept(&bytes) => return Ok(bytes),
            _ => {
                if wait_millis <= WAIT_DOUBLE_LIMIT_MILLIS {
                    wait_millis = wait_millis.saturating_mul(2);
                }
            }
        }
    }
    Err(Box::new(RetryLimitReached {
        url: url.to_string(),
        attempts: max_attempts,
    }))
}
