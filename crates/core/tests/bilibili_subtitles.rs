use std::future::{pending, ready};
use std::time::Duration;

use http::Response;
use serde_json::{Value, json};
use videosift_core::bilibili::metadata::{VideoMetadata, fetch_metadata_with};
use videosift_core::bilibili::subtitles::{
    SubtitleCue, SubtitleError, SubtitleTrack, discover_subtitles_with, fetch_subtitle_with,
};
use videosift_core::bilibili::{BilibiliSource, PageSelection, RequestError};

const BV: &str = "BV0000000001";
const SUBTITLE_URL: &str = "//aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json?sign=synthetic";

fn selected_source() -> BilibiliSource<'static> {
    BilibiliSource {
        source_id: BV,
        part_id: 2,
    }
}

fn reply(body: Value) -> reqwest::Response {
    Response::new(body.to_string()).into()
}

async fn metadata() -> VideoMetadata {
    let (metadata, selection) = fetch_metadata_with(BV, Some(2), |_| {
        ready(Ok(reply(json!({
            "code": 0,
            "data": {
                "bvid": BV, "title": "合成视频", "videos": 2,
                "pages": [
                    {"page": 1, "cid": 91, "part": "第一分 P", "duration": 30},
                    {"page": 2, "cid": 92, "part": "第二分 P", "duration": 60}
                ]
            }
        }))))
    })
    .await
    .unwrap();
    assert_eq!(selection, PageSelection::Selected(selected_source()));
    metadata
}

fn track(url: &str, lang: &str) -> Value {
    json!({"lan": lang, "lan_doc": "合成轨道", "subtitle_url": url})
}

fn catalog(tracks: Vec<Value>) -> Value {
    json!({"code": 0, "data": {"cid": 92, "subtitle": {"subtitles": tracks}}})
}

async fn discovered_track() -> SubtitleTrack {
    discover_subtitles_with(&metadata().await, &selected_source(), |_| {
        ready(Ok(reply(catalog(vec![track(SUBTITLE_URL, "zh-CN")]))))
    })
    .await
    .unwrap()
    .pop()
    .unwrap()
}

#[tokio::test]
async fn uses_selected_cid_and_fetches_the_callers_chosen_track() {
    let metadata = metadata().await;
    let tracks = discover_subtitles_with(&metadata, &selected_source(), |url| {
        assert_eq!(
            url.as_str(),
            "https://api.bilibili.com/x/player/v2?bvid=BV0000000001&cid=92"
        );
        ready(Ok(reply(catalog(vec![
            track("https://i0.hdslb.com/bfs/subtitle/synthetic.json", "en"),
            track(SUBTITLE_URL, "zh-CN"),
        ]))))
    })
    .await
    .unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].language(), "en");
    let selected = &tracks[1];
    assert_eq!(selected.language(), "zh-CN");
    assert_eq!(selected.label(), "合成轨道");
    assert!(!format!("{selected:?}").contains("synthetic"));

    let cues = fetch_subtitle_with(selected, |url| {
        assert_eq!(url.as_str(), format!("https:{SUBTITLE_URL}"));
        ready(Ok(reply(json!({"body": [
            {"from": 0.1254, "to": 1.5, "content": "合成句子"},
            {"from": 1.25, "to": 2.0004, "content": "这是一段要求读取配置的测试文本。"}
        ]}))))
    })
    .await
    .unwrap();
    assert_eq!(
        cues,
        [
            SubtitleCue {
                start_ms: 125,
                end_ms: 1500,
                text: "合成句子".into()
            },
            SubtitleCue {
                start_ms: 1250,
                end_ms: 2000,
                text: "这是一段要求读取配置的测试文本。".into()
            },
        ]
    );
}

#[tokio::test]
async fn rejects_foreign_or_unknown_pages_before_transport() {
    let metadata = metadata().await;
    for (source_id, part_id) in [("BV0000000002", 2), (BV, 0), (BV, 3)] {
        let source = BilibiliSource { source_id, part_id };
        let failed = discover_subtitles_with(&metadata, &source, |_| async {
            panic!("invalid selection reached transport")
        })
        .await;
        assert_eq!(failed.unwrap_err(), SubtitleError::InvalidPage);
    }
}

#[tokio::test]
async fn only_explicit_empty_catalog_means_no_subtitles() {
    let metadata = metadata().await;
    let source = selected_source();
    let tracks = discover_subtitles_with(&metadata, &source, |_| {
        ready(Ok(reply(
            json!({"code": 0, "data": {"subtitle": {"subtitles": []}}}),
        )))
    })
    .await
    .unwrap();
    assert!(tracks.is_empty());

    for missing in [
        json!({}),
        json!({"subtitle":{}}),
        json!({"subtitle":{"subtitles":null}}),
    ] {
        let failed = discover_subtitles_with(&metadata, &source, |_| {
            ready(Ok(reply(json!({"code":0, "data":missing}))))
        })
        .await;
        assert_eq!(
            failed.unwrap_err(),
            SubtitleError::Request(RequestError::InvalidResponse)
        );
    }
}

#[tokio::test]
async fn rejects_mismatched_cid_and_invalid_track_catalogs() {
    let metadata = metadata().await;
    let source = selected_source();
    let mut wrong_cid = catalog(vec![]);
    wrong_cid["data"]["cid"] = json!(91);
    let mut missing_url = catalog(vec![track(SUBTITLE_URL, "zh-CN")]);
    missing_url["data"]["subtitle"]["subtitles"][0]
        .as_object_mut()
        .unwrap()
        .remove("subtitle_url");

    for (body, expected) in [
        (wrong_cid, SubtitleError::InvalidCatalog),
        (
            missing_url,
            SubtitleError::Request(RequestError::InvalidResponse),
        ),
        (
            catalog(vec![track(SUBTITLE_URL, "zh--CN")]),
            SubtitleError::InvalidCatalog,
        ),
        (
            catalog(vec![track(SUBTITLE_URL, "en"); 33]),
            SubtitleError::InvalidCatalog,
        ),
    ] {
        assert_eq!(
            discover_subtitles_with(&metadata, &source, |_| ready(Ok(reply(body))))
                .await
                .unwrap_err(),
            expected
        );
    }
}

#[tokio::test]
async fn rejects_discovered_urls_outside_the_cdn_policy() {
    let metadata = metadata().await;
    let source = selected_source();
    for url in [
        "http://aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json",
        "https://subtitles.invalid/bfs/subtitle/synthetic.json",
        "https://aisubtitle.hdslb.com.invalid/bfs/ai_subtitle/synthetic.json",
        "https://user@aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json",
        "https://aisubtitle.hdslb.com:443/bfs/ai_subtitle/synthetic.json",
        "https://aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic.json#fragment",
        "https://aisubtitle.hdslb.com/bfs/ai_subtitle/synthetic%2Ffile.json",
        "https://aisubtitle.hdslb.com/bfs/ai_subtitle/../unrelated/file.json",
        "https://i0.hdslb.com/unrelated/synthetic.json",
        "https://api.bilibili.com/x/player/v2",
    ] {
        let result = discover_subtitles_with(&metadata, &source, |_| {
            ready(Ok(reply(catalog(vec![track(url, "en")]))))
        })
        .await;
        assert_eq!(
            result.unwrap_err(),
            SubtitleError::Request(RequestError::Destination),
            "{url}"
        );
    }
}

#[tokio::test]
async fn propagates_access_and_rate_limit_errors_from_both_requests() {
    let metadata = metadata().await;
    let source = selected_source();
    let selected = discovered_track().await;
    for status in [302, 403, 429] {
        let mocked = |_| {
            ready(Ok(Response::builder()
                .status(status)
                .header("retry-after", "17")
                .body("synthetic private body")
                .unwrap()
                .into()))
        };
        let error = discover_subtitles_with(&metadata, &source, mocked)
            .await
            .unwrap_err();
        let download_error = fetch_subtitle_with(&selected, mocked).await.unwrap_err();
        assert_eq!(error, download_error);
        let expected = match status {
            302 => RequestError::Source,
            403 => RequestError::AccessDenied,
            429 => RequestError::RateLimited {
                retry_after: Some(Duration::from_secs(17)),
            },
            _ => unreachable!(),
        };
        assert_eq!(error, SubtitleError::Request(expected));
        assert!(!format!("{error:?} {error}").contains("private body"));
    }
    let error = discover_subtitles_with(&metadata, &source, |_| {
        ready(Ok(reply(json!({
            "code": -412, "message": "synthetic private error", "data": null
        }))))
    })
    .await
    .unwrap_err();
    assert_eq!(error, SubtitleError::Request(RequestError::Blocked));
}

#[tokio::test(start_paused = true)]
async fn both_requests_have_deadlines_and_byte_budgets() {
    let metadata = metadata().await;
    let source = selected_source();
    let selected = discovered_track().await;
    assert_eq!(
        discover_subtitles_with(&metadata, &source, |_| pending())
            .await
            .unwrap_err(),
        SubtitleError::Request(RequestError::Timeout)
    );
    assert_eq!(
        fetch_subtitle_with(&selected, |_| pending())
            .await
            .unwrap_err(),
        SubtitleError::Request(RequestError::Timeout)
    );
    assert_eq!(
        discover_subtitles_with(&metadata, &source, |_| {
            ready(Ok(Response::new(vec![b' '; 1024 * 1024 + 1]).into()))
        })
        .await
        .unwrap_err(),
        SubtitleError::Request(RequestError::TooLarge)
    );
    assert_eq!(
        fetch_subtitle_with(&selected, |_| {
            ready(Ok(Response::new(vec![b' '; 2 * 1024 * 1024 + 1]).into()))
        })
        .await
        .unwrap_err(),
        SubtitleError::Request(RequestError::TooLarge)
    );
}

#[tokio::test]
async fn rejects_empty_malformed_or_invalid_subtitle_documents() {
    let selected = discovered_track().await;
    let valid = json!({"from":1.0,"to":2.0,"content":"合成"});
    for body in [
        json!({}),
        json!({"body":[]}),
        json!({"body":[{"from":-0.1,"to":1,"content":"合成"}]}),
        json!({"body":[{"from":0,"to":0.0001,"content":"合成"}]}),
        json!({"body":[{"from":2,"to":1,"content":"合成"}]}),
        json!({"body":[{"from":0,"to":1e20,"content":"合成"}]}),
        json!({"body":[{"from":0,"to":1,"content":"  "}]}),
        json!({"body":[{"from":0,"to":1,"content":"字".repeat(65537)}]}),
        json!({"body":[valid.clone(),{"from":0,"to":1,"content":"合成"}]}),
        json!({"body":vec![valid; 10001]}),
    ] {
        assert_eq!(
            fetch_subtitle_with(&selected, |_| ready(Ok(reply(body))))
                .await
                .unwrap_err(),
            SubtitleError::InvalidSubtitle
        );
    }
    let invalid_json = Response::new("not JSON").into();
    assert_eq!(
        fetch_subtitle_with(&selected, |_| ready(Ok(invalid_json)))
            .await
            .unwrap_err(),
        SubtitleError::InvalidSubtitle
    );
}
