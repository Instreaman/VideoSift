//! Discover the subtitle tracks for a validated page and retrieve one selected track.

use std::fmt;
use std::future::Future;

use reqwest::{Response, Url};
use serde::Deserialize;
use thiserror::Error;

use super::BilibiliSource;
use super::metadata::VideoMetadata;
use super::request::{self, RequestError};

const CATALOG_LIMIT: usize = 1024 * 1024;
const SUBTITLE_LIMIT: usize = 2 * 1024 * 1024;
const TRACK_LIMIT: usize = 32;
const CUE_LIMIT: usize = 10_000;
const MAX_TIME_MS: f64 = 9_007_199_254_740_991.0;

#[derive(Debug, PartialEq, Eq, Error)]
pub enum SubtitleError {
    #[error("The selected page does not belong to the validated metadata")]
    InvalidPage,
    #[error("Bilibili returned an invalid subtitle catalog")]
    InvalidCatalog,
    #[error("Bilibili returned an invalid subtitle document")]
    InvalidSubtitle,
    #[error(transparent)]
    Request(#[from] RequestError),
}

/// Track returned by discovery. Its download URL may contain a signature.
pub struct SubtitleTrack {
    url: Url,
    language: String,
    label: String,
    duration_ms: u64,
}

impl SubtitleTrack {
    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl fmt::Debug for SubtitleTrack {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubtitleTrack")
            .finish_non_exhaustive()
    }
}

/// Proof that discovery succeeded and explicitly reported no tracks for this page.
#[derive(Debug)]
pub struct ConfirmedNoSubtitles<'a> {
    source_id: &'a str,
    cid: u64,
}

impl<'a> ConfirmedNoSubtitles<'a> {
    pub(super) fn page(&self) -> (&'a str, u64) {
        (self.source_id, self.cid)
    }
}

#[derive(Debug)]
pub enum SubtitleAvailability<'a> {
    Available(Vec<SubtitleTrack>),
    Absent(ConfirmedNoSubtitles<'a>),
}

/// A source cue on the selected page's timeline. Text is untrusted input.
#[derive(Debug, PartialEq, Eq)]
pub struct SubtitleCue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Discovers tracks over HTTPS. The caller must authorize live platform access first.
pub async fn discover_authorized_subtitles(
    metadata: &VideoMetadata,
    source: &BilibiliSource<'_>,
) -> Result<Vec<SubtitleTrack>, SubtitleError> {
    discover_subtitles_with(metadata, source, super::https::get).await
}

/// An empty list means that the platform explicitly reported no subtitle tracks.
pub async fn discover_subtitles_with<F, Fut>(
    metadata: &VideoMetadata,
    source: &BilibiliSource<'_>,
    get: F,
) -> Result<Vec<SubtitleTrack>, SubtitleError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    match discover_subtitle_availability_with(metadata, source, get).await? {
        SubtitleAvailability::Available(tracks) => Ok(tracks),
        SubtitleAvailability::Absent(_) => Ok(Vec::new()),
    }
}

/// Discovers subtitle availability over HTTPS after the caller grants live access.
pub async fn discover_authorized_subtitle_availability<'a>(
    metadata: &'a VideoMetadata,
    source: &BilibiliSource<'_>,
) -> Result<SubtitleAvailability<'a>, SubtitleError> {
    discover_subtitle_availability_with(metadata, source, super::https::get).await
}

/// A successful, explicitly empty subtitle list permits audio fallback for the same page.
pub async fn discover_subtitle_availability_with<'a, F, Fut>(
    metadata: &'a VideoMetadata,
    source: &BilibiliSource<'_>,
    get: F,
) -> Result<SubtitleAvailability<'a>, SubtitleError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    let page = source
        .part_id
        .checked_sub(1)
        .filter(|_| metadata.source_id() == source.source_id)
        .and_then(|index| metadata.pages().get(index as usize))
        .ok_or(SubtitleError::InvalidPage)?;
    let mut url = Url::parse("https://api.bilibili.com/x/player/v2")
        .map_err(|_| RequestError::Destination)?;
    url.query_pairs_mut()
        .append_pair("bvid", metadata.source_id())
        .append_pair("cid", &page.cid.to_string());
    let body = request::fetch(get, url, CATALOG_LIMIT).await?;
    let player: Player = request::decode_data(&body)?;
    if player.cid.is_some_and(|cid| cid != page.cid)
        || player.subtitle.subtitles.len() > TRACK_LIMIT
    {
        return Err(SubtitleError::InvalidCatalog);
    }

    if player.subtitle.subtitles.is_empty() {
        return Ok(SubtitleAvailability::Absent(ConfirmedNoSubtitles {
            source_id: metadata.source_id(),
            cid: page.cid,
        }));
    }

    let tracks = player
        .subtitle
        .subtitles
        .into_iter()
        .map(|track| {
            if !valid_language(&track.lan)
                || track.lan_doc.trim().is_empty()
                || track.lan_doc.chars().count() > 128
            {
                return Err(SubtitleError::InvalidCatalog);
            }
            Ok(SubtitleTrack {
                url: request::download_url(&track.subtitle_url, request::subtitle_destination)?,
                language: track.lan,
                label: track.lan_doc,
                duration_ms: page.duration_ms,
            })
        })
        .collect::<Result<Vec<_>, SubtitleError>>()?;
    Ok(SubtitleAvailability::Available(tracks))
}

/// Retrieves one previously discovered track over HTTPS with caller authorization.
pub async fn fetch_authorized_subtitle(
    track: &SubtitleTrack,
) -> Result<Vec<SubtitleCue>, SubtitleError> {
    fetch_subtitle_with(track, super::https::get).await
}

/// Fetches and validates one track without requesting the discovery endpoint again.
pub async fn fetch_subtitle_with<F, Fut>(
    track: &SubtitleTrack,
    get: F,
) -> Result<Vec<SubtitleCue>, SubtitleError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    let body = request::fetch(get, track.url.clone(), SUBTITLE_LIMIT).await?;
    let document: SubtitleDocument =
        serde_json::from_slice(&body).map_err(|_| SubtitleError::InvalidSubtitle)?;
    if document.body.is_empty() || document.body.len() > CUE_LIMIT {
        return Err(SubtitleError::InvalidSubtitle);
    }

    let duration_seconds = track.duration_ms as f64 / 1000.0;
    let mut previous_start = 0;
    document
        .body
        .into_iter()
        .map(|cue| {
            if cue.to > duration_seconds {
                return Err(SubtitleError::InvalidSubtitle);
            }
            let start_ms = timestamp_ms(cue.from)?;
            let end_ms = timestamp_ms(cue.to)?;
            if end_ms <= start_ms
                || start_ms < previous_start
                || cue.content.trim().is_empty()
                || cue.content.chars().count() > 65_536
            {
                return Err(SubtitleError::InvalidSubtitle);
            }
            previous_start = start_ms;
            Ok(SubtitleCue {
                start_ms,
                end_ms,
                text: cue.content,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct Player {
    #[serde(default, deserialize_with = "request::deserialize_present_cid")]
    cid: Option<u64>,
    subtitle: TrackList,
}

#[derive(Deserialize)]
struct TrackList {
    subtitles: Vec<RawTrack>,
}

#[derive(Deserialize)]
struct RawTrack {
    lan: String,
    lan_doc: String,
    subtitle_url: String,
}

#[derive(Deserialize)]
struct SubtitleDocument {
    body: Vec<RawCue>,
}

#[derive(Deserialize)]
struct RawCue {
    from: f64,
    to: f64,
    content: String,
}

fn valid_language(language: &str) -> bool {
    (2..=35).contains(&language.len())
        && language
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn timestamp_ms(seconds: f64) -> Result<u64, SubtitleError> {
    let milliseconds = (seconds * 1000.0).round();
    if seconds < 0.0 || !milliseconds.is_finite() || milliseconds > MAX_TIME_MS {
        return Err(SubtitleError::InvalidSubtitle);
    }
    Ok(milliseconds as u64)
}
