//! The source abstraction: where a registry's catalog comes from.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::contract::{RegistryEntry, Validators};
use super::error::RegistryError;
use super::fetch::{FetchSpec, GuardedFetcher, GuardedResponse, RegistryLimits, RegistryTimeouts};
use super::transport::{BoxFuture, HttpMethod};

/// A source's id and display label. The id names the registry in queries,
/// in [`EntryKey`](crate::EntryKey) and in the store, and must be a plain path
/// segment (see [`is_safe_segment`](crate::is_safe_segment)) to be persisted
/// by a [`FileCatalogStore`](crate::FileCatalogStore).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SourceDescriptor {
    /// The registry id.
    pub id: String,
    /// Display label.
    pub label: String,
}

impl SourceDescriptor {
    /// A descriptor with `id` and `label`.
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// The outcome of [`SkillSource::load`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SourceLoad {
    /// A new catalog.
    Fresh {
        /// The entries, in catalog order.
        entries: Vec<RegistryEntry>,
        /// Validators for the next conditional load.
        validators: Validators,
        /// Upstream items that could not be read as entries.
        skipped: usize,
    },
    /// The catalog held is still current.
    NotModified,
}

/// What a source may use while loading: the guarded fetcher and the
/// registry's budgets. A source never sees the raw transport.
#[derive(Debug, Clone)]
pub struct SourceContext {
    fetcher: Arc<GuardedFetcher>,
    timeouts: RegistryTimeouts,
    limits: RegistryLimits,
}

impl SourceContext {
    pub(crate) fn new(
        fetcher: Arc<GuardedFetcher>,
        timeouts: RegistryTimeouts,
        limits: RegistryLimits,
    ) -> Self {
        Self {
            fetcher,
            timeouts,
            limits,
        }
    }

    /// The registry's time budgets.
    #[must_use]
    pub fn timeouts(&self) -> &RegistryTimeouts {
        &self.timeouts
    }

    /// The registry's limits.
    #[must_use]
    pub fn limits(&self) -> &RegistryLimits {
        &self.limits
    }

    /// A guarded `GET` of `url` with extra `headers`, reading at most
    /// `max_bytes` of a `2xx` body, within `budget`. `what` names the
    /// operation in errors.
    ///
    /// `Authorization`, `Proxy-Authorization` and `Cookie` headers are
    /// dropped from a redirect hop that changes origin.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnsafeUrl`] for a refused URL or redirect hop,
    /// [`RegistryError::TooLarge`], [`RegistryError::Timeout`],
    /// [`RegistryError::Transport`] and
    /// [`RegistryError::TransportContract`]. A non-`2xx` status is returned,
    /// not raised.
    pub async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        max_bytes: u64,
        what: &'static str,
        budget: Duration,
    ) -> Result<GuardedResponse, RegistryError> {
        self.fetcher
            .fetch(FetchSpec {
                method: HttpMethod::Get,
                url,
                headers,
                max_bytes,
                what,
                budget,
            })
            .await
    }

    /// A guarded `HEAD` of `url` within `budget`.
    ///
    /// # Errors
    ///
    /// As [`get`](Self::get).
    pub async fn head(
        &self,
        url: &str,
        what: &'static str,
        budget: Duration,
    ) -> Result<GuardedResponse, RegistryError> {
        self.fetcher
            .fetch(FetchSpec {
                method: HttpMethod::Head,
                url,
                headers: &[],
                max_bytes: 0,
                what,
                budget,
            })
            .await
    }
}

/// A catalog source.
pub trait SkillSource: Send + Sync {
    /// The source's id and label. Must not change over the source's life.
    fn descriptor(&self) -> SourceDescriptor;

    /// Load the catalog. `prior` holds the validators of the catalog held,
    /// for a conditional request; answer [`SourceLoad::NotModified`] only when
    /// `prior` is `Some`.
    fn load<'a>(
        &'a self,
        ctx: &'a SourceContext,
        prior: Option<&'a Validators>,
    ) -> BoxFuture<'a, Result<SourceLoad, RegistryError>>;

    /// The URL to fetch an entry's `SKILL.md` from. The default is the
    /// entry's download URL.
    fn resolve_document_url<'a>(
        &'a self,
        ctx: &'a SourceContext,
        entry: &'a RegistryEntry,
    ) -> BoxFuture<'a, Result<String, RegistryError>> {
        let _ = ctx;
        Box::pin(async move { direct_download_url(entry) })
    }

    /// The error for a non-`2xx` answer to an entry's document fetch, when
    /// the source knows better than the status alone. The default is `None`.
    fn document_status_error(&self, entry: &RegistryEntry, status: u16) -> Option<RegistryError> {
        let _ = (entry, status);
        None
    }

    /// Whether the catalog is local and never goes stale. A local source is
    /// reported as [`Freshness::LocalFallback`](crate::Freshness::LocalFallback).
    fn is_local(&self) -> bool {
        false
    }
}

pub(crate) fn direct_download_url(entry: &RegistryEntry) -> Result<String, RegistryError> {
    if entry.entry.has_direct_download() {
        Ok(entry.entry.download_url.clone())
    } else {
        Err(RegistryError::NoDirectDownload {
            name: entry.entry.name.clone(),
            source_url: entry.entry.source_url.clone(),
        })
    }
}

impl<T: SkillSource + ?Sized> SkillSource for Arc<T> {
    fn descriptor(&self) -> SourceDescriptor {
        (**self).descriptor()
    }

    fn load<'a>(
        &'a self,
        ctx: &'a SourceContext,
        prior: Option<&'a Validators>,
    ) -> BoxFuture<'a, Result<SourceLoad, RegistryError>> {
        (**self).load(ctx, prior)
    }

    fn resolve_document_url<'a>(
        &'a self,
        ctx: &'a SourceContext,
        entry: &'a RegistryEntry,
    ) -> BoxFuture<'a, Result<String, RegistryError>> {
        (**self).resolve_document_url(ctx, entry)
    }

    fn document_status_error(&self, entry: &RegistryEntry, status: u16) -> Option<RegistryError> {
        (**self).document_status_error(entry, status)
    }

    fn is_local(&self) -> bool {
        (**self).is_local()
    }
}
