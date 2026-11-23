//! Fetch and validate one complete view catalog before resolving the selected page.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use reqwest::{Response, Url};
use serde::Deserialize;
use thiserror::Error;

use super::request::{self, RequestError};
use super::{BilibiliInput, PageCatalog, PageSelection, SourceInputError};

const BODY_LIMIT: usize = 1024 * 1024;
const PAGE_LIMIT: usize = 1000;

#[derive(Debug, PartialEq, Eq, Error)]
pub enum MetadataError {
    #[error(transparent)]
    Input(#[from] SourceInputError),
    #[error("Metadata request exceeded its deadline")]
    Timeout,
    #[error("Metadata transport failed")]
    Transport,
    #[error("Metadata destination is not permitted")]
    Destination,
    #[error("Metadata access requires authentication or permission")]
    AccessDenied,
    #[error("The source blocked this request")]
    Blocked,
    #[error("Video metadata was not found")]
    NotFound,
    #[error("The source rate-limited this request")]
    RateLimited { retry_after: Option<Duration> },
    #[error("The source returned an unsuccessful response")]
    Source,
    #[error("Metadata exceeds the response byte limit")]
    TooLarge,
    #[error("Metadata is missing or malformed")]
    InvalidMetadata,
}

impl From<RequestError> for MetadataError {
    fn from(error: RequestError) -> Self {
        match error {
            RequestError::Timeout => Self::Timeout,
            RequestError::Transport => Self::Transport,
            RequestError::Destination => Self::Destination,
            RequestError::AccessDenied => Self::AccessDenied,
            RequestError::Blocked => Self::Blocked,
            RequestError::NotFound => Self::NotFound,
            RequestError::RateLimited { retry_after } => Self::RateLimited { retry_after },
            RequestError::Source => Self::Source,
            RequestError::TooLarge => Self::TooLarge,
            RequestError::InvalidResponse => Self::InvalidMetadata,
        }
    }
}

#[derive(Debug)]
pub struct VideoMetadata {
    source_id: String,
    pub title: String,
    pages: Vec<PageMetadata>,
}

#[derive(Debug)]
pub struct PageMetadata {
    pub number: u32,
    pub cid: u64,
    pub title: String,
    pub duration_ms: u64,
}

impl VideoMetadata {
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn pages(&self) -> &[PageMetadata] {
        &self.pages
    }

    /// Reuse the fetched catalog when the caller provides an explicit choice.
    pub fn select<'a>(
        &self,
        input: &BilibiliInput<'a>,
        page: Option<u32>,
    ) -> Result<PageSelection<'a>, SourceInputError> {
        let numbers: Vec<_> = self.pages.iter().map(|p| p.number).collect();
        input.select(
            page,
            Some(PageCatalog {
                source_id: &self.source_id,
                pages: &numbers,
            }),
        )
    }
}

/// Performs a live HTTPS request. The caller must obtain permission before calling.
pub async fn fetch_authorized_metadata(
    input: &str,
    page: Option<u32>,
) -> Result<(VideoMetadata, PageSelection<'_>), MetadataError> {
    fetch_metadata_with(input, page, |url| async {
        super::https::get(url).await.map_err(MetadataError::from)
    })
    .await
}

/// Runs the same request and response handling with a caller-supplied GET transport.
pub async fn fetch_metadata_with<F, Fut>(
    input: &str,
    page: Option<u32>,
    get: F,
) -> Result<(VideoMetadata, PageSelection<'_>), MetadataError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, MetadataError>>,
{
    let input = BilibiliInput::parse(input)?;
    input.select(page, None)?;
    let mut url = Url::parse("https://api.bilibili.com/x/web-interface/view")
        .map_err(|_| MetadataError::Destination)?;
    url.query_pairs_mut().append_pair("bvid", input.source_id());
    let body = request::fetch(get, url, BODY_LIMIT).await?;
    let metadata = decode(&body)?;
    let selection = metadata.select(&input, page)?;
    Ok((metadata, selection))
}

#[derive(Deserialize)]
struct View {
    bvid: String,
    title: String,
    videos: usize,
    pages: Vec<ViewPage>,
}

#[derive(Deserialize)]
struct ViewPage {
    page: u32,
    cid: u64,
    part: String,
    duration: u32,
}

fn decode(body: &[u8]) -> Result<VideoMetadata, MetadataError> {
    let view: View = request::decode_data(body)?;
    if !(1..=PAGE_LIMIT).contains(&view.videos)
        || view.videos != view.pages.len()
        || !valid_title(&view.title)
    {
        return Err(MetadataError::InvalidMetadata);
    }
    let mut ids = HashSet::with_capacity(view.videos);
    let mut pages = Vec::with_capacity(view.videos);
    for p in view.pages {
        if p.cid == 0 || !ids.insert(p.cid) || !valid_title(&p.part) {
            return Err(MetadataError::InvalidMetadata);
        }
        pages.push(PageMetadata {
            number: p.page,
            cid: p.cid,
            title: p.part,
            duration_ms: u64::from(p.duration) * 1000,
        });
    }
    Ok(VideoMetadata {
        source_id: view.bvid,
        title: view.title,
        pages,
    })
}

fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= 1024
}
