use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use std::error::Error;
use std::time::Duration;
use tokio::time::sleep;

const WAIT_MILLIS: u64 = 500;
/// Stop doubling the backoff once the wait exceeds this many milliseconds.
const WAIT_DOUBLE_LIMIT_MILLIS: u64 = 5_000;

fn build_client() -> Result<Client, Box<dyn Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("alt-rating/0.1"));
    headers.insert(
        "X-Requested-With",
        HeaderValue::from_static("XMLHttpRequest"),
    );

    Ok(Client::builder()
        .default_headers(headers)
        .pool_max_idle_per_host(0)
        .build()?)
}

/// Wait, then GET `url`. On transport/HTTP failure or rejected body, double the wait
/// (until over 5s) and retry until `accept` returns true.
pub async fn fetch_text_with_retry(
    url: &str,
    mut accept: impl FnMut(&str) -> bool,
) -> Result<String, Box<dyn Error>> {
    let mut wait_millis = WAIT_MILLIS;
    loop {
        sleep(Duration::from_millis(wait_millis)).await;
        match try_fetch_text(url).await {
            Ok(text) if accept(&text) => return Ok(text),
            _ => {
                if wait_millis <= WAIT_DOUBLE_LIMIT_MILLIS {
                    wait_millis = wait_millis.saturating_mul(2);
                }
            }
        }
    }
}

async fn try_fetch_text(url: &str) -> Result<String, Box<dyn Error>> {
    let client = build_client()?;
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

async fn try_fetch_bytes(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let client = build_client()?;
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

pub async fn fetch_bytes_with_retry(
    url: &str,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut wait_millis = WAIT_MILLIS;
    loop {
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
}
