use std::future::Future;
use std::time::{Duration, SystemTime};

use reqwest::{Response, Url, header::RETRY_AFTER};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use thiserror::Error;

pub(super) const DEADLINE: Duration = Duration::from_secs(10);
pub(super) const API_HOST: &str = "api.bilibili.com";

#[derive(Debug, PartialEq, Eq, Error)]
pub enum RequestError {
    #[error("Bilibili request exceeded its deadline")]
    Timeout,
    #[error("Bilibili transport failed")]
    Transport,
    #[error("Bilibili destination is not permitted")]
    Destination,
    #[error("Bilibili access requires authentication or permission")]
    AccessDenied,
    #[error("Bilibili blocked this request")]
    Blocked,
    #[error("The requested Bilibili resource was not found")]
    NotFound,
    #[error("Bilibili rate-limited this request")]
    RateLimited { retry_after: Option<Duration> },
    #[error("Bilibili returned an unsuccessful response")]
    Source,
    #[error("Bilibili response exceeds its byte limit")]
    TooLarge,
    #[error("Bilibili response is missing or malformed")]
    InvalidResponse,
}

impl From<reqwest::Error> for RequestError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Transport
        }
    }
}

pub(super) fn destination_allowed(url: &Url) -> bool {
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    match url.host_str() {
        Some(API_HOST) => matches!(
            url.path(),
            "/x/web-interface/view" | "/x/player/v2" | "/x/player/playurl"
        ),
        _ => subtitle_destination(url) || audio_destination(url),
    }
}

pub(super) fn subtitle_destination(url: &Url) -> bool {
    match url.host_str() {
        Some("aisubtitle.hdslb.com") => download_path(url.path(), "/bfs/ai_subtitle/"),
        Some("i0.hdslb.com") => download_path(url.path(), "/bfs/subtitle/"),
        _ => false,
    }
}

pub(super) fn audio_destination(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("upos-sz-mirrorcos.bilivideo.com" | "upos-sz-mirrorali.bilivideo.com")
    ) && url.path().ends_with(".m4s")
        && download_path(url.path(), "/upgcxcode/")
}

fn download_path(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|tail| {
        tail.split('/').all(|component| {
            !matches!(component, "" | "." | "..")
                && component
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        })
    })
}

pub(super) fn download_url(
    raw: &str,
    permits_destination: fn(&Url) -> bool,
) -> Result<Url, RequestError> {
    if raw.len() > 4096
        || raw
            .bytes()
            .any(|b| b.is_ascii_control() || matches!(b, b' ' | b'\\'))
    {
        return Err(RequestError::Destination);
    }
    let path = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("//"))
        .ok_or(RequestError::Destination)?;
    let (authority, _) = path.split_once('/').ok_or(RequestError::Destination)?;
    let url = Url::parse(&format!("https://{path}")).map_err(|_| RequestError::Destination)?;
    if !matches!(url.host_str(), Some(host) if authority.eq_ignore_ascii_case(host))
        || url.fragment().is_some()
        || !permits_destination(&url)
    {
        return Err(RequestError::Destination);
    }
    Ok(url)
}

pub(super) fn deserialize_present_cid<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}

pub(super) async fn fetch<F, Fut, E>(get: F, url: Url, limit: usize) -> Result<Vec<u8>, E>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, E>>,
    E: From<RequestError>,
{
    tokio::time::timeout(DEADLINE, async {
        let response = get(url).await?;
        read_response(response, limit).await.map_err(E::from)
    })
    .await
    .map_err(|_| E::from(RequestError::Timeout))?
}

async fn read_response(mut response: Response, limit: usize) -> Result<Vec<u8>, RequestError> {
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(RequestError::AccessDenied),
        404 => return Err(RequestError::NotFound),
        412 => return Err(RequestError::Blocked),
        429 => {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|header| header.to_str().ok())
                .and_then(|value| {
                    if !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()) {
                        return value.parse().ok().map(Duration::from_secs);
                    }
                    let date = httpdate::parse_http_date(value).ok()?;
                    Some(date.duration_since(SystemTime::now()).unwrap_or_default())
                });
            return Err(RequestError::RateLimited { retry_after });
        }
        _ => return Err(RequestError::Source),
    }

    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(RequestError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit - body.len() {
            return Err(RequestError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct Envelope<'a> {
    code: i64,
    #[serde(borrow)]
    data: Option<&'a RawValue>,
}

pub(super) fn decode_data<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, RequestError> {
    let envelope: Envelope<'_> =
        serde_json::from_slice(body).map_err(|_| RequestError::InvalidResponse)?;
    match envelope.code {
        0 => {}
        -101 | -403 => return Err(RequestError::AccessDenied),
        -404 => return Err(RequestError::NotFound),
        -412 | -352 => return Err(RequestError::Blocked),
        _ => return Err(RequestError::Source),
    }
    let data = envelope.data.ok_or(RequestError::InvalidResponse)?;
    serde_json::from_str(data.get()).map_err(|_| RequestError::InvalidResponse)
}
