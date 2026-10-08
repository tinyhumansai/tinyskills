#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn every_type_deserializes_from_an_empty_object() {
    let query: SkillQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(query, SkillQuery::default());
    assert_eq!(query.page, 1);
    assert_eq!(query.page_size, SkillQuery::DEFAULT_PAGE_SIZE);
    assert_eq!(query.read, ReadPolicy::AllowStale);
    let _: SkillSummary = serde_json::from_str("{}").unwrap();
    let _: SkillDetail = serde_json::from_str("{}").unwrap();
    let _: SkillPage = serde_json::from_str("{}").unwrap();
    let _: SourceStatus = serde_json::from_str("{}").unwrap();
    let _: EntryKey = serde_json::from_str("{}").unwrap();
    let _: Facet = serde_json::from_str("{}").unwrap();
    let _: RegistryFacets = serde_json::from_str("{}").unwrap();
    let _: Validators = serde_json::from_str("{}").unwrap();
    let entry: RegistryEntry = serde_json::from_str("{}").unwrap();
    assert_eq!(entry, RegistryEntry::default());
}

#[test]
fn round_trips_use_snake_case() {
    let mut query = SkillQuery::text("notes");
    query.installable_only = true;
    query.read = ReadPolicy::RequireFresh;
    let value = serde_json::to_value(&query).unwrap();
    assert_eq!(value["installable_only"], true);
    assert_eq!(value["read"], "require_fresh");
    assert_eq!(serde_json::from_value::<SkillQuery>(value).unwrap(), query);

    let mut page = SkillPage {
        freshness: Freshness::LocalFallback,
        ..SkillPage::default()
    };
    page.sources.push(SourceStatus::default());
    let value = serde_json::to_value(&page).unwrap();
    assert_eq!(value["freshness"], "local_fallback");
    assert_eq!(serde_json::from_value::<SkillPage>(value).unwrap(), page);

    let key = EntryKey::in_registry("hermes", "apple-notes");
    let value = serde_json::to_value(&key).unwrap();
    assert_eq!(serde_json::from_value::<EntryKey>(value).unwrap(), key);
    assert_eq!(EntryKey::new("x").registry, None);

    let summary = RegistryErrorSummary::of(&RegistryError::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(5)),
    });
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["kind"], "rate_limited");
    assert_eq!(value["retry_after_secs"], 5);
    assert_eq!(
        serde_json::from_value::<RegistryErrorSummary>(value).unwrap(),
        summary
    );
}

#[test]
fn freshness_orders_best_first() {
    assert!(Freshness::Live < Freshness::Cached);
    assert!(Freshness::Cached < Freshness::LocalFallback);
    assert_eq!(Freshness::default(), Freshness::Live);
}

#[test]
fn contract_versions() {
    assert!(is_registry_contract_compatible(REGISTRY_CONTRACT_VERSION));
    assert!(is_registry_contract_compatible((
        REGISTRY_CONTRACT_VERSION.0,
        99
    )));
    assert!(!is_registry_contract_compatible((
        REGISTRY_CONTRACT_VERSION.0 + 1,
        0
    )));
}

#[test]
fn validators_produce_conditional_headers() {
    let headers = vec![
        ("ETag".to_owned(), "\"v\"".to_owned()),
        ("last-modified".to_owned(), "yesterday".to_owned()),
    ];
    let validators = Validators::from_headers(&headers);
    assert_eq!(
        validators.conditional_headers(),
        vec![
            ("If-None-Match".to_owned(), "\"v\"".to_owned()),
            ("If-Modified-Since".to_owned(), "yesterday".to_owned()),
        ]
    );
    assert_eq!(Validators::default().conditional_headers().len(), 0);
}

#[test]
fn entries_compare_and_convert() {
    let mut entry = RegistryEntry::default();
    entry.entry.id = "x".to_owned();
    entry.entry.download_url = "https://x.test/SKILL.md".to_owned();
    entry.overview = "o".to_owned();
    entry.install_identifier = Some("i".to_owned());
    let mut other = entry.clone();
    assert_eq!(entry, other);
    other.entry.name = "changed".to_owned();
    assert_ne!(entry, other);
    let detail = SkillDetail::from_entry("r", &entry);
    assert_eq!(detail.summary.registry, "r");
    assert!(detail.summary.installable);
    assert_eq!(detail.overview, "o");
    assert_eq!(detail.install_identifier.as_deref(), Some("i"));
}

#[test]
fn error_summary_deserializes_with_defaults() {
    let summary: RegistryErrorSummary = serde_json::from_str("{}").unwrap();
    assert_eq!(summary, RegistryErrorSummary::default());
    assert_eq!(summary.kind, RegistryErrorKind::Unavailable);
    assert_eq!(summary.message, "");
    assert_eq!(summary.retry_after_secs, None);
}
