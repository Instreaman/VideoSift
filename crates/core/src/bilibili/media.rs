//! Discover the preferred media for a selected Bilibili page.

use std::future::Future;

use reqwest::{Response, Url};
use thiserror::Error;

use super::audio::{AudioError, AudioTrack, resolve_audio_with};
use super::metadata::VideoMetadata;
use super::subtitles::{
    SubtitleAvailability, SubtitleError, SubtitleTrack, discover_subtitle_availability_with,
};
use super::{BilibiliSource, RequestError};

/// Tracks ready for retrieval through the corresponding subtitle or audio API.
#[derive(Debug)]
pub enum PageMedia {
    Subtitles(Vec<SubtitleTrack>),
    Audio(AudioTrack),
}

#[derive(Debug, PartialEq, Eq, Error)]
pub enum PageMediaError {
    #[error(transparent)]
    Subtitles(#[from] SubtitleError),
    #[error(transparent)]
    Audio(#[from] AudioError),
}

/// Prefer discovered subtitles and resolve audio only after explicit absence.
///
/// The supplied transport handles at most two sequential requests. It owns any
/// live request authorization and pacing. The caller chooses a subtitle track
/// and retrieves the selected media separately.
pub async fn discover_page_media_with<F, Fut>(
    metadata: &VideoMetadata,
    source: &BilibiliSource<'_>,
    mut get: F,
) -> Result<PageMedia, PageMediaError>
where
    F: FnMut(Url) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    match discover_subtitle_availability_with(metadata, source, &mut get).await? {
        SubtitleAvailability::Available(tracks) => Ok(PageMedia::Subtitles(tracks)),
        SubtitleAvailability::Absent(absence) => {
            Ok(PageMedia::Audio(resolve_audio_with(&absence, get).await?))
        }
    }
}
