//! Cached, searchable skill registries (feature `registry`).
//!
//! A [`SkillRegistry`] owns one or more [`SkillSource`]s, keeps each source's
//! catalog in a [`CatalogStore`], and answers search, detail and facet
//! queries from an in-memory index. Stale catalogs are served while a
//! background refresh runs; concurrent refreshes of one source collapse into
//! a single fetch.
//!
//! Network I/O is host-supplied through [`RegistryTransport`]: the crate links
//! no HTTP client. Every request goes through a guard that allows `https`
//! only, resolves the host itself, rejects non-public addresses, pins the
//! connection to the addresses it checked, re-validates every redirect hop and
//! bounds every body and every operation.

mod contract;
mod error;
mod fetch;
mod hermes;
mod index;
mod service;
mod source;
mod static_source;
mod store;
mod transport;
mod url;

pub use contract::{
    EntryKey, Facet, Freshness, REGISTRY_CONTRACT_VERSION, ReadPolicy, RegistryEntry,
    RegistryErrorSummary, RegistryFacets, SkillDetail, SkillPage, SkillQuery, SkillSummary,
    SourceStatus, Validators, is_registry_contract_compatible,
};
pub use error::{RegistryError, RegistryErrorKind, StoreError};
pub use fetch::{
    FetchPolicy, GuardedResponse, RegistryDocument, RegistryLimits, RegistryTimeouts,
    fetch_skill_document,
};
pub use hermes::HermesIndexSource;
pub use service::{SkillRegistry, SkillRegistryBuilder};
pub use source::{SkillSource, SourceContext, SourceDescriptor, SourceLoad};
pub use static_source::StaticSource;
pub use store::{
    CatalogStore, Clock, FileCatalogStore, MemoryCatalogStore, StoredCatalog, SystemClock,
};
pub use transport::{
    BodyChunks, BoxFuture, HttpMethod, RegistryTransport, Resolver, SystemResolver, TransportError,
    TransportRequest, TransportResponse,
};
pub use url::normalize_registry_document_url;
