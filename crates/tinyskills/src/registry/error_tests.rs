#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn every_error() -> Vec<RegistryError> {
    vec![
        RegistryError::Timeout {
            operation: "catalog",
            budget: Duration::from_secs(3),
        },
        RegistryError::Unavailable { status: 502 },
        RegistryError::RateLimited {
            retry_after: Some(Duration::from_secs(9)),
        },
        RegistryError::RateLimited { retry_after: None },
        RegistryError::TooLarge {
            what: "document",
            limit: 7,
        },
        RegistryError::Malformed {
            what: "catalog",
            detail: "eof".to_owned(),
        },
        RegistryError::NotFound {
            id: "x".to_owned(),
            closest: vec![],
        },
        RegistryError::NotFound {
            id: "x".to_owned(),
            closest: vec!["y".to_owned()],
        },
        RegistryError::Ambiguous {
            name: "n".to_owned(),
            count: 2,
            ids: vec!["a".to_owned(), "b".to_owned()],
        },
        RegistryError::UpstreamAmbiguous {
            name: "n".to_owned(),
        },
        RegistryError::NoDirectDownload {
            name: "n".to_owned(),
            source_url: Some("https://l.test/a".to_owned()),
        },
        RegistryError::UnsafeUrl(InstallError::InvalidUrl {
            input: "https://h.test/?token=secret".to_owned(),
            message: "bad".to_owned(),
        }),
        RegistryError::UnsafeUrl(InstallError::MissingHost(
            "file:///?token=secret".to_owned(),
        )),
        RegistryError::UnsafeUrl(InstallError::EmptyUrl),
        RegistryError::UnsafeUrl(InstallError::UrlTooLong { len: 3, max: 2 }),
        RegistryError::UnsafeUrl(InstallError::UnsupportedUrl("u".to_owned())),
        RegistryError::UnsafeUrl(InstallError::UnsupportedScheme("ftp".to_owned())),
        RegistryError::UnsafeUrl(InstallError::UnsafeHost {
            host: "localhost".to_owned(),
        }),
        RegistryError::UnsafeUrl(InstallError::EmptySlug),
        RegistryError::UnsafeUrl(InstallError::SlugTooLong { max: 1 }),
        RegistryError::UnsafeUrl(InstallError::DnsLookup {
            host: "h".to_owned(),
            message: "m".to_owned(),
        }),
        RegistryError::UnsafeUrl(InstallError::NoAddresses {
            host: "h".to_owned(),
        }),
        RegistryError::UnsafeUrl(InstallError::NonPublicAddress {
            host: "h".to_owned(),
            address: "10.0.0.1".parse().unwrap(),
        }),
        RegistryError::InvalidDocument(DocumentError::TooLarge { size: 2, limit: 1 }),
        RegistryError::InvalidDocument(DocumentError::UnterminatedFrontmatter),
        RegistryError::InvalidDocument(DocumentError::MissingField("name")),
        RegistryError::InvalidDocument(DocumentError::Slug(InstallError::EmptySlug)),
        RegistryError::UnknownRegistry { id: "r".to_owned() },
        RegistryError::Store(StoreError::Io("disk".to_owned())),
        RegistryError::TransportContract {
            detail: "d".to_owned(),
        },
        RegistryError::Transport(TransportError::Timeout),
        RegistryError::Transport(TransportError::Connect("c".to_owned())),
    ]
}

#[test]
fn kinds_are_stable_and_serialize_as_their_names() {
    for error in every_error() {
        let kind = error.kind();
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            serde_json::Value::String(kind.as_str().to_owned())
        );
        let back: RegistryErrorKind =
            serde_json::from_value(serde_json::to_value(kind).unwrap()).unwrap();
        assert_eq!(back, kind);
        assert_ne!(error.to_string().len(), 0);
    }
}

#[test]
fn duplicate_preserves_kind_and_message() {
    for error in every_error() {
        let copy = error.duplicate();
        assert_eq!(copy.kind(), error.kind());
        assert_eq!(copy.to_string(), error.to_string());
    }
}

#[test]
fn invalid_utf8_duplicates_as_malformed() {
    let utf8 = String::from_utf8(vec![0xff]).unwrap_err();
    let error = RegistryError::InvalidDocument(DocumentError::InvalidUtf8(utf8));
    let copy = error.duplicate();
    assert_eq!(copy.kind(), RegistryErrorKind::Malformed);
    assert!(copy.to_string().contains("utf-8"));
}

#[test]
fn messages_redact_url_inputs() {
    for error in every_error() {
        assert!(!error.to_string().contains("secret"), "{error}");
    }
}

#[test]
fn helpers_classify() {
    let timeout = RegistryError::Timeout {
        operation: "document",
        budget: Duration::from_secs(15),
    };
    assert!(timeout.is_timeout() && timeout.is_unavailable());
    assert_eq!(timeout.to_string(), "document timed out after 15s");
    assert!(RegistryError::Transport(TransportError::Timeout).is_timeout());
    assert!(RegistryError::Unavailable { status: 500 }.is_unavailable());
    assert!(!RegistryError::UnknownRegistry { id: "x".to_owned() }.is_unavailable());
    let limited = RegistryError::RateLimited {
        retry_after: Some(Duration::from_secs(4)),
    };
    assert_eq!(limited.retry_after(), Some(Duration::from_secs(4)));
    assert_eq!(limited.to_string(), "rate limited: retry after 4s");
    assert!(limited.is_unavailable());
    assert_eq!(RegistryError::Unavailable { status: 1 }.retry_after(), None);
    assert_eq!(
        RegistryError::RateLimited { retry_after: None }.to_string(),
        "rate limited: retry shortly"
    );
}
