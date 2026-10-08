//! Typed registry failures and their stable kinds.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::transport::TransportError;
use crate::{DocumentError, InstallError};

/// Why a registry operation failed.
///
/// Messages never contain a full URL; a host shows them as they are.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// An operation did not finish within its budget.
    #[error("{operation} timed out after {}s", budget.as_secs())]
    Timeout {
        /// What timed out, e.g. `catalog`, `document`.
        operation: &'static str,
        /// The budget that ran out.
        budget: Duration,
    },
    /// The upstream answered with an unusable status.
    #[error("upstream returned status {status}")]
    Unavailable {
        /// The HTTP status.
        status: u16,
    },
    /// The upstream is throttling requests.
    #[error("{}", rate_limited_message(*.retry_after))]
    RateLimited {
        /// The delay the upstream asked for, when it sent one.
        retry_after: Option<Duration>,
    },
    /// A body, a collection or a chain exceeded its limit.
    #[error("{what} exceeds the limit of {limit}")]
    TooLarge {
        /// What was too large.
        what: &'static str,
        /// The limit, in the unit of `what`.
        limit: u64,
    },
    /// An upstream body could not be read as expected.
    #[error("malformed {what}: {detail}")]
    Malformed {
        /// What was malformed.
        what: &'static str,
        /// Parser diagnostic.
        detail: String,
    },
    /// No entry has the requested id or a unique matching name.
    #[error("no catalog entry has id '{id}'. {}", not_found_hint(.closest))]
    NotFound {
        /// The requested id.
        id: String,
        /// Up to five real ids that resemble it, best first.
        closest: Vec<String>,
    },
    /// Several entries carry the requested name.
    #[error(
        "{count} catalog entries are named '{name}'; use one of their ids, e.g. {}.",
        .ids.join(", ")
    )]
    Ambiguous {
        /// The requested name.
        name: String,
        /// How many entries carry it.
        count: usize,
        /// Up to five of their ids.
        ids: Vec<String>,
    },
    /// The upstream holds several skills for this entry and the catalog does
    /// not say which one it lists.
    #[error(
        "'{name}' matches more than one upstream skill and the catalog does not say which one, so it cannot be fetched automatically"
    )]
    UpstreamAmbiguous {
        /// The entry's display name.
        name: String,
    },
    /// The entry publishes no `SKILL.md` that can be fetched directly.
    #[error("'{name}' has no direct SKILL.md download")]
    NoDirectDownload {
        /// The entry's display name.
        name: String,
        /// The human-facing page for the entry, when the catalog has one.
        source_url: Option<String>,
    },
    /// A URL failed validation.
    #[error("unsafe url: {}", describe_install_error(.0))]
    UnsafeUrl(InstallError),
    /// A fetched document is not a valid `SKILL.md`.
    #[error(transparent)]
    InvalidDocument(DocumentError),
    /// No source has the requested registry id.
    #[error("no registry has id '{id}'")]
    UnknownRegistry {
        /// The requested id.
        id: String,
    },
    /// The catalog store failed.
    #[error(transparent)]
    Store(StoreError),
    /// The transport broke its contract.
    #[error("transport contract violated: {detail}")]
    TransportContract {
        /// What was wrong.
        detail: String,
    },
    /// The transport failed.
    #[error(transparent)]
    Transport(TransportError),
}

/// Why the store failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The registry id is not a plain path segment.
    #[error("registry id {0:?} cannot name a stored catalog")]
    InvalidId(String),
    /// The stored file is larger than the read limit.
    #[error("stored catalog exceeds {limit} bytes")]
    TooLarge {
        /// The read limit.
        limit: u64,
    },
    /// The stored file is a symlink.
    #[error("stored catalog is a symlink")]
    Symlink,
    /// The stored file is not a readable catalog.
    #[error("stored catalog is corrupt: {0}")]
    Corrupt(String),
    /// A filesystem operation failed.
    #[error("catalog store i/o failed: {0}")]
    Io(String),
}

/// The stable, serializable kind of a [`RegistryError`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RegistryErrorKind {
    /// [`RegistryError::Timeout`].
    Timeout,
    /// [`RegistryError::Unavailable`].
    #[default]
    Unavailable,
    /// [`RegistryError::RateLimited`].
    RateLimited,
    /// [`RegistryError::TooLarge`].
    TooLarge,
    /// [`RegistryError::Malformed`].
    Malformed,
    /// [`RegistryError::NotFound`].
    NotFound,
    /// [`RegistryError::Ambiguous`].
    Ambiguous,
    /// [`RegistryError::UpstreamAmbiguous`].
    UpstreamAmbiguous,
    /// [`RegistryError::NoDirectDownload`].
    NoDirectDownload,
    /// [`RegistryError::UnsafeUrl`].
    UnsafeUrl,
    /// [`RegistryError::InvalidDocument`].
    InvalidDocument,
    /// [`RegistryError::UnknownRegistry`].
    UnknownRegistry,
    /// [`RegistryError::Store`].
    Store,
    /// [`RegistryError::TransportContract`].
    TransportContract,
    /// [`RegistryError::Transport`].
    Transport,
}

impl RegistryErrorKind {
    /// The `snake_case` name of this kind, as serialized.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::RateLimited => "rate_limited",
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::UpstreamAmbiguous => "upstream_ambiguous",
            Self::NoDirectDownload => "no_direct_download",
            Self::UnsafeUrl => "unsafe_url",
            Self::InvalidDocument => "invalid_document",
            Self::UnknownRegistry => "unknown_registry",
            Self::Store => "store",
            Self::TransportContract => "transport_contract",
            Self::Transport => "transport",
        }
    }
}

impl RegistryError {
    /// The stable kind of this error.
    #[must_use]
    pub fn kind(&self) -> RegistryErrorKind {
        match self {
            Self::Timeout { .. } => RegistryErrorKind::Timeout,
            Self::Unavailable { .. } => RegistryErrorKind::Unavailable,
            Self::RateLimited { .. } => RegistryErrorKind::RateLimited,
            Self::TooLarge { .. } => RegistryErrorKind::TooLarge,
            Self::Malformed { .. } => RegistryErrorKind::Malformed,
            Self::NotFound { .. } => RegistryErrorKind::NotFound,
            Self::Ambiguous { .. } => RegistryErrorKind::Ambiguous,
            Self::UpstreamAmbiguous { .. } => RegistryErrorKind::UpstreamAmbiguous,
            Self::NoDirectDownload { .. } => RegistryErrorKind::NoDirectDownload,
            Self::UnsafeUrl(_) => RegistryErrorKind::UnsafeUrl,
            Self::InvalidDocument(_) => RegistryErrorKind::InvalidDocument,
            Self::UnknownRegistry { .. } => RegistryErrorKind::UnknownRegistry,
            Self::Store(_) => RegistryErrorKind::Store,
            Self::TransportContract { .. } => RegistryErrorKind::TransportContract,
            Self::Transport(_) => RegistryErrorKind::Transport,
        }
    }

    /// Whether the failure was a timeout, the registry's own or the
    /// transport's.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. } | Self::Transport(TransportError::Timeout)
        )
    }

    /// Whether the upstream could not be reached or answered with an unusable
    /// status, so a later retry may succeed.
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable { .. } | Self::RateLimited { .. } | Self::Transport(_)
        ) || self.is_timeout()
    }

    /// The delay a throttling upstream asked for.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }

    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Timeout { operation, budget } => Self::Timeout {
                operation,
                budget: *budget,
            },
            Self::Unavailable { status } => Self::Unavailable { status: *status },
            Self::RateLimited { retry_after } => Self::RateLimited {
                retry_after: *retry_after,
            },
            Self::TooLarge { what, limit } => Self::TooLarge {
                what,
                limit: *limit,
            },
            Self::Malformed { what, detail } => Self::Malformed {
                what,
                detail: detail.clone(),
            },
            Self::NotFound { id, closest } => Self::NotFound {
                id: id.clone(),
                closest: closest.clone(),
            },
            Self::Ambiguous { name, count, ids } => Self::Ambiguous {
                name: name.clone(),
                count: *count,
                ids: ids.clone(),
            },
            Self::UpstreamAmbiguous { name } => Self::UpstreamAmbiguous { name: name.clone() },
            Self::NoDirectDownload { name, source_url } => Self::NoDirectDownload {
                name: name.clone(),
                source_url: source_url.clone(),
            },
            Self::UnsafeUrl(error) => Self::UnsafeUrl(duplicate_install_error(error)),
            Self::InvalidDocument(error) => match duplicate_document_error(error) {
                Some(error) => Self::InvalidDocument(error),
                None => Self::Malformed {
                    what: "document",
                    detail: error.to_string(),
                },
            },
            Self::UnknownRegistry { id } => Self::UnknownRegistry { id: id.clone() },
            Self::Store(error) => Self::Store(error.clone()),
            Self::TransportContract { detail } => Self::TransportContract {
                detail: detail.clone(),
            },
            Self::Transport(error) => Self::Transport(error.clone()),
        }
    }
}

fn rate_limited_message(retry_after: Option<Duration>) -> String {
    match retry_after {
        Some(delay) => format!("rate limited: retry after {}s", delay.as_secs()),
        None => "rate limited: retry shortly".to_owned(),
    }
}

fn not_found_hint(closest: &[String]) -> String {
    if closest.is_empty() {
        "Use an id returned by a catalog search.".to_owned()
    } else {
        format!("Closest ids: {}.", closest.join(", "))
    }
}

fn describe_install_error(error: &InstallError) -> String {
    match error {
        InstallError::InvalidUrl { message, .. } => format!("invalid url: {message}"),
        InstallError::MissingHost(_) => "url has no host".to_owned(),
        other => other.to_string(),
    }
}

fn duplicate_install_error(error: &InstallError) -> InstallError {
    match error {
        InstallError::EmptyUrl => InstallError::EmptyUrl,
        InstallError::UrlTooLong { len, max } => InstallError::UrlTooLong {
            len: *len,
            max: *max,
        },
        InstallError::InvalidUrl { input, message } => InstallError::InvalidUrl {
            input: input.clone(),
            message: message.clone(),
        },
        InstallError::UnsupportedUrl(reason) => InstallError::UnsupportedUrl(reason.clone()),
        InstallError::UnsupportedScheme(scheme) => InstallError::UnsupportedScheme(scheme.clone()),
        InstallError::MissingHost(raw) => InstallError::MissingHost(raw.clone()),
        InstallError::UnsafeHost { host } => InstallError::UnsafeHost { host: host.clone() },
        InstallError::EmptySlug => InstallError::EmptySlug,
        InstallError::SlugTooLong { max } => InstallError::SlugTooLong { max: *max },
        InstallError::DnsLookup { host, message } => InstallError::DnsLookup {
            host: host.clone(),
            message: message.clone(),
        },
        InstallError::NoAddresses { host } => InstallError::NoAddresses { host: host.clone() },
        InstallError::NonPublicAddress { host, address } => InstallError::NonPublicAddress {
            host: host.clone(),
            address: *address,
        },
    }
}

fn duplicate_document_error(error: &DocumentError) -> Option<DocumentError> {
    Some(match error {
        DocumentError::TooLarge { size, limit } => DocumentError::TooLarge {
            size: *size,
            limit: *limit,
        },
        DocumentError::UnterminatedFrontmatter => DocumentError::UnterminatedFrontmatter,
        DocumentError::MissingField(field) => DocumentError::MissingField(field),
        DocumentError::Slug(error) => DocumentError::Slug(duplicate_install_error(error)),
        DocumentError::InvalidUtf8(_) => return None,
    })
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
