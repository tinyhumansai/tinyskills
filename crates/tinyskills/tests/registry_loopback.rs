//! The registry over a real loopback socket: chunked bodies, redirects and a
//! slow upstream, through a minimal socket transport.

#![cfg(feature = "registry")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/loopback.rs"]
mod loopback;

use std::sync::Arc;
use std::time::Duration;

use loopback::{Script, Server, SocketTransport};
use tinyskills::{
    FetchPolicy, Freshness, HermesIndexSource, RegistryError, RegistryLimits, RegistryTimeouts,
    SkillQuery, SkillRegistry, SystemResolver, fetch_skill_document,
};

const FIXTURE: &str = include_str!("fixtures/hermes/skills-sample.json");
const SKILL_MD: &str = "---\nname: loopback\ndescription: Served locally\n---\nBody.\n";

fn loopback_policy() -> FetchPolicy {
    let mut policy = FetchPolicy::default();
    policy.allow_loopback_http = true;
    policy
}

fn fixture_len() -> usize {
    serde_json::from_str::<Vec<serde_json::Value>>(FIXTURE)
        .unwrap()
        .len()
}

async fn fetch(
    transport: &Arc<SocketTransport>,
    url: &str,
    timeouts: &RegistryTimeouts,
    limits: &RegistryLimits,
) -> Result<tinyskills::RegistryDocument, RegistryError> {
    fetch_skill_document(
        Arc::clone(transport) as Arc<dyn tinyskills::RegistryTransport>,
        Arc::new(SystemResolver),
        url,
        &loopback_policy(),
        timeouts,
        limits,
    )
    .await
}

#[tokio::test]
async fn chunked_catalog_behind_a_redirect() {
    let pieces: Vec<Vec<u8>> = FIXTURE
        .as_bytes()
        .chunks(7_919)
        .map(<[u8]>::to_vec)
        .collect();
    let server = Server::start(vec![
        ("/old.json", Script::Redirect("/skills.json".to_owned())),
        ("/skills.json", Script::Chunked(pieces)),
    ]);
    let transport = Arc::new(SocketTransport::default());
    let registry = SkillRegistry::builder(Arc::clone(&transport))
        .policy(loopback_policy())
        .source(HermesIndexSource::new("local", server.url("/old.json")))
        .build();

    let page = registry.search(&SkillQuery::default()).await.unwrap();
    assert_eq!(page.total, fixture_len());
    assert_eq!(page.freshness, Freshness::Live);
    assert_eq!(server.seen(), ["GET /old.json", "GET /skills.json"]);
    for pinned in transport.pinned_seen.lock().unwrap().iter() {
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].ip().is_loopback());
    }
}

#[tokio::test]
async fn documents_over_the_cap_are_aborted() {
    let server = Server::start(vec![
        ("/chunked.md", Script::Chunked(vec![vec![b'a'; 600]; 4])),
        (
            "/declared.md",
            Script::Plain {
                status: 200,
                headers: vec![("Content-Length".to_owned(), "999999".to_owned())],
                body: Vec::new(),
            },
        ),
        (
            "/ok.md",
            Script::Plain {
                status: 200,
                headers: Vec::new(),
                body: SKILL_MD.as_bytes().to_vec(),
            },
        ),
    ]);
    let transport = Arc::new(SocketTransport::default());
    let timeouts = RegistryTimeouts::default();
    let mut limits = RegistryLimits::default();
    limits.max_document_bytes = 1024;

    let error = fetch(&transport, &server.url("/chunked.md"), &timeouts, &limits)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "document",
            limit: 1024
        }
    ));
    let error = fetch(&transport, &server.url("/declared.md"), &timeouts, &limits)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RegistryError::TooLarge {
            what: "document",
            limit: 1024
        }
    ));

    let document = fetch(&transport, &server.url("/ok.md"), &timeouts, &limits)
        .await
        .unwrap();
    assert_eq!(document.flat.name, "loopback");
    assert_eq!(document.fetched_from, server.url("/ok.md"));

    let error = fetch(&transport, &server.url("/missing.md"), &timeouts, &limits)
        .await
        .unwrap_err();
    assert!(matches!(error, RegistryError::Unavailable { status: 404 }));
}

#[tokio::test]
async fn an_oversized_response_head_is_refused() {
    let server = Server::start(vec![(
        "/big.md",
        Script::Plain {
            status: 200,
            headers: vec![("X-Pad".to_owned(), "a".repeat(66_000))],
            body: SKILL_MD.as_bytes().to_vec(),
        },
    )]);
    let transport = Arc::new(SocketTransport::default());
    let error = fetch(
        &transport,
        &server.url("/big.md"),
        &RegistryTimeouts::default(),
        &RegistryLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, RegistryError::Transport(_)), "{error:?}");
}

#[tokio::test]
async fn a_slow_upstream_hits_the_document_budget() {
    let server = Server::start(vec![(
        "/slow.md",
        Script::Drip {
            body: SKILL_MD.as_bytes().to_vec(),
            every: Duration::from_millis(50),
        },
    )]);
    let transport = Arc::new(SocketTransport::default());
    let mut timeouts = RegistryTimeouts::default();
    timeouts.document = Duration::from_millis(300);
    let error = fetch(
        &transport,
        &server.url("/slow.md"),
        &timeouts,
        &RegistryLimits::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        RegistryError::Timeout {
            operation: "document",
            ..
        }
    ));
}
