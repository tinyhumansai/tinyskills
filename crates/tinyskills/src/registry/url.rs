//! Normalization of a registry document URL.

use crate::{InstallError, normalize_install_url};

/// Normalize a `SKILL.md` URL from a registry entry or a user.
///
/// `ClawHub`'s file API (`https://clawhub.ai/api/v1/skills/<slug>/file?path=<file>.md`)
/// names the Markdown file in its query, so it is accepted as is when `path`
/// ends in `.md`. Every other URL goes through [`normalize_install_url`]
/// unchanged.
///
/// # Errors
///
/// Returns the [`normalize_install_url`] error for any other URL it refuses.
pub fn normalize_registry_document_url(raw: &str) -> Result<String, InstallError> {
    if let Ok(url) = ::url::Url::parse(raw) {
        let is_clawhub_file_api = url.scheme() == "https"
            && url.host_str() == Some("clawhub.ai")
            && url.port().is_none()
            && url.path().starts_with("/api/v1/skills/")
            && url.path().ends_with("/file");
        if is_clawhub_file_api
            && url
                .query_pairs()
                .find(|(key, _)| key == "path")
                .is_some_and(|(_, value)| value.to_ascii_lowercase().ends_with(".md"))
        {
            return Ok(raw.to_owned());
        }
    }
    normalize_install_url(raw)
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod tests;
