//! Bilibili input, page metadata, subtitles and audio fallback.

pub mod audio;
mod https;
pub mod media;
pub mod metadata;
mod request;
pub mod subtitles;

pub use request::RequestError;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SourceInputError {
    #[error("Input exceeds the 2048-byte limit")]
    TooLong,
    #[error("Expected a BV identifier or a supported HTTPS video page")]
    InvalidInput,
    #[error("Expected exactly one query parameter named p")]
    InvalidQuery,
    #[error("Expected a positive 32-bit page number")]
    InvalidPage,
    #[error("Requested page conflicts with the URL")]
    ConflictingPage,
    #[error("Page catalog belongs to another video")]
    CatalogMismatch,
    #[error("Expected a complete page catalog numbered consecutively from one")]
    InvalidCatalog,
    #[error("Requested page is outside the supplied catalog")]
    PageOutOfRange,
}

/// A syntactically valid input, before checking any page catalog.
#[derive(Debug, PartialEq, Eq)]
pub struct BilibiliInput<'a> {
    source_id: &'a str,
    url_page: Option<u32>,
}

/// Caller-supplied, complete catalog in playback order for a particular video.
pub struct PageCatalog<'a> {
    pub source_id: &'a str,
    pub pages: &'a [u32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum PageSelection<'a> {
    MetadataRequired { requested: Option<u32> },
    SelectionRequired { page_count: usize },
    Selected(BilibiliSource<'a>),
}

/// The Bilibili branch of the shared VideoSource schema.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename = "bilibili")]
pub struct BilibiliSource<'a> {
    pub source_id: &'a str,
    pub part_id: u32,
}

impl<'a> BilibiliInput<'a> {
    pub fn parse(input: &'a str) -> Result<Self, SourceInputError> {
        if input.len() > 2048 {
            return Err(SourceInputError::TooLong);
        }
        let (source_id, url_page) = match input.strip_prefix("https://www.bilibili.com/video/") {
            Some(tail) => {
                let (path, page) = match tail.split_once('?') {
                    None => (tail, None),
                    Some((path, query)) => {
                        if query.contains('&') {
                            return Err(SourceInputError::InvalidQuery);
                        }
                        let digits = query
                            .strip_prefix("p=")
                            .ok_or(SourceInputError::InvalidQuery)?;
                        if digits.is_empty()
                            || digits.starts_with('0')
                            || !digits.bytes().all(|b| b.is_ascii_digit())
                        {
                            return Err(SourceInputError::InvalidPage);
                        }
                        let page = digits
                            .parse::<u32>()
                            .map_err(|_| SourceInputError::InvalidPage)?;
                        (path, Some(page))
                    }
                };
                (path.strip_suffix('/').unwrap_or(path), page)
            }
            None => (input, None),
        };
        if !source_id.strip_prefix("BV").is_some_and(|suffix| {
            suffix.len() == 10 && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
        }) {
            return Err(SourceInputError::InvalidInput);
        }
        Ok(Self {
            source_id,
            url_page,
        })
    }

    pub fn source_id(&self) -> &'a str {
        self.source_id
    }

    pub fn select(
        &self,
        page: Option<u32>,
        catalog: Option<PageCatalog<'_>>,
    ) -> Result<PageSelection<'a>, SourceInputError> {
        if page == Some(0) {
            return Err(SourceInputError::InvalidPage);
        }
        if matches!((page, self.url_page), (Some(x), Some(y)) if x != y) {
            return Err(SourceInputError::ConflictingPage);
        }
        let requested = page.or(self.url_page);
        let Some(catalog) = catalog else {
            return Ok(PageSelection::MetadataRequired { requested });
        };
        if catalog.source_id != self.source_id {
            return Err(SourceInputError::CatalogMismatch);
        }
        let pages = catalog.pages;
        if pages.is_empty() || !pages.iter().copied().eq((1..=u32::MAX).take(pages.len())) {
            return Err(SourceInputError::InvalidCatalog);
        }
        let part_id = match requested {
            Some(p) if u64::from(p) > pages.len() as u64 => {
                return Err(SourceInputError::PageOutOfRange);
            }
            Some(p) => p,
            None if pages.len() == 1 => 1,
            None => {
                return Ok(PageSelection::SelectionRequired {
                    page_count: pages.len(),
                });
            }
        };
        Ok(PageSelection::Selected(BilibiliSource {
            source_id: self.source_id,
            part_id,
        }))
    }
}
