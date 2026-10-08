#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn accepts_the_clawhub_file_api() {
    for url in [
        "https://clawhub.ai/api/v1/skills/apple-design/file?path=SKILL.md",
        "https://clawhub.ai/api/v1/skills/x/file?path=docs/README.MD",
    ] {
        assert_eq!(normalize_registry_document_url(url).unwrap(), url);
    }
}

#[test]
fn other_clawhub_shapes_fall_through_and_fail() {
    for url in [
        "https://clawhub.ai/api/v1/skills/x/file?path=run.sh",
        "https://clawhub.ai/api/v1/skills/x/file",
        "https://clawhub.ai/api/v1/skills/x/other?path=SKILL.md",
        "http://clawhub.ai/api/v1/skills/x/file?path=SKILL.md",
        "https://clawhub.ai:8443/api/v1/skills/x/file?path=SKILL.md",
        "https://evil.test/api/v1/skills/x/file?path=SKILL.md",
    ] {
        assert!(normalize_registry_document_url(url).is_err(), "{url}");
    }
}

#[test]
fn delegates_everything_else_unchanged() {
    for url in [
        "https://github.com/o/r/blob/main/s/SKILL.md",
        "https://raw.githubusercontent.com/o/r/main/SKILL.md",
        "https://github.com/o/r/tree/main/s",
        "not a url",
        "https://example.test/a/../SKILL.md",
    ] {
        assert_eq!(
            normalize_registry_document_url(url),
            crate::normalize_install_url(url),
            "{url}"
        );
    }
}
