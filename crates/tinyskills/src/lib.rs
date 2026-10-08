//! Portable primitives for agentskills.io-style skill bundles.
//!
//! `tinyskills` owns the host-independent parts of skill handling: document
//! parsing (YAML for discovery, [`parse_flat`] for documents a host stores and
//! re-serves), metadata, deterministic discovery, collision precedence, safe
//! resource reads, document digests, and materialization of compile-time
//! bundles and resolved skill trees. Product policy
//! such as installation roots, workspace trust, RPC, approvals, and execution
//! remains with the embedding host.

#[cfg(feature = "archive")]
mod archive;
mod authoring;
mod bundle;
mod catalog;
mod digest;
mod discovery;
mod document;
mod flat;
mod install;
mod materialize;
mod model;
#[cfg(feature = "registry")]
mod registry;
mod remove;
mod resource;
mod scan;
mod slug;
mod trigger;

#[cfg(feature = "archive")]
pub use archive::{
    ArchiveError, ArchiveFile, ArchiveFormat, ArchiveLimits, MAX_ARCHIVE_BYTES,
    MAX_ARCHIVE_ENTRIES, SkillArchive, read_skill_archive,
};
pub use authoring::{
    AuthoringError, BundleDocument, BundleSpec, DescriptionTooLong, FrontmatterTooLarge,
    ScaffoldOptions, ScaffoldOutcome, check_frontmatter_size, render_workflow_frontmatter,
    render_workflow_md, scaffold_bundle, slugify, validate_description, validate_description_chars,
    validate_display_name, yaml_scalar,
};
pub use bundle::{BundledFile, BundledSkill, InstallReport, install, is_current_materialization};
pub use catalog::{
    CatalogEntry, CatalogError, SkillsShRef, TreeMiss, catalog_entry_id, clawhub_download_url,
    closest_entry_ids, derive_download_url, download_url_from_docs_path,
    download_url_from_source_url, filter_catalog, find_catalog_entry, find_skill_md_in_tree,
    is_safe_segment, parse_catalog_json, parse_hermes_entry,
};
pub use digest::document_digest;
pub use discovery::{
    CollisionPolicy, DiscoveryRoot, TieBreak, discover, discover_with, load_skill_dir,
    resolve_collisions, resolve_collisions_with, scan_root,
};
pub use document::{inventory_resources, parse_skill, parse_skill_str, read_document};
pub use flat::{FlatError, FlatSkill, parse_flat, render_flat, split_frontmatter};
pub use install::{
    DocumentError, DocumentWrite, FetchedDocument, InstallError, MAX_INSTALL_DOCUMENT_BYTES,
    MAX_INSTALL_URL_LEN, WriteError, check_document_size, derive_install_slug,
    is_loopback_http_url, is_private_or_local_host, normalize_install_url, redact_url,
    validate_fetched_document, validate_install_url, validate_resolved_host,
    write_installed_document,
};
pub use materialize::{
    MAX_MATERIALIZE_DEPTH, MAX_MATERIALIZE_FILE_BYTES, MaterializeEntry, MaterializeError,
    MaterializeReport, MaterializeSource, materialize_tree,
};
pub use model::{
    MAX_DESCRIPTION_LEN, MAX_DOCUMENT_BYTES, MAX_NAME_LEN, MAX_RESOURCE_BYTES, RESOURCE_DIRS,
    SKILL_JSON, SKILL_MD, Skill, SkillFrontmatter, SkillScope, WORKFLOW_MD,
};
#[cfg(feature = "registry")]
pub use registry::{
    BodyChunks, BoxFuture, CatalogStore, Clock, EntryKey, Facet, FetchPolicy, FileCatalogStore,
    Freshness, GuardedResponse, HermesIndexSource, HttpMethod, MemoryCatalogStore,
    REGISTRY_CONTRACT_VERSION, ReadPolicy, RegistryDocument, RegistryEntry, RegistryError,
    RegistryErrorKind, RegistryErrorSummary, RegistryFacets, RegistryLimits, RegistryTimeouts,
    RegistryTransport, Resolver, SkillDetail, SkillPage, SkillQuery, SkillRegistry,
    SkillRegistryBuilder, SkillSource, SkillSummary, SourceContext, SourceDescriptor, SourceLoad,
    SourceStatus, StaticSource, StoreError, StoredCatalog, SystemClock, SystemResolver,
    TransportError, TransportRequest, TransportResponse, Validators, fetch_skill_document,
    is_registry_contract_compatible, normalize_registry_document_url,
};
pub use remove::{RemoveError, remove_bundle};
pub use resource::{ResourceError, read_resource, resolve_skill};
pub use scan::{
    Finding, ScanCheck, ScanDocument, ScanField, ScanReport, ScanResource, Verdict, is_invisible,
    sanitize_catalogue_text, scan_skill,
};
pub use slug::{PunctuationRule, SlugError, SlugRules, slugify_with, validate_slug};
pub use trigger::TriggerPattern;

/// Re-exported so a host opens the directory handles [`materialize_tree`] takes
/// with the same `cap-std` and `cap-fs-ext` versions.
pub use {cap_fs_ext, cap_std};
