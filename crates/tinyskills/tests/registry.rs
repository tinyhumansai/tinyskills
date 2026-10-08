//! Public-API tests for `SkillRegistry` over in-process test doubles.

#![cfg(feature = "registry")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/fake.rs"]
mod fake;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use fake::{FakeResolver, FakeTransport, ManualClock, PUBLIC_IP, Reply};
use tinyskills::{
    CatalogEntry, EntryKey, FetchPolicy, FileCatalogStore, Freshness, HermesIndexSource,
    HttpMethod, ReadPolicy, RegistryEntry, RegistryError, RegistryErrorKind, RegistryLimits,
    RegistryTimeouts, SkillQuery, SkillRegistry, SkillRegistryBuilder, SkillsShRef, StaticSource,
    TransportError, document_digest, fetch_skill_document,
};

const INDEX: &str = "https://index.test/skills.json";
const FIXTURE: &str = include_str!("fixtures/hermes/skills-sample.json");
const SKILL_MD: &str = "---\nname: apple-notes\ndescription: Manage Apple Notes\n---\nUse memo.\n";
const APPLE_NOTES_RAW: &str = "https://raw.githubusercontent.com/NousResearch/hermes-agent/main/skills/apple/apple-notes/SKILL.md";
const CLAWHUB_RAW: &str = "https://clawhub.ai/api/v1/skills/apple-design/file?path=SKILL.md";
const TTL: Duration = Duration::from_secs(3600);

struct Rig {
    transport: Arc<FakeTransport>,
    resolver: Arc<FakeResolver>,
    clock: Arc<ManualClock>,
}

impl Rig {
    fn new() -> Self {
        let rig = Self {
            transport: Arc::new(FakeTransport::new()),
            resolver: Arc::new(FakeResolver::new()),
            clock: Arc::new(ManualClock::new()),
        };
        rig.transport.get(
            INDEX,
            Reply::ok(FIXTURE)
                .header("ETag", "\"v1\"")
                .header("Last-Modified", "Wed, 07 Oct 2026 12:40:10 GMT"),
        );
        rig
    }

    fn builder(&self) -> SkillRegistryBuilder {
        SkillRegistry::builder(Arc::clone(&self.transport))
            .resolver(Arc::clone(&self.resolver))
            .clock(Arc::clone(&self.clock))
            .source(HermesIndexSource::new("hermes", INDEX))
    }

    fn registry(&self) -> Arc<SkillRegistry> {
        self.builder().build()
    }

    fn index_calls(&self) -> usize {
        self.transport.count(INDEX)
    }
}

fn fixture_len() -> usize {
    serde_json::from_str::<Vec<serde_json::Value>>(FIXTURE)
        .unwrap()
        .len()
}

fn query(text: &str) -> SkillQuery {
    SkillQuery::text(text)
}

fn library_entry(id: &str, name: &str) -> RegistryEntry {
    RegistryEntry::new(CatalogEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        description: "packaged".to_owned(),
        source: "library".to_owned(),
        category: "tools".to_owned(),
        author: None,
        version: None,
        tags: Vec::new(),
        platforms: Vec::new(),
        download_url: String::new(),
        source_url: None,
        docs_path: None,
        commands: Vec::new(),
        env_vars: Vec::new(),
        license: None,
    })
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(1)).await;
}

#[tokio::test]
async fn search_pages_and_facets_over_every_upstream() {
    let rig = Rig::new();
    let registry = rig.registry();

    let mut q = query("");
    q.page_size = 10;
    let page = registry.search(&q).await.unwrap();
    assert_eq!(page.total, fixture_len());
    assert_eq!(page.items.len(), 10);
    assert_eq!(page.total_pages, fixture_len().div_ceil(10));
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(page.fetched_at, Some(rig.clock.unix()));
    assert_eq!(page.sources.len(), 1);
    assert_eq!(page.sources[0].entry_count, fixture_len());
    assert_eq!(page.sources[0].skipped, 0);

    q.page = 0;
    assert_eq!(registry.search(&q).await.unwrap().page, 1);
    q.page = 1000;
    assert_eq!(registry.search(&q).await.unwrap().items.len(), 0);
    q.page = 1;
    q.page_size = 10_000;
    assert_eq!(registry.search(&q).await.unwrap().page_size, 100);
    q.page_size = 0;
    assert_eq!(registry.search(&q).await.unwrap().items.len(), 1);

    let facets = registry.facets(None).await.unwrap();
    assert_eq!(facets.upstreams.len(), 12);
    assert_eq!(
        facets.upstreams.iter().map(|f| f.count).sum::<usize>(),
        fixture_len()
    );
    assert_eq!(facets.upstreams[0].value, "ClawHub");
    let apple = facets
        .categories
        .iter()
        .find(|f| f.value == "apple")
        .unwrap();
    assert_eq!(apple.label, "Apple");
    assert!(registry.facets(Some("nope")).await.is_err());
    assert_eq!(rig.index_calls(), 1);
}

#[tokio::test]
async fn search_filters_and_ranks() {
    let rig = Rig::new();
    let registry = rig
        .builder()
        .featured(["lobehub/9-somboon", "clawhub/apple-design"])
        .build();

    let page = registry.search(&query("apple-notes")).await.unwrap();
    assert_eq!(page.items[0].id, "apple-notes");
    assert_eq!(page.items[0].registry, "hermes");
    assert!(page.items[0].installable);

    let page = registry.search(&query("")).await.unwrap();
    assert_eq!(page.items[0].id, "lobehub/9-somboon");
    assert_eq!(page.items[1].id, "clawhub/apple-design");

    let mut q = query("");
    q.installable_only = true;
    let page = registry.search(&q).await.unwrap();
    assert!(page.total > 0);
    assert!(page.items.iter().all(|item| item.installable));
    assert_ne!(page.items[0].id, "lobehub/9-somboon");

    let mut q = query("");
    q.upstreams = vec!["lobehub".to_owned()];
    q.page_size = 100;
    let page = registry.search(&q).await.unwrap();
    assert_eq!(page.total, 25);
    assert!(
        page.items
            .iter()
            .all(|item| item.upstream == "LobeHub" && !item.installable)
    );

    let mut q = query("");
    q.categories = vec!["APPLE".to_owned()];
    q.page_size = 100;
    let page = registry.search(&q).await.unwrap();
    assert!(page.total > 0);
    assert!(page.items.iter().all(|item| item.category == "apple"));

    let mut q = query("");
    q.registries = vec!["missing".to_owned()];
    let error = registry.search(&q).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UnknownRegistry);
}

#[tokio::test]
async fn detail_resolves_ids_and_names() {
    let rig = Rig::new();
    let registry = rig.registry();

    let detail = registry
        .detail(&EntryKey::new("apple-notes"))
        .await
        .unwrap();
    assert_eq!(detail.download_url, APPLE_NOTES_RAW);
    assert_eq!(detail.summary.category_label.as_deref(), Some("Apple"));
    assert_ne!(detail.overview.len(), 0);
    assert_eq!(
        detail.install_identifier.as_deref(),
        Some("NousResearch/hermes-agent/skills/apple/apple-notes")
    );

    let by_name = registry
        .detail(&EntryKey::in_registry("hermes", "Apple Design"))
        .await
        .unwrap();
    assert_eq!(by_name.summary.id, "clawhub/apple-design");

    let error = registry
        .detail(&EntryKey::new("01 Product Selection"))
        .await
        .unwrap_err();
    match error {
        RegistryError::Ambiguous { count, ids, .. } => {
            assert_eq!(count, 2);
            assert!(ids.contains(&"clawhub/01-product-selection".to_owned()));
        }
        other => panic!("expected ambiguous, got {other:?}"),
    }

    let error = registry
        .detail(&EntryKey::new("apple notez"))
        .await
        .unwrap_err();
    match &error {
        RegistryError::NotFound { closest, .. } => assert_ne!(closest.len(), 0),
        other => panic!("expected not found, got {other:?}"),
    }
    assert_eq!(error.kind().as_str(), "not_found");

    let error = registry
        .detail(&EntryKey::in_registry("missing", "x"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UnknownRegistry);
}

#[tokio::test(start_paused = true)]
async fn concurrent_cold_reads_share_one_fetch() {
    let rig = Rig::new();
    rig.transport
        .get(INDEX, Reply::ok(FIXTURE).delayed(Duration::from_millis(50)));
    let registry = rig.registry();

    let mut readers = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let registry = Arc::clone(&registry);
        readers.spawn(async move { registry.search(&SkillQuery::default()).await });
    }
    while let Some(result) = readers.join_next().await {
        let page = result.unwrap().unwrap();
        assert_eq!(page.total, fixture_len());
        assert_eq!(page.freshness, Freshness::Live);
    }
    assert_eq!(rig.index_calls(), 1);
}

#[tokio::test]
async fn stale_reads_are_cached_and_revalidated_in_the_background() {
    let rig = Rig::new();
    let registry = rig.registry();
    registry.search(&query("")).await.unwrap();
    rig.transport
        .route(HttpMethod::Get, INDEX, vec![Reply::status(304, Vec::new())]);

    rig.clock.advance(TTL + Duration::from_secs(1));
    let page = registry.search(&query("")).await.unwrap();
    assert_eq!(page.freshness, Freshness::Cached);
    assert_eq!(page.total, fixture_len());

    settle().await;
    assert_eq!(rig.index_calls(), 2);
    let conditional = rig.transport.requests().last().cloned().unwrap();
    assert_eq!(conditional.header("if-none-match"), Some("\"v1\""));
    assert_eq!(
        conditional.header("if-modified-since"),
        Some("Wed, 07 Oct 2026 12:40:10 GMT")
    );
    assert!(
        conditional
            .header("user-agent")
            .unwrap()
            .starts_with("tinyskills/")
    );

    let page = registry.search(&query("")).await.unwrap();
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(page.total, fixture_len());
    assert_eq!(page.fetched_at, Some(rig.clock.unix()));
    assert_eq!(rig.index_calls(), 2);
}

#[tokio::test]
async fn require_fresh_refreshes_before_answering() {
    let rig = Rig::new();
    let registry = rig.registry();
    registry.search(&query("")).await.unwrap();
    rig.clock.advance(TTL);

    let mut q = query("");
    q.read = ReadPolicy::RequireFresh;
    let page = registry.search(&q).await.unwrap();
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(rig.index_calls(), 2);
}

#[test]
fn stale_read_without_a_runtime_does_not_panic() {
    let rig = Rig::new();
    let registry = rig.registry();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(registry.search(&query(""))).unwrap();
    rig.clock.advance(TTL * 2);
    let page = poll_without_runtime(registry.search(&query("")));
    assert_eq!(page.unwrap().freshness, Freshness::Cached);
    assert_eq!(rig.index_calls(), 1);
}

fn poll_without_runtime<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
    }
}

#[tokio::test]
async fn failed_refresh_keeps_the_cache_and_cools_down() {
    let rig = Rig::new();
    let registry = rig.registry();
    registry.search(&query("")).await.unwrap();
    rig.transport.get(INDEX, Reply::status(503, "down"));
    rig.clock.advance(TTL);

    let mut q = query("");
    q.read = ReadPolicy::RequireFresh;
    let page = registry.search(&q).await.unwrap();
    assert_eq!(page.freshness, Freshness::Cached);
    let error = page.sources[0].last_error.clone().unwrap();
    assert_eq!(error.kind, RegistryErrorKind::Unavailable);
    assert_eq!(rig.index_calls(), 2);

    registry.search(&q).await.unwrap();
    assert_eq!(rig.index_calls(), 2);

    rig.clock.advance(Duration::from_secs(61));
    rig.transport.get(INDEX, Reply::ok(FIXTURE));
    let page = registry.search(&q).await.unwrap();
    assert_eq!(page.freshness, Freshness::Live);
    assert!(page.sources[0].last_error.is_none());
    assert_eq!(rig.index_calls(), 3);
}

#[tokio::test]
async fn cooldown_bounds_an_extreme_retry_after() {
    let rig = Rig::new();
    rig.transport.get(
        INDEX,
        Reply::status(429, "slow down").header("Retry-After", "18446744073709551615"),
    );
    let registry = rig.registry();

    let error = registry.search(&query("")).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::RateLimited);
    rig.clock.advance(Duration::from_secs(3600));
    registry.search(&query("")).await.unwrap_err();
    assert_eq!(rig.index_calls(), 1);

    rig.clock.advance(Duration::from_secs(24 * 3600));
    rig.transport.get(INDEX, Reply::ok(FIXTURE));
    assert_eq!(
        registry.search(&query("")).await.unwrap().total,
        fixture_len()
    );
    assert_eq!(rig.index_calls(), 2);
}

#[tokio::test]
async fn cooldown_honours_retry_after() {
    let rig = Rig::new();
    rig.transport.get(
        INDEX,
        Reply::status(429, "slow down").header("Retry-After", "600"),
    );
    let registry = rig.registry();

    let error = registry.search(&query("")).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::RateLimited);
    assert_eq!(error.retry_after(), Some(Duration::from_secs(600)));

    rig.clock.advance(Duration::from_secs(120));
    let error = registry.search(&query("")).await.unwrap_err();
    assert_eq!(error.retry_after(), Some(Duration::from_secs(600)));
    assert_eq!(rig.index_calls(), 1);
    let statuses = registry.refresh(None, true).await.unwrap();
    assert_eq!(statuses[0].entry_count, 0);
    assert_eq!(rig.index_calls(), 1);
    let status = &registry.sources()[0];
    assert_eq!(status.freshness, None);
    assert_eq!(
        status.last_error.as_ref().unwrap().retry_after_secs,
        Some(600)
    );

    rig.clock.advance(Duration::from_secs(600));
    rig.transport.get(INDEX, Reply::ok(FIXTURE));
    assert_eq!(
        registry.search(&query("")).await.unwrap().total,
        fixture_len()
    );
    assert_eq!(rig.index_calls(), 2);
}

#[tokio::test]
async fn baseline_answers_when_nothing_is_cached() {
    let rig = Rig::new();
    rig.transport.get(INDEX, Reply::status(500, "boom"));
    let registry = rig
        .builder()
        .baseline(StaticSource::new(
            "library",
            "Packaged library",
            vec![library_entry("library/notes", "notes")],
        ))
        .build();

    let page = registry.search(&query("notes")).await.unwrap();
    assert_eq!(page.freshness, Freshness::LocalFallback);
    assert_eq!(page.items[0].registry, "library");
    assert_eq!(page.sources.len(), 2);
    assert!(page.sources[0].last_error.is_some());
    assert_eq!(page.sources[1].freshness, Some(Freshness::LocalFallback));

    let detail = registry.detail(&EntryKey::new("notes")).await.unwrap();
    assert_eq!(detail.summary.id, "library/notes");
    let error = registry
        .fetch_document(&EntryKey::new("library/notes"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::NoDirectDownload);
    assert_eq!(registry.sources().len(), 2);
}

#[tokio::test]
async fn no_cache_and_no_baseline_is_a_typed_error() {
    let rig = Rig::new();
    rig.transport.get(
        INDEX,
        Reply::Fail(TransportError::Connect("refused".to_owned())),
    );
    let registry = rig.registry();
    let error = registry.search(&query("")).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::Transport);
    assert!(error.is_unavailable());
    assert!(!error.is_timeout());
}

#[tokio::test]
async fn static_sources_search_beside_remote_ones() {
    let rig = Rig::new();
    let registry = rig
        .builder()
        .source(StaticSource::new(
            "library",
            "Packaged library",
            vec![library_entry("library/apple-notes-pro", "apple-notes-pro")],
        ))
        .source(HermesIndexSource::new(
            "hermes",
            "https://duplicate.test/ignored.json",
        ))
        .build();

    let page = registry.search(&query("apple-notes")).await.unwrap();
    assert_eq!(page.items[0].id, "apple-notes");
    assert_eq!(page.items[1].id, "library/apple-notes-pro");
    assert_eq!(page.freshness, Freshness::LocalFallback);
    assert_eq!(page.sources.len(), 2);

    let mut q = query("");
    q.registries = vec!["library".to_owned()];
    assert_eq!(registry.search(&q).await.unwrap().total, 1);
    assert_eq!(
        rig.transport.count("https://duplicate.test/ignored.json"),
        0
    );
}

#[tokio::test]
async fn refresh_and_warm_report_status() {
    let rig = Rig::new();
    let builder = rig.builder().ttl(TTL).featured(["x"]);
    assert!(format!("{builder:?}").contains("featured"));
    let registry = builder.build();
    assert!(format!("{registry:?}").contains("hermes"));
    assert_eq!(registry.sources()[0].freshness, None);
    assert_eq!(registry.sources()[0].label, "hermes");

    let statuses = registry.warm().await;
    assert_eq!(statuses[0].freshness, Some(Freshness::Live));
    assert_eq!(statuses[0].entry_count, fixture_len());
    assert!(!statuses[0].refreshing);

    registry.refresh(None, false).await.unwrap();
    assert_eq!(rig.index_calls(), 1);
    registry.refresh(Some("hermes"), true).await.unwrap();
    assert_eq!(rig.index_calls(), 2);
    let error = registry.refresh(Some("nope"), true).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UnknownRegistry);
}

#[tokio::test]
async fn max_entries_is_enforced() {
    let rig = Rig::new();
    let mut limits = RegistryLimits::default();
    limits.max_entries = 10;
    let registry = rig.builder().limits(limits).build();
    let error = registry.search(&query("")).await.unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "catalog entries",
            limit: 10
        }
    ));
}

#[tokio::test]
async fn catalog_over_the_byte_cap_is_refused() {
    let rig = Rig::new();
    let mut limits = RegistryLimits::default();
    limits.max_catalog_bytes = 1024;
    let registry = rig.builder().limits(limits).build();
    let error = registry.search(&query("")).await.unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "catalog",
            limit: 1024
        }
    ));

    rig.transport
        .get(INDEX, Reply::ok("{\"not\": \"an array\"}"));
    let registry = rig.builder().build();
    let error = registry.search(&query("")).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::Malformed);
}

#[tokio::test]
async fn file_store_persists_between_registries() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let first = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    first.search(&query("")).await.unwrap();
    assert!(dir.path().join("hermes.json").is_file());

    let second = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    let page = second.search(&query("")).await.unwrap();
    assert_eq!(page.total, fixture_len());
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(rig.index_calls(), 1);

    rig.clock.advance(TTL * 2);
    rig.transport
        .route(HttpMethod::Get, INDEX, vec![Reply::status(304, Vec::new())]);
    let third = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    let page = third.search(&query("")).await.unwrap();
    assert_eq!(page.freshness, Freshness::Cached);
    settle().await;
    assert_eq!(rig.index_calls(), 2);
    assert_eq!(
        rig.transport
            .requests()
            .last()
            .unwrap()
            .header("if-none-match"),
        Some("\"v1\"")
    );
    third.refresh(None, false).await.unwrap();
    assert_eq!(
        third.search(&query("")).await.unwrap().freshness,
        Freshness::Live
    );
}

#[tokio::test]
async fn not_modified_revalidation_persists_the_new_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let first = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    first.search(&query("")).await.unwrap();

    rig.clock.advance(TTL * 2);
    rig.transport
        .route(HttpMethod::Get, INDEX, vec![Reply::status(304, Vec::new())]);
    let second = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    second.refresh(None, false).await.unwrap();
    assert_eq!(rig.index_calls(), 2);

    let third = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    let page = third.search(&query("")).await.unwrap();
    assert_eq!(page.total, fixture_len());
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(page.fetched_at, Some(rig.clock.unix()));
    assert_eq!(rig.index_calls(), 2);
}

#[tokio::test]
async fn corrupt_store_falls_through_to_a_fetch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hermes.json"), b"{ not json").unwrap();
    let rig = Rig::new();
    let registry = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    let page = registry.search(&query("")).await.unwrap();
    assert_eq!(page.freshness, Freshness::Live);
    assert!(page.sources[0].last_error.is_none());
    assert_eq!(rig.index_calls(), 1);
}

#[tokio::test]
async fn unrepresentable_stored_timestamp_is_treated_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new();
    let seed = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    seed.search(&query("")).await.unwrap();
    let path = dir.path().join("hermes.json");
    let mut stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["fetched_at"] = serde_json::json!(u64::MAX);
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();

    let registry = rig
        .builder()
        .store(FileCatalogStore::new(dir.path()))
        .build();
    let page = registry.search(&query("")).await.unwrap();
    assert_eq!(page.total, fixture_len());
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(rig.index_calls(), 2);
}

#[tokio::test]
async fn store_write_failures_are_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, b"x").unwrap();
    let rig = Rig::new();
    let registry = rig
        .builder()
        .store(FileCatalogStore::new(blocker.join("store")))
        .build();
    let page = registry.search(&query("")).await.unwrap();
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(
        page.sources[0].last_error.as_ref().unwrap().kind,
        RegistryErrorKind::Store
    );
}

#[tokio::test]
async fn fetch_document_for_a_bundled_skill() {
    let rig = Rig::new();
    rig.transport.get(APPLE_NOTES_RAW, Reply::ok(SKILL_MD));
    let registry = rig.registry();

    let document = registry
        .fetch_document(&EntryKey::new("apple-notes"))
        .await
        .unwrap();
    assert_eq!(document.document.slug, "apple-notes");
    assert_eq!(document.flat.name, "apple-notes");
    assert_eq!(document.digest, document_digest(SKILL_MD));
    assert_eq!(document.fetched_from, APPLE_NOTES_RAW);
    assert!(!document.is_blocked());
    assert_eq!(document.entry.unwrap().id, "apple-notes");
}

#[tokio::test]
async fn blocked_documents_are_reported_not_refused() {
    let rig = Rig::new();
    rig.transport.get(
        APPLE_NOTES_RAW,
        Reply::ok("---\nname: apple-notes\ndescription: Notes\n---\nHidden\u{200b}text\n"),
    );
    let document = rig
        .registry()
        .fetch_document(&EntryKey::new("apple-notes"))
        .await
        .unwrap();
    assert!(document.is_blocked());
}

#[tokio::test]
async fn invalid_documents_are_typed() {
    let rig = Rig::new();
    rig.transport
        .get(APPLE_NOTES_RAW, Reply::ok("no frontmatter at all"));
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new("apple-notes"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::InvalidDocument);

    rig.transport
        .get(APPLE_NOTES_RAW, Reply::status(404, "gone"));
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new("apple-notes"))
        .await
        .unwrap_err();
    assert!(matches!(error, RegistryError::Unavailable { status: 404 }));
}

#[tokio::test]
async fn clawhub_conflict_is_upstream_ambiguous() {
    let rig = Rig::new();
    rig.transport
        .get(CLAWHUB_RAW, Reply::status(409, "conflict"));
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new("clawhub/apple-design"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::UpstreamAmbiguous { ref name } if name == "Apple Design")
    );
}

#[tokio::test]
async fn lobehub_entries_have_no_direct_download() {
    let rig = Rig::new();
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new("lobehub/9-somboon"))
        .await
        .unwrap_err();
    match error {
        RegistryError::NoDirectDownload { name, source_url } => {
            assert_eq!(name, "9-somboon");
            assert_eq!(
                source_url.as_deref(),
                Some("https://lobehub.com/agent/9-somboon")
            );
        }
        other => panic!("expected no direct download, got {other:?}"),
    }
}

const SKILLS_SH_ID: &str = "skills-sh/challengepost/learn-ai-basics/1-start";
const SKILLS_SH_URL: &str = "https://skills.sh/challengepost/learn-ai-basics/1-start";

#[tokio::test]
async fn skills_sh_probes_conventional_locations() {
    let rig = Rig::new();
    let skill = SkillsShRef::parse(SKILLS_SH_URL).unwrap();
    let candidates = skill.candidate_urls();
    rig.transport.route(
        HttpMethod::Head,
        &candidates[2],
        vec![Reply::ok(Vec::new())],
    );
    rig.transport.route(
        HttpMethod::Head,
        &candidates[3],
        vec![Reply::ok(Vec::new())],
    );
    rig.transport.get(
        &candidates[2],
        Reply::ok("---\nname: 1-start\ndescription: Start here\n---\nGo.\n"),
    );

    let document = rig
        .registry()
        .fetch_document(&EntryKey::new(SKILLS_SH_ID))
        .await
        .unwrap();
    assert_eq!(document.fetched_from, candidates[2]);
    let heads = rig
        .transport
        .requests()
        .into_iter()
        .filter(|r| r.method == HttpMethod::Head)
        .count();
    assert_eq!(heads, candidates.len());
}

#[tokio::test]
async fn skills_sh_falls_back_to_one_tree_listing() {
    let rig = Rig::new();
    let skill = SkillsShRef::parse(SKILLS_SH_URL).unwrap();
    let tree = serde_json::json!({
        "truncated": false,
        "tree": [{"type": "blob", "path": "lessons/1-start/SKILL.md"}]
    });
    rig.transport
        .get(&skill.tree_api_url(), Reply::ok(tree.to_string()));
    let located = skill.raw_url("lessons/1-start/SKILL.md").unwrap();
    rig.transport.get(
        &located,
        Reply::ok("---\nname: 1-start\ndescription: Start here\n---\nGo.\n"),
    );
    let document = rig
        .registry()
        .fetch_document(&EntryKey::new(SKILLS_SH_ID))
        .await
        .unwrap();
    assert_eq!(document.fetched_from, located);

    let ambiguous = serde_json::json!({
        "tree": [
            {"type": "blob", "path": "a/1-start/SKILL.md"},
            {"type": "blob", "path": "b/1-start/SKILL.md"}
        ]
    });
    rig.transport
        .get(&skill.tree_api_url(), Reply::ok(ambiguous.to_string()));
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new(SKILLS_SH_ID))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UpstreamAmbiguous);

    rig.transport
        .get(&skill.tree_api_url(), Reply::ok("{\"tree\": []}"));
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new(SKILLS_SH_ID))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::NoDirectDownload);

    rig.transport
        .get(&skill.tree_api_url(), Reply::ok("not json"));
    let error = rig
        .registry()
        .fetch_document(&EntryKey::new(SKILLS_SH_ID))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::Malformed);
}

#[tokio::test]
async fn skills_sh_tree_listing_is_size_capped() {
    let rig = Rig::new();
    let skill = SkillsShRef::parse(SKILLS_SH_URL).unwrap();
    rig.transport
        .get(&skill.tree_api_url(), Reply::ok(vec![b' '; 4096]));
    let mut limits = RegistryLimits::default();
    limits.max_tree_listing_bytes = 1024;
    let error = rig
        .builder()
        .limits(limits)
        .build()
        .fetch_document(&EntryKey::new(SKILLS_SH_ID))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "tree listing",
            limit: 1024
        }
    ));
}

async fn ad_hoc(rig: &Rig, url: &str) -> Result<tinyskills::RegistryDocument, RegistryError> {
    let mut timeouts = RegistryTimeouts::default();
    timeouts.document = Duration::from_secs(15);
    ad_hoc_with(
        rig,
        url,
        &FetchPolicy::default(),
        &timeouts,
        &RegistryLimits::default(),
    )
    .await
}

async fn ad_hoc_with(
    rig: &Rig,
    url: &str,
    policy: &FetchPolicy,
    timeouts: &RegistryTimeouts,
    limits: &RegistryLimits,
) -> Result<tinyskills::RegistryDocument, RegistryError> {
    fetch_skill_document(
        Arc::clone(&rig.transport) as Arc<dyn tinyskills::RegistryTransport>,
        Arc::clone(&rig.resolver) as Arc<dyn tinyskills::Resolver>,
        url,
        policy,
        timeouts,
        limits,
    )
    .await
}

#[tokio::test]
async fn ad_hoc_fetch_normalizes_github_blob_links() {
    let rig = Rig::new();
    rig.transport.get(
        "https://raw.githubusercontent.com/o/r/main/skills/x/SKILL.md",
        Reply::ok(SKILL_MD),
    );
    let document = ad_hoc(&rig, " https://github.com/o/r/blob/main/skills/x/SKILL.md ")
        .await
        .unwrap();
    assert_eq!(document.entry, None);
    assert_eq!(document.document.frontmatter.name, "apple-notes");
    let request = rig.transport.requests().pop().unwrap();
    assert_eq!(
        request.pinned,
        vec![std::net::SocketAddr::new(PUBLIC_IP, 443)]
    );
    assert_eq!(rig.resolver.lookups(), vec!["raw.githubusercontent.com"]);

    let error = ad_hoc(&rig, "https://github.com/o/r").await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UnsafeUrl);
}

#[tokio::test]
async fn redirects_are_followed_and_revalidated() {
    let rig = Rig::new();
    rig.resolver
        .answer("mirror.test", vec![IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4))]);
    rig.transport.get(
        "https://docs.test/SKILL.md",
        Reply::redirect(301, "https://mirror.test/skills/SKILL.md"),
    );
    rig.transport.get(
        "https://mirror.test/skills/SKILL.md",
        Reply::redirect(302, "../other/SKILL.md"),
    );
    rig.transport
        .get("https://mirror.test/other/SKILL.md", Reply::ok(SKILL_MD));
    let document = ad_hoc(&rig, "https://docs.test/SKILL.md").await.unwrap();
    assert_eq!(document.fetched_from, "https://mirror.test/other/SKILL.md");
    let requests = rig.transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].pinned,
        vec!["8.8.4.4:443".parse::<std::net::SocketAddr>().unwrap()]
    );
}

#[tokio::test]
async fn unsafe_redirect_targets_are_refused() {
    let rig = Rig::new();
    rig.resolver
        .answer("private.test", vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))]);
    rig.resolver.answer(
        "rebind.test",
        vec![PUBLIC_IP, IpAddr::V4(Ipv4Addr::LOCALHOST)],
    );
    rig.resolver.answer("nxdomain.test", Vec::new());
    for target in [
        "http://mirror.test/SKILL.md",
        "https://127.0.0.1/SKILL.md",
        "https://[::1]/SKILL.md",
        "https://private.test/SKILL.md",
        "https://rebind.test/SKILL.md",
        "https://localhost/SKILL.md",
    ] {
        rig.transport
            .get("https://docs.test/SKILL.md", Reply::redirect(307, target));
        let error = ad_hoc(&rig, "https://docs.test/SKILL.md")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), RegistryErrorKind::UnsafeUrl, "{target}");
        assert_eq!(rig.transport.count(target), 0, "{target}");
    }

    rig.transport.get(
        "https://docs.test/SKILL.md",
        Reply::redirect(307, "https://nxdomain.test/SKILL.md"),
    );
    let error = ad_hoc(&rig, "https://docs.test/SKILL.md")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::Transport);

    rig.transport
        .get("https://docs.test/SKILL.md", Reply::status(301, Vec::new()));
    let error = ad_hoc(&rig, "https://docs.test/SKILL.md")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::Malformed);
}

#[tokio::test]
async fn redirect_chains_are_bounded() {
    let rig = Rig::new();
    for hop in 0..6 {
        rig.transport.get(
            &format!("https://hop{hop}.test/SKILL.md"),
            Reply::redirect(308, &format!("https://hop{}.test/SKILL.md", hop + 1)),
        );
    }
    rig.transport
        .get("https://hop5.test/SKILL.md", Reply::ok(SKILL_MD));
    ad_hoc(&rig, "https://hop0.test/SKILL.md").await.unwrap();

    rig.transport.get(
        "https://hop5.test/SKILL.md",
        Reply::redirect(308, "https://hop6.test/SKILL.md"),
    );
    let error = ad_hoc(&rig, "https://hop0.test/SKILL.md")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "redirect chain",
            limit: 5
        }
    ));
}

#[tokio::test]
async fn document_bodies_are_capped() {
    let rig = Rig::new();
    let url = "https://docs.test/SKILL.md";
    rig.transport
        .get(url, Reply::ok(SKILL_MD).header("Content-Length", "2000000"));
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "document",
            ..
        }
    ));

    rig.transport.get(
        url,
        Reply::ok(Vec::new()).chunked(vec![vec![b'a'; 700_000], vec![b'b'; 700_000]]),
    );
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert!(
        matches!(error, RegistryError::TooLarge { what: "document", limit } if limit == 1024 * 1024)
    );

    rig.transport
        .get(url, Reply::status(503, "x").header("Retry-After", "7"));
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert_eq!(error.retry_after(), Some(Duration::from_secs(7)));
    rig.transport.get(url, Reply::status(503, "x"));
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert!(matches!(error, RegistryError::Unavailable { status: 503 }));
}

#[tokio::test]
async fn empty_chunks_do_not_end_a_document_body() {
    let rig = Rig::new();
    let url = "https://docs.test/SKILL.md";
    rig.transport.get(
        url,
        Reply::ok(Vec::new()).chunked(vec![vec![], SKILL_MD.as_bytes().to_vec(), vec![]]),
    );
    let document = ad_hoc(&rig, url).await.unwrap();
    assert_eq!(document.document.frontmatter.name, "apple-notes");
    assert_eq!(document.digest, document_digest(SKILL_MD));
}

#[tokio::test(start_paused = true)]
async fn operations_time_out_with_typed_errors() {
    let rig = Rig::new();
    let url = "https://docs.test/SKILL.md";
    rig.transport.get(url, Reply::Hang);
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert!(
        matches!(error, RegistryError::Timeout { operation: "document", budget } if budget == Duration::from_secs(15))
    );
    assert!(error.is_timeout());
    assert_eq!(error.to_string(), "document timed out after 15s");

    rig.transport.get(url, Reply::Fail(TransportError::Timeout));
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::Timeout);

    let mut timeouts = RegistryTimeouts::default();
    timeouts.catalog = Duration::from_secs(2);
    rig.transport
        .get(INDEX, Reply::ok(FIXTURE).delayed(Duration::from_secs(3)));
    let error = rig
        .builder()
        .timeouts(timeouts)
        .build()
        .search(&query(""))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RegistryError::Timeout {
            operation: "catalog",
            ..
        }
    ));
}

#[tokio::test]
async fn a_response_for_another_url_breaks_the_contract() {
    let rig = Rig::new();
    let url = "https://docs.test/SKILL.md";
    rig.transport.get(
        url,
        Reply::ok(SKILL_MD).answering_for("https://elsewhere.test/SKILL.md?token=secret"),
    );
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::TransportContract);
    assert!(!error.to_string().contains("secret"));
}

#[tokio::test]
async fn loopback_http_needs_the_policy() {
    let rig = Rig::new();
    let url = "http://127.0.0.1:8080/SKILL.md";
    rig.transport.get(url, Reply::ok(SKILL_MD));
    let error = ad_hoc(&rig, url).await.unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UnsafeUrl);

    let mut policy = FetchPolicy::default();
    policy.allow_loopback_http = true;
    let timeouts = RegistryTimeouts::default();
    let limits = RegistryLimits::default();
    ad_hoc_with(&rig, url, &policy, &timeouts, &limits)
        .await
        .unwrap();

    rig.resolver
        .answer("localhost", vec![IpAddr::V4(Ipv4Addr::new(10, 1, 1, 1))]);
    let error = ad_hoc_with(
        &rig,
        "http://localhost:8080/SKILL.md",
        &policy,
        &timeouts,
        &limits,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), RegistryErrorKind::UnsafeUrl);
}
