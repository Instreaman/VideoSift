use serde_json::Value;
use videosift_core::bilibili::{
    BilibiliInput, PageCatalog, PageSelection, SourceInputError as Error,
};

const BV: &str = "BV1xx411c7mD";
const PAGE: &str = "https://www.bilibili.com/video/BV1xx411c7mD";

fn catalog(pages: &[u32]) -> Option<PageCatalog<'_>> {
    Some(PageCatalog {
        source_id: BV,
        pages,
    })
}

#[test]
fn accepts_whole_identifiers_and_canonical_playback_paths() {
    for text in [BV, PAGE, &format!("{PAGE}/")] {
        let input = BilibiliInput::parse(text).unwrap();
        assert_eq!(input.source_id(), BV);
        assert_eq!(
            input.select(None, None),
            Ok(PageSelection::MetadataRequired { requested: None })
        );
    }
    for text in [
        "",
        "BV123",
        "BV12345678901",
        "bv1xx411c7mD",
        "BV123456789_",
        "BV12345678é",
        " BV1xx411c7mD",
        "BV1xx411c7mD\n",
        "prefixBV1xx411c7mD",
        "BV1xx411c7mD?p=2",
        "https://example.test/video/BV1xx411c7mD",
        "https://www.bilibili.com.example.test/video/BV1xx411c7mD",
        "http://www.bilibili.com/video/BV1xx411c7mD",
        "https://www.bilibili.com@other.test/video/BV1xx411c7mD",
        "https://www.bilibili.com:443/video/BV1xx411c7mD",
        "https://b23.tv/demo",
        "https://www.bilibili.com/video/other/BV1xx411c7mD",
        &format!("{PAGE}//"),
        &format!("{PAGE}#part"),
        &format!("{PAGE}/%2e"),
    ] {
        assert_eq!(
            BilibiliInput::parse(text),
            Err(Error::InvalidInput),
            "{text:?}"
        );
    }
    assert_eq!(
        BilibiliInput::parse(&"a".repeat(2048)),
        Err(Error::InvalidInput)
    );
    assert_eq!(BilibiliInput::parse(&"a".repeat(2049)), Err(Error::TooLong));
}

#[test]
fn parses_only_one_unambiguous_decimal_page() {
    for (text, expected) in [("1", 1), ("2", 2), ("4294967295", u32::MAX)] {
        let url = format!("{PAGE}/?p={text}");
        let input = BilibiliInput::parse(&url).unwrap();
        assert_eq!(
            input.select(None, None),
            Ok(PageSelection::MetadataRequired {
                requested: Some(expected)
            })
        );
    }
    for query in [
        "p=0",
        "p=01",
        "p=",
        "p=+2",
        "p=-1",
        "p=2.0",
        "p=4294967296",
        "p=%32",
        "p=２",
        "p=2#x",
    ] {
        assert_eq!(
            BilibiliInput::parse(&format!("{PAGE}?{query}")),
            Err(Error::InvalidPage)
        );
    }
    for query in [
        "",
        "p",
        "P=1",
        "track=x",
        "p=2&p=2",
        "p=2&p=3",
        "p=2&track=x",
        "track=x&p=2",
    ] {
        assert_eq!(
            BilibiliInput::parse(&format!("{PAGE}?{query}")),
            Err(Error::InvalidQuery)
        );
    }
}

#[test]
fn requires_a_choice_only_for_a_known_multipart_catalog() {
    let input = BilibiliInput::parse(BV).unwrap();
    assert_eq!(
        input.select(None, catalog(&[1, 2])),
        Ok(PageSelection::SelectionRequired { page_count: 2 })
    );
    assert_eq!(
        input.select(Some(2), None),
        Ok(PageSelection::MetadataRequired { requested: Some(2) })
    );
    assert_eq!(input.select(Some(0), None), Err(Error::InvalidPage));
    assert_eq!(
        input.select(Some(2), catalog(&[1])),
        Err(Error::PageOutOfRange)
    );
    assert_eq!(
        input.select(Some(u32::MAX), catalog(&[1, 2])),
        Err(Error::PageOutOfRange)
    );
    let PageSelection::Selected(single) = input.select(None, catalog(&[1])).unwrap() else {
        panic!("single page expected")
    };
    assert_eq!(single.part_id, 1);
}

#[test]
fn reconciles_explicit_choices_and_matches_the_shared_fixture() {
    let url = format!("{PAGE}?p=2");
    let input = BilibiliInput::parse(&url).unwrap();
    assert_eq!(input.select(Some(1), None), Err(Error::ConflictingPage));
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../../../contracts/examples/valid.json")).unwrap();
    let fixture = fixtures
        .iter()
        .find(|f| f["name"] == "bilibili_part")
        .unwrap();
    for selection in [
        input.select(None, catalog(&[1, 2])),
        input.select(Some(2), catalog(&[1, 2])),
        BilibiliInput::parse(BV)
            .unwrap()
            .select(Some(2), catalog(&[1, 2])),
    ] {
        let PageSelection::Selected(source) = selection.unwrap() else {
            panic!("resolved source expected")
        };
        assert_eq!(
            serde_json::to_value(source).unwrap(),
            fixture["instance"]["source"]
        );
    }
}

#[test]
fn rejects_incomplete_duplicate_unordered_and_foreign_catalogs() {
    let input = BilibiliInput::parse(BV).unwrap();
    for pages in [&[][..], &[0], &[2], &[1, 1], &[1, 3], &[2, 1]] {
        assert_eq!(
            input.select(Some(1), catalog(pages)),
            Err(Error::InvalidCatalog)
        );
    }
    assert_eq!(
        input.select(
            None,
            Some(PageCatalog {
                source_id: "BV1234567890",
                pages: &[1]
            })
        ),
        Err(Error::CatalogMismatch)
    );
}
