//! A source whose catalog the host supplies in memory.

use super::contract::{RegistryEntry, Validators};
use super::error::RegistryError;
use super::source::{SkillSource, SourceContext, SourceDescriptor, SourceLoad};
use super::transport::BoxFuture;

/// A fixed, host-supplied catalog: a packaged library, or a baseline served
/// when no remote catalog is available. It never goes stale and is reported
/// as [`Freshness::LocalFallback`](crate::Freshness::LocalFallback).
#[derive(Debug, Clone)]
pub struct StaticSource {
    descriptor: SourceDescriptor,
    entries: Vec<RegistryEntry>,
}

impl StaticSource {
    /// A source with registry id `id`, display `label` and `entries`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        entries: Vec<RegistryEntry>,
    ) -> Self {
        Self {
            descriptor: SourceDescriptor::new(id, label),
            entries,
        }
    }
}

impl SkillSource for StaticSource {
    fn descriptor(&self) -> SourceDescriptor {
        self.descriptor.clone()
    }

    fn load<'a>(
        &'a self,
        _ctx: &'a SourceContext,
        _prior: Option<&'a Validators>,
    ) -> BoxFuture<'a, Result<SourceLoad, RegistryError>> {
        Box::pin(async move {
            Ok(SourceLoad::Fresh {
                entries: self.entries.clone(),
                validators: Validators::default(),
                skipped: 0,
            })
        })
    }

    fn is_local(&self) -> bool {
        true
    }
}
