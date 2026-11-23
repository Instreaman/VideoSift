#[path = "support/streaming_response.rs"]
mod streaming_response;

use std::future::{pending, ready};
use std::time::Duration;

use http::Response;
use serde_json::{Value, json};
use videosift_core::bilibili::audio::{
    AudioError, AudioTrack, fetch_audio_with, resolve_audio_with,
};
use videosift_core::bilibili::metadata::{VideoMetadata, fetch_metadata_with};
use videosift_core::bilibili::subtitles::{
    ConfirmedNoSubtitles, SubtitleAvailability, SubtitleError, discover_subtitle_availability_with,
    fetch_subtitle_with,
};
use videosift_core::bilibili::{BilibiliSource, PageSelection, RequestError};

const BV: &str = "BV0000000001";
const AUDIO_URL: &str =
    "//upos-sz-mirrorcos.bilivideo.com/upgcxcode/92/00/92/92-30280.m4s?token=synthetic";
const AUDIO_BODY: &[u8] =
    b"\x00\x00\x00\x10ftypM4A \x00\x00\x00\x00\x00\x00\x00\x0cmdat\x01\x02\x03\x04";

fn response(body: Value) -> reqwest::Response {
    Response::new(body.to_string()).into()
}

async fn selected() -> (VideoMetadata, BilibiliSource<'static>) {
    let (metadata, selection) = fetch_metadata_with(BV, Some(2), |_| {
        ready(Ok(response(json!({"code":0,"data":{
            "bvid":BV,"title":"合成视频","videos":2,"pages":[
                {"page":1,"cid":91,"part":"合成 P1","duration":1},
                {"page":2,"cid":92,"part":"合成 P2","duration":2}
            ]
        }}))))
    })
    .await
    .unwrap();
    let PageSelection::Selected(source) = selection else {
        panic!("expected selected P2")
    };
    (metadata, source)
}

fn subtitle_catalog(tracks: Vec<Value>) -> Value {
    json!({"code":0,"data":{"cid":92,"subtitle":{"subtitles":tracks}}})
}

async fn absence<'a>(
    metadata: &'a VideoMetadata,
    source: &BilibiliSource<'_>,
) -> ConfirmedNoSubtitles<'a> {
    let availability = discover_subtitle_availability_with(metadata, source, |_| {
        ready(Ok(response(subtitle_catalog(vec![]))))
    })
    .await
    .unwrap();
    let SubtitleAvailability::Absent(absence) = availability else {
        panic!("expected confirmed absence")
    };
    absence
}

fn track(url: &str) -> Value {
    json!({"baseUrl":url,"mimeType":"audio/mp4","codecs":"mp4a.40.2"})
}

fn manifest(audio: Vec<Value>) -> Value {
    json!({"code":0,"data":{"cid":92,"dash":{"audio":audio}}})
}

async fn audio_track() -> AudioTrack {
    let (metadata, source) = selected().await;
    resolve_audio_with(&absence(&metadata, &source).await, |_| {
        ready(Ok(response(manifest(vec![track(AUDIO_URL)]))))
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn explicit_absence_fetches_only_audio_for_the_selected_page() {
    let (metadata, source) = selected().await;
    let availability = discover_subtitle_availability_with(&metadata, &source, |url| {
        assert_eq!(
            url.as_str(),
            "https://api.bilibili.com/x/player/v2?bvid=BV0000000001&cid=92"
        );
        ready(Ok(response(subtitle_catalog(vec![]))))
    })
    .await
    .unwrap();
    let SubtitleAvailability::Absent(absence) = availability else {
        panic!("expected confirmed absence")
    };
    let candidate = resolve_audio_with(&absence, |url| {
        assert_eq!(url.as_str(), "https://api.bilibili.com/x/player/playurl?bvid=BV0000000001&cid=92&fnval=16");
        let mut body = manifest(vec![
            json!({"base_url":"https://unused.invalid/codec","mime_type":"audio/flac","codecs":"flac"}),
            track(AUDIO_URL)
        ]);
        body["data"]["dash"]["video"] = json!([{"baseUrl":"https://unused.invalid/video"}]);
        ready(Ok(response(body)))
    }).await.unwrap();
    assert!(!format!("{candidate:?}").contains("synthetic"));
    let audio = fetch_audio_with(&candidate, |url| {
        assert_eq!(url.as_str(), format!("https:{AUDIO_URL}"));
        ready(Ok(Response::new(AUDIO_BODY).into()))
    })
    .await
    .unwrap();
    assert_eq!(audio, AUDIO_BODY);
}

#[tokio::test]
async fn subtitle_presence_does_not_grant_audio_even_if_subtitle_fetch_fails() {
    let (metadata, source) = selected().await;
    let availability = discover_subtitle_availability_with(&metadata, &source, |_| {
        ready(Ok(response(subtitle_catalog(vec![json!({
            "lan":"zh-CN","lan_doc":"合成字幕","subtitle_url":"//aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json"
        })]))))
    }).await.unwrap();
    let SubtitleAvailability::Available(tracks) = availability else {
        panic!("fallback must not be granted")
    };
    assert_eq!(tracks.len(), 1);
    let error = fetch_subtitle_with(&tracks[0], |_| ready(Ok(response(json!({"body":[]})))))
        .await
        .unwrap_err();
    assert_eq!(error, SubtitleError::InvalidSubtitle);
}

#[tokio::test]
async fn invalid_subtitle_responses_do_not_grant_audio() {
    let (metadata, mut source) = selected().await;
    source.part_id = 3;
    assert_eq!(
        discover_subtitle_availability_with(&metadata, &source, |_| async {
            panic!("unvalidated page reached transport")
        })
        .await
        .unwrap_err(),
        SubtitleError::InvalidPage
    );
    source.part_id = 2;

    for (body, expected) in [
        (
            json!({"code":0,"data":{"subtitle":{"subtitles":null}}}),
            RequestError::InvalidResponse,
        ),
        (
            json!({"code":0,"data":{"subtitle":{}}}),
            RequestError::InvalidResponse,
        ),
        (
            json!({"code":0,"data":{"cid":null,"subtitle":{"subtitles":[]}}}),
            RequestError::InvalidResponse,
        ),
        (json!({"code":-101,"data":null}), RequestError::AccessDenied),
        (json!({"code":-412,"data":null}), RequestError::Blocked),
    ] {
        let error =
            discover_subtitle_availability_with(&metadata, &source, |_| ready(Ok(response(body))))
                .await
                .unwrap_err();
        assert_eq!(error, SubtitleError::Request(expected));
    }
    let mismatched = json!({"code":0,"data":{"cid":91,"subtitle":{"subtitles":[]}}});
    assert_eq!(
        discover_subtitle_availability_with(&metadata, &source, |_| {
            ready(Ok(response(mismatched)))
        })
        .await
        .unwrap_err(),
        SubtitleError::InvalidCatalog
    );
}

#[tokio::test]
async fn rejects_protected_invalid_or_unsupported_manifests() {
    let (metadata, source) = selected().await;
    let absent = absence(&metadata, &source).await;
    let mut mismatched = manifest(vec![track(AUDIO_URL)]);
    mismatched["data"]["cid"] = json!(91);
    let mut protected = manifest(vec![track(AUDIO_URL)]);
    protected["data"]["is_drm"] = json!(true);
    for (body, expected) in [
        (mismatched, AudioError::InvalidManifest),
        (protected, AudioError::ProtectedMedia),
        (manifest(vec![]), AudioError::NoSupportedAudio),
        (
            manifest(vec![
                json!({"baseUrl":AUDIO_URL,"mimeType":"video/mp4","codecs":"avc1"}),
            ]),
            AudioError::NoSupportedAudio,
        ),
        (
            json!({"code":0,"data":{"dash":{}}}),
            AudioError::Request(RequestError::InvalidResponse),
        ),
        (
            json!({"code":0,"data":{"dash":null}}),
            AudioError::InvalidManifest,
        ),
        (
            manifest(vec![track(AUDIO_URL); 33]),
            AudioError::InvalidManifest,
        ),
    ] {
        assert_eq!(
            resolve_audio_with(&absent, |_| ready(Ok(response(body))))
                .await
                .unwrap_err(),
            expected
        );
    }
}

#[tokio::test]
async fn rejects_audio_destinations_outside_the_allowlist() {
    let (metadata, source) = selected().await;
    let absent = absence(&metadata, &source).await;
    for url in [
        "http://upos-sz-mirrorcos.bilivideo.com/upgcxcode/synthetic.m4s",
        "https://audio.invalid/upgcxcode/synthetic.m4s",
        "https://upos-sz-mirrorcos.bilivideo.com.invalid/upgcxcode/synthetic.m4s",
        "https://upos-sz-mirrorcos.bilivideo.com:443/upgcxcode/synthetic.m4s",
        "https://upos-sz-mirrorcos.bilivideo.com/upgcxcode/../elsewhere/synthetic.m4s",
        "https://upos-sz-mirrorcos.bilivideo.com/upgcxcode/synthetic.json",
        "https://aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json",
    ] {
        let result =
            resolve_audio_with(&absent, |_| ready(Ok(response(manifest(vec![track(url)]))))).await;
        assert_eq!(
            result.unwrap_err(),
            AudioError::Request(RequestError::Destination),
            "{url}"
        );
    }
}

#[tokio::test]
async fn rejects_empty_truncated_and_non_mp4_bodies() {
    let track = audio_track().await;
    for bytes in [
        b"".as_slice(),
        &AUDIO_BODY[..16],
        b"{\"error\":\"synthetic\"}",
    ] {
        let error = fetch_audio_with(&track, |_| ready(Ok(Response::new(bytes).into())))
            .await
            .unwrap_err();
        assert_eq!(error, AudioError::InvalidAudio);
    }
}

#[tokio::test(start_paused = true)]
async fn rejects_manifest_and_unknown_length_audio_one_byte_over_budget() {
    let (metadata, source) = selected().await;
    let absent = absence(&metadata, &source).await;
    let candidate = audio_track().await;
    let manifest = streaming_response::streaming_response(1024 * 1024 + 1);
    let audio = streaming_response::streaming_response(32 * 1024 * 1024 + 1);
    assert_eq!(manifest.content_length(), None);
    assert_eq!(audio.content_length(), None);
    assert_eq!(
        resolve_audio_with(&absent, |_| ready(Ok(manifest)))
            .await
            .unwrap_err(),
        AudioError::Request(RequestError::TooLarge)
    );
    assert_eq!(
        fetch_audio_with(&candidate, |_| ready(Ok(audio)))
            .await
            .unwrap_err(),
        AudioError::Request(RequestError::TooLarge)
    );
}

#[tokio::test]
async fn rejects_incomplete_top_level_boxes_and_missing_media_data() {
    let track = audio_track().await;
    let file_type = &AUDIO_BODY[..16];
    for body in [
        AUDIO_BODY[..17].to_vec(),
        AUDIO_BODY[..AUDIO_BODY.len() - 1].to_vec(),
        [AUDIO_BODY, b"x"].concat(),
        [AUDIO_BODY, b"\0\0\0\x0cfreeabc"].concat(),
        [file_type, b"\0\0\0\x08free"].concat(),
        [file_type, b"\0\0\0\x08mdat"].concat(),
        [file_type, b"\0\0\0\x07mdat"].concat(),
        [file_type, b"\0\0\0\x01mdat\0"].concat(),
        [file_type, b"\0\0\0\x01mdat", &[0xff; 8]].concat(),
        [AUDIO_BODY, b"\0\0\0\x08uuid"].concat(),
    ] {
        assert_eq!(
            fetch_audio_with(&track, |_| ready(Ok(Response::new(body).into())))
                .await
                .unwrap_err(),
            AudioError::InvalidAudio
        );
    }
}

#[tokio::test]
async fn accepts_complete_multi_box_extended_and_to_end_media() {
    let track = audio_track().await;
    for media in [
        b"\0\0\0\x01mdat\0\0\0\0\0\0\0\x11x\0\0\0\x08free".as_slice(),
        b"\0\0\0\0mdatx",
    ] {
        let body = [&AUDIO_BODY[..16], b"\0\0\0\x08free", media].concat();
        let response = Response::new(body.clone()).into();
        assert_eq!(
            fetch_audio_with(&track, |_| ready(Ok(response)))
                .await
                .unwrap(),
            body
        );
    }
}

#[tokio::test(start_paused = true)]
async fn propagates_blocking_rate_limits_deadlines_and_budget_errors() {
    let (metadata, source) = selected().await;
    let absent = absence(&metadata, &source).await;
    let candidate = audio_track().await;
    for (status, expected) in [
        (302, RequestError::Source),
        (403, RequestError::AccessDenied),
        (
            429,
            RequestError::RateLimited {
                retry_after: Some(Duration::from_secs(12)),
            },
        ),
    ] {
        let mock = |_| {
            ready(Ok(Response::builder()
                .status(status)
                .header("retry-after", "12")
                .body("synthetic error")
                .unwrap()
                .into()))
        };
        assert_eq!(
            resolve_audio_with(&absent, mock).await.unwrap_err(),
            AudioError::Request(expected)
        );
    }
    assert_eq!(
        fetch_audio_with(&candidate, |_| pending())
            .await
            .unwrap_err(),
        AudioError::Request(RequestError::Timeout)
    );
    let oversized = |_| ready(Ok(Response::new(vec![0; 32 * 1024 * 1024 + 1]).into()));
    assert_eq!(
        fetch_audio_with(&candidate, oversized).await.unwrap_err(),
        AudioError::Request(RequestError::TooLarge)
    );
}
