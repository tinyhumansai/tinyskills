//! Serializable request and response types a host forwards over its own RPC.
//!
//! Field names are `snake_case`. Additions are additive: every struct
//! deserializes with defaults for fields it does not see, so a newer host can
//! read an older payload. [`REGISTRY_CONTRACT_VERSION`] changes major version
//! only for a breaking change.

use serde::{Deserialize, Serialize};

use super::error::{RegistryError, RegistryErrorKind};
use super::transport::find_header;
use crate::CatalogEntry;

/// The contract version, `(major, minor)`.
pub const REGISTRY_CONTRACT_VERSION: (u16, u16) = (1, 0);

/// Whether a peer speaking `version` can exchange these types with this
/// crate: the major versions match.
#[must_use]
pub fn is_registry_contract_compatible(version: (u16, u16)) -> bool {
    version.0 == REGISTRY_CONTRACT_VERSION.0
}

/// One catalog entry with the presentation fields a source publishes beside
/// the [`CatalogEntry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RegistryEntry {
    /// The entry as every other catalog API sees it.
    pub entry: CatalogEntry,
    /// Longer description, when the source has one.
    pub overview: String,
    /// Display label for the category.
    pub category_label: Option<String>,
    /// Whether the source pins the category rather than inferring it.
    pub fixed_category: Option<bool>,
    /// The identifier the source's own installer uses.
    pub install_identifier: Option<String>,
}

impl RegistryEntry {
    /// An entry with no presentation fields.
    #[must_use]
    pub fn new(entry: CatalogEntry) -> Self {
        Self {
            entry,
            overview: String::new(),
            category_label: None,
            fixed_category: None,
            install_identifier: None,
        }
    }
}

impl Default for RegistryEntry {
    fn default() -> Self {
        Self::new(CatalogEntry {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            source: String::new(),
            category: String::new(),
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
}

impl PartialEq for RegistryEntry {
    fn eq(&self, other: &Self) -> bool {
        self.overview == other.overview
            && self.category_label == other.category_label
            && self.fixed_category == other.fixed_category
            && self.install_identifier == other.install_identifier
            && serde_json::to_value(&self.entry).ok() == serde_json::to_value(&other.entry).ok()
    }
}

/// How fresh the data behind a response is. Ordered best first, so the
/// maximum over several sources is the page's freshness.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Freshness {
    /// Fetched within the time-to-live.
    #[default]
    Live,
    /// Older than the time-to-live, or kept after a failed refresh.
    Cached,
    /// From a host-supplied local source or baseline.
    LocalFallback,
}

/// Whether a read may be answered from a stale catalog.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReadPolicy {
    /// Answer from a stale catalog, marked [`Freshness::Cached`], and refresh
    /// it in the background.
    #[default]
    AllowStale,
    /// Refresh a stale catalog before answering.
    RequireFresh,
}

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillQuery {
    /// Case-insensitive text matched against name, description, tags,
    /// category and author. Empty matches everything.
    pub text: String,
    /// Registry ids to search; empty searches every source.
    pub registries: Vec<String>,
    /// Upstream sources to keep (e.g. `ClawHub`), matched case-insensitively.
    pub upstreams: Vec<String>,
    /// Categories to keep, matched case-insensitively.
    pub categories: Vec<String>,
    /// Keep only entries with a direct `SKILL.md` download.
    pub installable_only: bool,
    /// 1-based page number; 0 is read as 1.
    pub page: usize,
    /// Entries per page, clamped to `1..=max_page_size`.
    pub page_size: usize,
    /// Whether a stale catalog may answer.
    pub read: ReadPolicy,
}

impl SkillQuery {
    /// Default entries per page.
    pub const DEFAULT_PAGE_SIZE: usize = 25;

    /// A first-page query for `text`.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

impl Default for SkillQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            registries: Vec::new(),
            upstreams: Vec::new(),
            categories: Vec::new(),
            installable_only: false,
            page: 1,
            page_size: Self::DEFAULT_PAGE_SIZE,
            read: ReadPolicy::AllowStale,
        }
    }
}

/// One search hit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillSummary {
    /// The registry (source id) the entry came from.
    pub registry: String,
    /// The entry id within the registry.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Upstream source within the registry.
    pub upstream: String,
    /// Category value.
    pub category: String,
    /// Category display label.
    pub category_label: Option<String>,
    /// Author, when known.
    pub author: Option<String>,
    /// Version, when declared.
    pub version: Option<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// Platform hints.
    pub platforms: Vec<String>,
    /// Whether the entry has a direct `SKILL.md` download.
    pub installable: bool,
    /// Human-facing page for the entry.
    pub source_url: Option<String>,
}

impl SkillSummary {
    pub(crate) fn from_entry(registry: &str, entry: &RegistryEntry) -> Self {
        let catalog = &entry.entry;
        Self {
            registry: registry.to_owned(),
            id: catalog.id.clone(),
            name: catalog.name.clone(),
            description: catalog.description.clone(),
            upstream: catalog.source.clone(),
            category: catalog.category.clone(),
            category_label: entry.category_label.clone(),
            author: catalog.author.clone(),
            version: catalog.version.clone(),
            tags: catalog.tags.clone(),
            platforms: catalog.platforms.clone(),
            installable: catalog.has_direct_download(),
            source_url: catalog.source_url.clone(),
        }
    }
}

/// Everything the registry knows about one entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillDetail {
    /// The search-hit fields.
    pub summary: SkillSummary,
    /// Longer description.
    pub overview: String,
    /// Software license.
    pub license: Option<String>,
    /// Required CLI commands.
    pub commands: Vec<String>,
    /// Required environment variables.
    pub env_vars: Vec<String>,
    /// Docs path in the source catalog.
    pub docs_path: Option<String>,
    /// The derived `SKILL.md` URL; empty when there is none.
    pub download_url: String,
    /// The identifier the source's own installer uses.
    pub install_identifier: Option<String>,
}

impl SkillDetail {
    pub(crate) fn from_entry(registry: &str, entry: &RegistryEntry) -> Self {
        let catalog = &entry.entry;
        Self {
            summary: SkillSummary::from_entry(registry, entry),
            overview: entry.overview.clone(),
            license: catalog.license.clone(),
            commands: catalog.commands.clone(),
            env_vars: catalog.env_vars.clone(),
            docs_path: catalog.docs_path.clone(),
            download_url: catalog.download_url.clone(),
            install_identifier: entry.install_identifier.clone(),
        }
    }
}

/// A page of search results.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillPage {
    /// The hits on this page.
    pub items: Vec<SkillSummary>,
    /// The 1-based page number served.
    pub page: usize,
    /// Entries per page after clamping.
    pub page_size: usize,
    /// Matching entries across all pages.
    pub total: usize,
    /// Number of pages; 0 when nothing matched.
    pub total_pages: usize,
    /// The worst freshness among the catalogs that answered.
    pub freshness: Freshness,
    /// Unix seconds of the oldest catalog fetch that answered.
    pub fetched_at: Option<u64>,
    /// Status of every source the query selected.
    pub sources: Vec<SourceStatus>,
}

/// A summary of a stored failure, safe to show.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RegistryErrorSummary {
    /// The stable kind.
    pub kind: RegistryErrorKind,
    /// The error message.
    pub message: String,
    /// The delay a throttling upstream asked for, in seconds.
    pub retry_after_secs: Option<u64>,
}

impl RegistryErrorSummary {
    pub(crate) fn of(error: &RegistryError) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
            retry_after_secs: error.retry_after().map(|delay| delay.as_secs()),
        }
    }
}

/// The state of one source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SourceStatus {
    /// The registry id.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Freshness of the catalog held, or `None` when none is held.
    pub freshness: Option<Freshness>,
    /// Entries held.
    pub entry_count: usize,
    /// Upstream items the last load dropped (no name, not an object).
    pub skipped: usize,
    /// Unix seconds of the catalog's fetch.
    pub fetched_at: Option<u64>,
    /// The last refresh or persistence failure, cleared by a success.
    pub last_error: Option<RegistryErrorSummary>,
    /// Whether a refresh is running now.
    pub refreshing: bool,
}

/// Identifies one entry: by id within a registry, or by id (or unique name)
/// across every registry when `registry` is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct EntryKey {
    /// The registry id, or `None` for every registry.
    pub registry: Option<String>,
    /// The entry id, or a name carried by exactly one entry.
    pub id: String,
}

impl EntryKey {
    /// A key searched across every registry.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            registry: None,
            id: id.into(),
        }
    }

    /// A key within one registry.
    #[must_use]
    pub fn in_registry(registry: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            registry: Some(registry.into()),
            id: id.into(),
        }
    }
}

/// One facet value and how many entries carry it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Facet {
    /// The value a query filters on.
    pub value: String,
    /// Display label.
    pub label: String,
    /// Entries carrying the value.
    pub count: usize,
}

/// The filter values available in the selected registries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RegistryFacets {
    /// Upstream sources, most entries first.
    pub upstreams: Vec<Facet>,
    /// Categories, most entries first.
    pub categories: Vec<Facet>,
    /// The worst freshness among the catalogs that answered.
    pub freshness: Freshness,
}

/// HTTP cache validators from the last successful catalog fetch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Validators {
    /// The `ETag` header.
    pub etag: Option<String>,
    /// The `Last-Modified` header.
    pub last_modified: Option<String>,
}

impl Validators {
    /// Validators read from response headers.
    #[must_use]
    pub fn from_headers(headers: &[(String, String)]) -> Self {
        Self {
            etag: find_header(headers, "etag").map(str::to_owned),
            last_modified: find_header(headers, "last-modified").map(str::to_owned),
        }
    }

    /// The conditional request headers these validators produce.
    #[must_use]
    pub fn conditional_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let Some(etag) = &self.etag {
            headers.push(("If-None-Match".to_owned(), etag.clone()));
        }
        if let Some(modified) = &self.last_modified {
            headers.push(("If-Modified-Since".to_owned(), modified.clone()));
        }
        headers
    }
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
