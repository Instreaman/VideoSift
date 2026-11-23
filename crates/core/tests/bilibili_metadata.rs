#[path = "support/streaming_response.rs"]
mod streaming_response;

use std::future::{pending, ready};
use std::time::Duration;

use http::Response;
use serde_json::{Value, json};
use videosift_core::bilibili::metadata::{MetadataError, fetch_metadata_with};
use videosift_core::bilibili::{BilibiliInput, PageSelection, SourceInputError};

const BV: &str = "BV0000000001";
const URL_P2: &str = "https://www.bilibili.com/video/BV0000000001/?p=2";

fn envelope(count: u32) -> Value {
    let pages: Vec<_> = (1..=count)
        .map(|p| {
            json!({
                "page": p, "cid": 90 + p, "part": format!("合成分 P {p}"), "duration": p * 2
            })
        })
        .collect();
    json!({"code": 0, "data": {"bvid": BV, "title": "合成标题", "videos": count, "pages": pages}})
}

fn reply(body: Value) -> reqwest::Response {
    Response::new(body.to_string()).into()
}

async fn failure(body: Value) -> MetadataError {
    fetch_metadata_with(BV, None, |_| ready(Ok(reply(body))))
        .await
        .unwrap_err()
}

#[tokio::test]
async fn fetches_only_the_normalized_video_and_reuses_the_catalog() {
    let (metadata, initial) = fetch_metadata_with(BV, None, |url| {
        assert_eq!(
            url.as_str(),
            "https://api.bilibili.com/x/web-interface/view?bvid=BV0000000001"
        );
        ready(Ok(reply(envelope(2))))
    })
    .await
    .unwrap();
    assert_eq!(initial, PageSelection::SelectionRequired { page_count: 2 });
    assert_eq!(metadata.source_id(), BV);
    assert_eq!(metadata.title, "合成标题");
    let p = &metadata.pages()[1];
    assert_eq!(
        (p.number, p.cid, p.title.as_str(), p.duration_ms),
        (2, 92, "合成分 P 2", 4000)
    );
    let input = BilibiliInput::parse(URL_P2).unwrap();
    let PageSelection::Selected(source) = metadata.select(&input, Some(2)).unwrap() else {
        panic!("expected P2")
    };
    assert_eq!(
        serde_json::to_value(source).unwrap(),
        json!({"kind":"bilibili","source_id":BV,"part_id":2})
    );
    let (_, selected) = fetch_metadata_with(URL_P2, None, |url| {
        assert_eq!(url.query(), Some("bvid=BV0000000001"));
        ready(Ok(reply(envelope(2))))
    })
    .await
    .unwrap();
    assert_eq!(selected, metadata.select(&input, None).unwrap());
}

#[tokio::test]
async fn selects_single_page_and_reports_out_of_range_after_fetch() {
    let (_, selected) = fetch_metadata_with(BV, None, |_| ready(Ok(reply(envelope(1)))))
        .await
        .unwrap();
    assert!(matches!(selected, PageSelection::Selected(s) if s.part_id == 1));
    let error = fetch_metadata_with(BV, Some(2), |_| ready(Ok(reply(envelope(1)))))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        MetadataError::Input(SourceInputError::PageOutOfRange)
    );
}

#[tokio::test]
async fn invalid_and_conflicting_requests_never_reach_transport() {
    for (input, p, expected) in [
        ("invalid", None, SourceInputError::InvalidInput),
        (BV, Some(0), SourceInputError::InvalidPage),
        (URL_P2, Some(1), SourceInputError::ConflictingPage),
    ] {
        let error = fetch_metadata_with(input, p, |_| async {
            panic!("invalid input reached transport")
        })
        .await
        .unwrap_err();
        assert_eq!(error, MetadataError::Input(expected));
    }
}

#[tokio::test]
async fn rejects_mismatched_incomplete_or_ambiguous_catalogs() {
    for (pointer, value, expected) in [
        (
            "/data/bvid",
            json!("BV0000000002"),
            MetadataError::Input(SourceInputError::CatalogMismatch),
        ),
        (
            "/data/pages/1/page",
            json!(1),
            MetadataError::Input(SourceInputError::InvalidCatalog),
        ),
        (
            "/data/pages/1/page",
            json!(3),
            MetadataError::Input(SourceInputError::InvalidCatalog),
        ),
        ("/data/videos", json!(3), MetadataError::InvalidMetadata),
        (
            "/data/pages/1/cid",
            json!(91),
            MetadataError::InvalidMetadata,
        ),
        (
            "/data/pages/0/cid",
            json!(0),
            MetadataError::InvalidMetadata,
        ),
        ("/data/title", json!("  "), MetadataError::InvalidMetadata),
        (
            "/data/pages/0/part",
            json!("字".repeat(1025)),
            MetadataError::InvalidMetadata,
        ),
        (
            "/data/pages/0/duration",
            json!(-1),
            MetadataError::InvalidMetadata,
        ),
    ] {
        let mut view = envelope(2);
        *view.pointer_mut(pointer).unwrap() = value;
        assert_eq!(failure(view).await, expected, "{pointer}");
    }
    for value in [
        envelope(0),
        envelope(1001),
        json!({"code":0}),
        json!({"code":0,"data":{"unexpected": true}}),
    ] {
        assert_eq!(failure(value).await, MetadataError::InvalidMetadata);
    }
}

#[tokio::test]
async fn maps_http_and_application_errors_without_exposing_the_response() {
    for (status, expected) in [
        (302, MetadataError::Source),
        (401, MetadataError::AccessDenied),
        (403, MetadataError::AccessDenied),
        (404, MetadataError::NotFound),
        (412, MetadataError::Blocked),
        (500, MetadataError::Source),
        (
            429,
            MetadataError::RateLimited {
                retry_after: Some(Duration::from_secs(12)),
            },
        ),
    ] {
        let response = Response::builder()
            .status(status)
            .header("retry-after", "12")
            .body("private fixture message")
            .unwrap()
            .into();
        let error = fetch_metadata_with(BV, None, |_| ready(Ok(response)))
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("private fixture message"));
    }
    for (code, expected) in [
        (-101, MetadataError::AccessDenied),
        (-403, MetadataError::AccessDenied),
        (-404, MetadataError::NotFound),
        (-412, MetadataError::Blocked),
        (-352, MetadataError::Blocked),
        (42, MetadataError::Source),
    ] {
        assert_eq!(
            failure(json!({"code":code,"message":"private fixture message","data":"unexpected"}))
                .await,
            expected
        );
    }
}

#[tokio::test(start_paused = true)]
async fn bounds_request_time_and_distinguishes_transport_failure() {
    let error = fetch_metadata_with(BV, None, |_| pending())
        .await
        .unwrap_err();
    assert_eq!(error, MetadataError::Timeout);
    let error = fetch_metadata_with(BV, None, |_| ready(Err(MetadataError::Transport)))
        .await
        .unwrap_err();
    assert_eq!(error, MetadataError::Transport);
}

#[tokio::test]
async fn carries_http_date_or_unknown_retry_delay_without_retrying() {
    for (header, expected) in [
        ("Sun, 06 Nov 1994 08:49:37 GMT", Some(Duration::ZERO)),
        ("unknown", None),
    ] {
        let response = Response::builder()
            .status(429)
            .header("retry-after", header)
            .body("")
            .unwrap()
            .into();
        let error = fetch_metadata_with(BV, None, |_| ready(Ok(response)))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            MetadataError::RateLimited {
                retry_after: expected
            }
        );
    }
}

#[tokio::test]
async fn rejects_oversized_or_invalid_json_bodies() {
    for (body, expected) in [
        (vec![b' '; 1024 * 1024 + 1], MetadataError::TooLarge),
        (b"{broken".to_vec(), MetadataError::InvalidMetadata),
    ] {
        let response = Response::new(body).into();
        let error = fetch_metadata_with(BV, None, |_| ready(Ok(response)))
            .await
            .unwrap_err();
        assert_eq!(error, expected);
    }
}

#[tokio::test(start_paused = true)]
async fn enforces_byte_budget_and_deadline_while_reading_an_unknown_length_body() {
    for (chunks, expected) in [(129, MetadataError::TooLarge), (1, MetadataError::Timeout)] {
        let response = streaming_response::streaming_response(chunks * 8192);
        assert_eq!(
            fetch_metadata_with(BV, None, |_| ready(Ok(response)))
                .await
                .unwrap_err(),
            expected
        );
    }
}
