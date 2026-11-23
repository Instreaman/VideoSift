//! Resolve and retrieve one audio representation after confirmed subtitle absence.

use std::fmt;
use std::future::Future;

use reqwest::{Response, Url};
use serde::Deserialize;
use thiserror::Error;

use super::request::{self, RequestError};
use super::subtitles::ConfirmedNoSubtitles;

const MANIFEST_LIMIT: usize = 1024 * 1024;
const AUDIO_LIMIT: usize = 32 * 1024 * 1024;
const REPRESENTATION_LIMIT: usize = 32;

#[derive(Debug, PartialEq, Eq, Error)]
pub enum AudioError {
    #[error(transparent)]
    Request(#[from] RequestError),
    #[error("Bilibili returned an invalid audio manifest")]
    InvalidManifest,
    #[error("Bilibili marked this media as protected")]
    ProtectedMedia,
    #[error("Bilibili did not offer a supported audio track")]
    NoSupportedAudio,
    #[error("The downloaded audio lacks a valid MP4 file or segment header")]
    InvalidAudio,
}

/// A resolved DASH audio candidate with a private, potentially signed download URL.
pub struct AudioTrack {
    url: Url,
}

impl fmt::Debug for AudioTrack {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("AudioTrack").finish_non_exhaustive()
    }
}

/// Resolves audio over HTTPS. The caller must grant live access first.
pub async fn resolve_authorized_audio(
    absence: &ConfirmedNoSubtitles<'_>,
) -> Result<AudioTrack, AudioError> {
    resolve_audio_with(absence, super::https::get).await
}

/// Uses the page whose successful subtitle discovery explicitly reported no tracks.
pub async fn resolve_audio_with<F, Fut>(
    absence: &ConfirmedNoSubtitles<'_>,
    get: F,
) -> Result<AudioTrack, AudioError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    let (bvid, cid) = absence.page();
    let mut url = Url::parse("https://api.bilibili.com/x/player/playurl")
        .map_err(|_| RequestError::Destination)?;
    url.query_pairs_mut()
        .append_pair("bvid", bvid)
        .append_pair("cid", &cid.to_string())
        .append_pair("fnval", "16");
    let body = request::fetch(get, url, MANIFEST_LIMIT).await?;
    let manifest: Manifest = request::decode_data(&body)?;
    if manifest.cid.is_some_and(|reported| reported != cid) {
        return Err(AudioError::InvalidManifest);
    }
    if manifest.is_drm {
        return Err(AudioError::ProtectedMedia);
    }

    let dash = manifest.dash.ok_or(AudioError::InvalidManifest)?;
    if dash.audio.len() > REPRESENTATION_LIMIT {
        return Err(AudioError::InvalidManifest);
    }
    let representation = dash
        .audio
        .iter()
        .find(|track| track.mime_type == "audio/mp4" && track.codecs == "mp4a.40.2")
        .ok_or(AudioError::NoSupportedAudio)?;
    Ok(AudioTrack {
        url: request::download_url(&representation.base_url, request::audio_destination)?,
    })
}

/// Downloads the previously resolved track over HTTPS after the caller grants live access.
pub async fn fetch_authorized_audio(track: &AudioTrack) -> Result<Vec<u8>, AudioError> {
    fetch_audio_with(track, super::https::get).await
}

/// Retrieves a bounded audio body. Container decoding happens downstream.
pub async fn fetch_audio_with<F, Fut>(track: &AudioTrack, get: F) -> Result<Vec<u8>, AudioError>
where
    F: FnOnce(Url) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    let audio = request::fetch(get, track.url.clone(), AUDIO_LIMIT).await?;
    let [a, b, c, d, kind @ ..] = audio.as_slice() else {
        return Err(AudioError::InvalidAudio);
    };
    let header_size = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
    if !(16..audio.len()).contains(&header_size)
        || !matches!(kind.get(..4), Some(b"ftyp" | b"styp"))
    {
        return Err(AudioError::InvalidAudio);
    }
    Ok(audio)
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default, deserialize_with = "request::deserialize_present_cid")]
    cid: Option<u64>,
    #[serde(default)]
    is_drm: bool,
    dash: Option<Dash>,
}

#[derive(Deserialize)]
struct Dash {
    audio: Vec<Representation>,
}

#[derive(Deserialize)]
struct Representation {
    #[serde(alias = "baseUrl")]
    base_url: String,
    #[serde(alias = "mimeType")]
    mime_type: String,
    codecs: String,
}
