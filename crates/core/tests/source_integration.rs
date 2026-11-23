use std::collections::VecDeque;
use std::fs;
use std::future::{pending, ready};
use std::path::Path;
use std::time::Duration;

use http::Response;
use reqwest::Url;
use serde_json::{Value, json};
use tempfile::tempdir;
use videosift_core::VideoSource;
use videosift_core::bilibili::audio::{AudioError, fetch_audio_with};
use videosift_core::bilibili::media::{PageMedia, PageMediaError, discover_page_media_with};
use videosift_core::bilibili::metadata::{MetadataError, VideoMetadata, fetch_metadata_with};
use videosift_core::bilibili::subtitles::{SubtitleCue, SubtitleError, fetch_subtitle_with};
use videosift_core::bilibili::{
    BilibiliInput, BilibiliSource, PageSelection, RequestError, SourceInputError,
};
use videosift_core::local_file::{LocalFileError, validate_local_file};

const BV: &str = "BV1xx411c7mD";
const PAGE_URL: &str = "https://www.bilibili.com/video/BV1xx411c7mD?p=2";
const METADATA_URL: &str = "https://api.bilibili.com/x/web-interface/view?bvid=BV1xx411c7mD";
const SUBTITLES_URL: &str = "https://api.bilibili.com/x/player/v2?bvid=BV1xx411c7mD&cid=92";
const MANIFEST_URL: &str =
    "https://api.bilibili.com/x/player/playurl?bvid=BV1xx411c7mD&cid=92&fnval=16";
const SUBTITLE_URL: &str = "https://aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json";
const AUDIO_URL: &str = "https://upos-sz-mirrorcos.bilivideo.com/upgcxcode/synthetic.m4s";
const AUDIO_BODY: &[u8] = b"\0\0\0\x10ftypM4A \0\0\0\0\0\0\0\x0cmdatdata";

struct MockGet(VecDeque<(&'static str, reqwest::Response)>);

impl MockGet {
    fn new<const N: usize>(steps: [(&'static str, reqwest::Response); N]) -> Self {
        Self(steps.into())
    }

    fn get(&mut self, url: Url) -> reqwest::Response {
        let (expected, response) = self.0.pop_front().expect("unexpected request");
        assert_eq!(url.as_str(), expected);
        response
    }

    fn assert_finished(&self) {
        assert!(self.0.is_empty(), "expected more requests");
    }
}

fn reply(body: Value) -> reqwest::Response {
    Response::new(body.to_string()).into()
}

fn metadata_reply() -> reqwest::Response {
    reply(json!({"code":0,"data":{
        "bvid":BV,"title":"Synthetic video","videos":2,"pages":[
            {"page":1,"cid":91,"part":"Page 1","duration":1},
            {"page":2,"cid":92,"part":"Page 2","duration":2}
        ]
    }}))
}

fn subtitle_catalog(tracks: Vec<Value>) -> reqwest::Response {
    reply(json!({"code":0,"data":{"cid":92,"subtitle":{"subtitles":tracks}}}))
}

fn track(language: &str, url: &str) -> Value {
    json!({"lan":language,"lan_doc":language,"subtitle_url":url})
}

fn audio_manifest() -> reqwest::Response {
    reply(json!({"code":0,"data":{"cid":92,"dash":{"audio":[
        {"baseUrl":AUDIO_URL,"mimeType":"audio/mp4","codecs":"mp4a.40.2"}
    ]}}}))
}

fn assert_source_fixture(source: VideoSource<'_>, name: &str) {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../../../contracts/examples/valid.json")).unwrap();
    let fixture = fixtures
        .iter()
        .find(|fixture| fixture["name"] == name)
        .unwrap();
    assert_eq!(
        serde_json::to_value(source).unwrap(),
        fixture["instance"]["source"]
    );
}

async fn selected_page(transport: &mut MockGet) -> (VideoMetadata, BilibiliSource<'static>) {
    let (metadata, selection) =
        fetch_metadata_with(PAGE_URL, None, |url| ready(Ok(transport.get(url))))
            .await
            .unwrap();
    let PageSelection::Selected(source) = selection else {
        panic!("expected the page from the URL");
    };
    assert_eq!(source.part_id, 2);
    (metadata, source)
}

#[test]
fn local_file_uses_the_shared_source_contract_and_authorized_root() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let input = Path::new("合成 clip.bin");
    fs::write(root.path().join(input), b"abc").unwrap();
    fs::write(outside.path().join(input), b"abc").unwrap();
    let source = validate_local_file(root.path(), input).unwrap();
    assert_source_fixture(VideoSource::Local(source), "local_file_digest");
    assert_eq!(
        validate_local_file(root.path(), &outside.path().join(input)),
        Err(LocalFileError::InvalidPath)
    );
}

#[tokio::test]
async fn explicit_page_choice_prefers_subtitles_and_fetches_only_the_chosen_track() {
    let mut transport = MockGet::new([
        (METADATA_URL, metadata_reply()),
        (
            SUBTITLES_URL,
            subtitle_catalog(vec![
                track("en", "https://i0.hdslb.com/bfs/subtitle/other.json"),
                track("zh-CN", SUBTITLE_URL),
            ]),
        ),
        (
            SUBTITLE_URL,
            reply(json!({"body":[{"from":0.125,"to":2.0,"content":"合成字幕"}]})),
        ),
    ]);
    let (metadata, selection) = fetch_metadata_with(BV, None, |url| ready(Ok(transport.get(url))))
        .await
        .unwrap();
    assert_eq!(
        selection,
        PageSelection::SelectionRequired { page_count: 2 }
    );
    let input = BilibiliInput::parse(BV).unwrap();
    let PageSelection::Selected(source) = metadata.select(&input, Some(2)).unwrap() else {
        panic!("expected explicit P2");
    };
    let PageMedia::Subtitles(tracks) =
        discover_page_media_with(&metadata, &source, |url| ready(Ok(transport.get(url))))
            .await
            .unwrap()
    else {
        panic!("expected subtitles to take precedence");
    };
    assert_eq!(tracks.len(), 2);
    let chosen = tracks
        .iter()
        .find(|track| track.language() == "zh-CN")
        .unwrap();
    let cues = fetch_subtitle_with(chosen, |url| ready(Ok(transport.get(url))))
        .await
        .unwrap();
    assert_eq!(
        cues,
        vec![SubtitleCue {
            start_ms: 125,
            end_ms: 2000,
            text: "合成字幕".into()
        }]
    );
    assert_source_fixture(VideoSource::Bilibili(source), "bilibili_part");
    transport.assert_finished();
}

#[tokio::test]
async fn explicit_subtitle_absence_fetches_audio_for_the_same_page() {
    let mut transport = MockGet::new([
        (METADATA_URL, metadata_reply()),
        (SUBTITLES_URL, subtitle_catalog(vec![])),
        (MANIFEST_URL, audio_manifest()),
        (AUDIO_URL, Response::new(AUDIO_BODY).into()),
    ]);
    let (metadata, source) = selected_page(&mut transport).await;
    let PageMedia::Audio(track) =
        discover_page_media_with(&metadata, &source, |url| ready(Ok(transport.get(url))))
            .await
            .unwrap()
    else {
        panic!("expected audio after confirmed subtitle absence");
    };
    assert_eq!(
        fetch_audio_with(&track, |url| ready(Ok(transport.get(url))))
            .await
            .unwrap(),
        AUDIO_BODY
    );
    transport.assert_finished();
}

#[tokio::test]
async fn existing_subtitles_with_an_invalid_body_still_do_not_trigger_audio() {
    let mut transport = MockGet::new([
        (METADATA_URL, metadata_reply()),
        (
            SUBTITLES_URL,
            subtitle_catalog(vec![track("zh-CN", SUBTITLE_URL)]),
        ),
        (
            SUBTITLE_URL,
            reply(json!({"body":[{"from":0,"to":2.01,"content":"Out of range"}]})),
        ),
    ]);
    let (metadata, source) = selected_page(&mut transport).await;
    let PageMedia::Subtitles(tracks) =
        discover_page_media_with(&metadata, &source, |url| ready(Ok(transport.get(url))))
            .await
            .unwrap()
    else {
        panic!("expected subtitles");
    };
    assert_eq!(
        fetch_subtitle_with(&tracks[0], |url| ready(Ok(transport.get(url)))).await,
        Err(SubtitleError::InvalidSubtitle)
    );
    transport.assert_finished();
}

#[tokio::test]
async fn discovery_errors_stop_before_audio_and_keep_the_original_error() {
    let malformed = reply(json!({"code":0,"data":{"cid":92,"subtitle":{}}}));
    let forbidden = Response::builder().status(403).body("").unwrap().into();
    let limited = Response::builder()
        .status(429)
        .header("retry-after", "7")
        .body("")
        .unwrap()
        .into();
    for (response, expected) in [
        (malformed, RequestError::InvalidResponse),
        (forbidden, RequestError::AccessDenied),
        (
            limited,
            RequestError::RateLimited {
                retry_after: Some(Duration::from_secs(7)),
            },
        ),
    ] {
        let mut transport =
            MockGet::new([(METADATA_URL, metadata_reply()), (SUBTITLES_URL, response)]);
        let (metadata, source) = selected_page(&mut transport).await;
        let error =
            discover_page_media_with(&metadata, &source, |url| ready(Ok(transport.get(url))))
                .await
                .unwrap_err();
        assert_eq!(
            error,
            PageMediaError::Subtitles(SubtitleError::Request(expected))
        );
        transport.assert_finished();
    }
}

#[tokio::test(start_paused = true)]
async fn a_discovery_timeout_does_not_trigger_audio() {
    let mut transport = MockGet::new([(METADATA_URL, metadata_reply())]);
    let (metadata, source) = selected_page(&mut transport).await;
    transport.assert_finished();
    let error = discover_page_media_with(&metadata, &source, |url| {
        assert_eq!(url.as_str(), SUBTITLES_URL);
        pending()
    })
    .await
    .unwrap_err();
    assert_eq!(
        error,
        PageMediaError::Subtitles(SubtitleError::Request(RequestError::Timeout))
    );
}

#[tokio::test]
async fn an_invalid_audio_manifest_ends_fallback_before_download() {
    let mut transport = MockGet::new([
        (METADATA_URL, metadata_reply()),
        (SUBTITLES_URL, subtitle_catalog(vec![])),
        (
            MANIFEST_URL,
            reply(json!({"code":0,"data":{"cid":91,"dash":{"audio":[]}}})),
        ),
    ]);
    let (metadata, source) = selected_page(&mut transport).await;
    let error = discover_page_media_with(&metadata, &source, |url| ready(Ok(transport.get(url))))
        .await
        .unwrap_err();
    assert_eq!(error, PageMediaError::Audio(AudioError::InvalidManifest));
    transport.assert_finished();
}

#[tokio::test]
async fn an_oversized_input_never_reaches_transport() {
    let input = format!("{BV}{}", "x".repeat(2048));
    let error = fetch_metadata_with(&input, None, |_| async { panic!("unexpected request") })
        .await
        .unwrap_err();
    assert_eq!(error, MetadataError::Input(SourceInputError::TooLong));
}
