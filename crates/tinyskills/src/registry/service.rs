//! The registry service: sources, cache, single-flight refresh and queries.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use super::contract::{
    EntryKey, Freshness, ReadPolicy, RegistryEntry, RegistryErrorSummary, RegistryFacets,
    SkillDetail, SkillPage, SkillQuery, SkillSummary, SourceStatus, Validators,
};
use super::error::{RegistryError, StoreError};
use super::fetch::{
    FetchPolicy, GuardedFetcher, RegistryDocument, RegistryLimits, RegistryTimeouts, build_document,
};
use super::index::{CatalogIndex, Hit, Lookup, MAX_SUGGESTED_IDS, merge_facets};
use super::source::{SkillSource, SourceContext, SourceDescriptor, SourceLoad};
use super::store::{CatalogStore, Clock, MemoryCatalogStore, StoredCatalog, SystemClock};
use super::transport::{RegistryTransport, Resolver, SystemResolver};
use super::url::normalize_registry_document_url;

const DEFAULT_TTL: Duration = Duration::from_secs(3600);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 3600);

/// A set of skill sources behind one cache and one query API.
///
/// Built with [`SkillRegistry::builder`]. Reads follow stale-while-revalidate:
///
/// - a catalog within its time-to-live answers as [`Freshness::Live`];
/// - a stale catalog under [`ReadPolicy::AllowStale`] answers as
///   [`Freshness::Cached`] and starts one background refresh, when a tokio
///   runtime is running;
/// - a stale catalog under [`ReadPolicy::RequireFresh`], or a source with no
///   catalog, refreshes first. Concurrent refreshes of one source share one
///   fetch.
///
/// A failed refresh keeps the catalog held, answering as
///   [`Freshness::Cached`] with the failure in [`SourceStatus::last_error`],
/// and the source is not fetched again until the cooldown (the larger of
/// [`RegistryTimeouts::cooldown`] and the upstream's `Retry-After`) passes.
/// When no selected source has a catalog, the baselines answer as
/// [`Freshness::LocalFallback`]; with no baseline, the first failure is
/// returned.
///
/// The registry never writes a skill to disk and never refuses a document
/// for its scan verdict.
pub struct SkillRegistry {
    slots: Vec<Arc<Slot>>,
    baselines: Vec<Arc<Slot>>,
    shared: Arc<Shared>,
    featured: HashMap<String, usize>,
    max_page_size: usize,
}

impl fmt::Debug for SkillRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillRegistry")
            .field(
                "sources",
                &self
                    .slots
                    .iter()
                    .map(|slot| &slot.descriptor.id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "baselines",
                &self
                    .baselines
                    .iter()
                    .map(|slot| &slot.descriptor.id)
                    .collect::<Vec<_>>(),
            )
            .field("ttl", &self.shared.ttl)
            .finish_non_exhaustive()
    }
}

struct Shared {
    store: Arc<dyn CatalogStore>,
    ctx: SourceContext,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    cooldown: Duration,
}

struct Slot {
    source: Arc<dyn SkillSource>,
    descriptor: SourceDescriptor,
    baseline: bool,
    lock: tokio::sync::Mutex<()>,
    state: RwLock<SlotState>,
    refreshing: AtomicBool,
    background: AtomicBool,
}

#[derive(Default)]
struct SlotState {
    index: Option<Arc<CatalogIndex>>,
    fetched_at: Option<SystemTime>,
    validators: Validators,
    skipped: usize,
    consulted_store: bool,
    last_error: Option<RegistryError>,
    cooldown_until: Option<SystemTime>,
}

struct View {
    slot: Arc<Slot>,
    index: Arc<CatalogIndex>,
    freshness: Freshness,
    fetched_at: Option<u64>,
}

struct ClearOnDrop<'a>(&'a AtomicBool);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Slot {
    fn new(source: Arc<dyn SkillSource>, baseline: bool) -> Self {
        Self {
            descriptor: source.descriptor(),
            source,
            baseline,
            lock: tokio::sync::Mutex::new(()),
            state: RwLock::new(SlotState::default()),
            refreshing: AtomicBool::new(false),
            background: AtomicBool::new(false),
        }
    }

    fn is_local(&self) -> bool {
        self.baseline || self.source.is_local()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, SlotState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, SlotState> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Shared {
    fn is_fresh(&self, slot: &Slot, state: &SlotState, now: SystemTime) -> bool {
        if state.index.is_none() {
            return false;
        }
        if slot.is_local() {
            return true;
        }
        state.fetched_at.is_some_and(|fetched| {
            now.duration_since(fetched)
                .map_or(true, |age| age < self.ttl)
        })
    }

    fn freshness(&self, slot: &Slot, state: &SlotState, now: SystemTime) -> Freshness {
        if slot.is_local() {
            Freshness::LocalFallback
        } else if self.is_fresh(slot, state, now) {
            Freshness::Live
        } else {
            Freshness::Cached
        }
    }

    fn view(&self, slot: &Arc<Slot>, state: &SlotState, now: SystemTime) -> Option<View> {
        state.index.as_ref().map(|index| View {
            slot: Arc::clone(slot),
            index: Arc::clone(index),
            freshness: self.freshness(slot, state, now),
            fetched_at: state.fetched_at.map(unix_secs),
        })
    }

    fn status(&self, slot: &Slot) -> SourceStatus {
        let now = self.clock.now();
        let state = slot.read();
        SourceStatus {
            id: slot.descriptor.id.clone(),
            label: slot.descriptor.label.clone(),
            freshness: state
                .index
                .as_ref()
                .map(|_| self.freshness(slot, &state, now)),
            entry_count: state.index.as_ref().map_or(0, |index| index.len()),
            skipped: state.skipped,
            fetched_at: state.fetched_at.map(unix_secs),
            last_error: state.last_error.as_ref().map(RegistryErrorSummary::of),
            refreshing: slot.refreshing.load(Ordering::Acquire),
        }
    }

    fn cooling_down(state: &SlotState, now: SystemTime) -> bool {
        state.cooldown_until.is_some_and(|until| now < until)
    }

    fn cooldown_error(state: &SlotState) -> RegistryError {
        state.last_error.as_ref().map_or(
            RegistryError::RateLimited { retry_after: None },
            RegistryError::duplicate,
        )
    }

    async fn consult_store_locked(&self, slot: &Slot) {
        if slot.read().consulted_store {
            return;
        }
        if slot.is_local() {
            slot.write().consulted_store = true;
            return;
        }
        let loaded = self.store.load(&slot.descriptor.id).await;
        let mut index = None;
        let mut stored_meta = None;
        let mut error = None;
        match loaded {
            Ok(Some(stored)) => {
                match SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(stored.fetched_at)) {
                    Some(fetched_at) => {
                        stored_meta = Some((fetched_at, stored.validators, stored.skipped));
                        index = Some(Arc::new(CatalogIndex::new(stored.entries)));
                    }
                    None => {
                        error = Some(RegistryError::Store(StoreError::Corrupt(
                            "fetched_at is out of range".to_owned(),
                        )));
                    }
                }
            }
            Ok(None) => {}
            Err(store_error) => error = Some(RegistryError::Store(store_error)),
        }
        let mut state = slot.write();
        state.consulted_store = true;
        if state.index.is_none()
            && let (Some(index), Some((fetched_at, validators, skipped))) = (index, stored_meta)
        {
            state.index = Some(index);
            state.fetched_at = Some(fetched_at);
            state.validators = validators;
            state.skipped = skipped;
        }
        if error.is_some() {
            state.last_error = error;
        }
    }

    async fn refresh(&self, slot: &Slot, force: bool) -> Result<(), RegistryError> {
        let _guard = slot.lock.lock().await;
        self.consult_store_locked(slot).await;
        let prior = {
            let now = self.clock.now();
            let state = slot.read();
            if !force && self.is_fresh(slot, &state, now) {
                return Ok(());
            }
            if Self::cooling_down(&state, now) {
                return Err(Self::cooldown_error(&state));
            }
            state.index.as_ref().map(|_| state.validators.clone())
        };
        slot.refreshing.store(true, Ordering::Release);
        let _refreshing = ClearOnDrop(&slot.refreshing);
        let loaded = slot.source.load(&self.ctx, prior.as_ref()).await;
        let now = self.clock.now();
        match loaded {
            Ok(SourceLoad::Fresh {
                entries,
                validators,
                skipped,
            }) => {
                let mut entries = entries;
                let mut store_error = None;
                if !slot.is_local() {
                    let stored =
                        StoredCatalog::new(entries, unix_secs(now), validators.clone(), skipped);
                    if let Err(error) = self.store.save(&slot.descriptor.id, &stored).await {
                        store_error = Some(RegistryError::Store(error));
                    }
                    entries = stored.entries;
                }
                let index = Arc::new(CatalogIndex::new(entries));
                let mut state = slot.write();
                state.index = Some(index);
                state.fetched_at = Some(now);
                state.validators = validators;
                state.skipped = skipped;
                state.last_error = store_error;
                state.cooldown_until = None;
                Ok(())
            }
            Ok(SourceLoad::NotModified) => {
                let held = {
                    let state = slot.read();
                    state
                        .index
                        .as_ref()
                        .map(|index| (Arc::clone(index), state.validators.clone(), state.skipped))
                };
                let Some((index, validators, skipped)) = held else {
                    return Err(RegistryError::Malformed {
                        what: "catalog",
                        detail: "not modified, but no catalog is held".to_owned(),
                    });
                };
                let mut store_error = None;
                if !slot.is_local() {
                    let stored = StoredCatalog::new(
                        index.entries().to_vec(),
                        unix_secs(now),
                        validators,
                        skipped,
                    );
                    if let Err(error) = self.store.save(&slot.descriptor.id, &stored).await {
                        store_error = Some(RegistryError::Store(error));
                    }
                }
                let mut state = slot.write();
                state.fetched_at = Some(now);
                state.last_error = store_error;
                state.cooldown_until = None;
                Ok(())
            }
            Err(error) => {
                let wait = error.retry_after().map_or(self.cooldown, |delay| {
                    delay.min(MAX_RETRY_AFTER).max(self.cooldown)
                });
                let mut state = slot.write();
                state.cooldown_until = now.checked_add(wait);
                state.last_error = Some(error.duplicate());
                Err(error)
            }
        }
    }
}

fn spawn_background_refresh(shared: &Arc<Shared>, slot: &Arc<Slot>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if Shared::cooling_down(&slot.read(), shared.clock.now()) {
        return;
    }
    if slot
        .background
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let shared = Arc::clone(shared);
    let slot = Arc::clone(slot);
    handle.spawn(async move {
        let _background = ClearOnDrop(&slot.background);
        let _ = shared.refresh(&slot, false).await;
    });
}

impl SkillRegistry {
    /// A builder whose requests go through `transport`.
    #[must_use]
    pub fn builder(transport: impl RegistryTransport + 'static) -> SkillRegistryBuilder {
        SkillRegistryBuilder {
            transport: Arc::new(transport),
            sources: Vec::new(),
            baselines: Vec::new(),
            store: Arc::new(MemoryCatalogStore::new()),
            resolver: Arc::new(SystemResolver),
            clock: Arc::new(SystemClock),
            timeouts: RegistryTimeouts::default(),
            limits: RegistryLimits::default(),
            policy: FetchPolicy::default(),
            ttl: DEFAULT_TTL,
            featured: Vec::new(),
        }
    }

    fn select(&self, registries: &[String]) -> Result<Vec<Arc<Slot>>, RegistryError> {
        if registries.is_empty() {
            return Ok(self.slots.clone());
        }
        registries
            .iter()
            .map(|id| {
                self.slots
                    .iter()
                    .chain(&self.baselines)
                    .find(|slot| slot.descriptor.id == *id)
                    .cloned()
                    .ok_or_else(|| RegistryError::UnknownRegistry { id: id.clone() })
            })
            .collect()
    }

    async fn ensure(&self, slot: &Arc<Slot>, read: ReadPolicy) -> Result<View, RegistryError> {
        if !slot.read().consulted_store {
            let _guard = slot.lock.lock().await;
            self.shared.consult_store_locked(slot).await;
        }
        {
            let now = self.shared.clock.now();
            let state = slot.read();
            if self.shared.is_fresh(slot, &state, now) {
                if let Some(view) = self.shared.view(slot, &state, now) {
                    return Ok(view);
                }
            } else if state.index.is_some()
                && (read == ReadPolicy::AllowStale || Shared::cooling_down(&state, now))
            {
                let view = self.shared.view(slot, &state, now);
                drop(state);
                if read == ReadPolicy::AllowStale {
                    spawn_background_refresh(&self.shared, slot);
                }
                if let Some(view) = view {
                    return Ok(view);
                }
            } else if state.index.is_none() && Shared::cooling_down(&state, now) {
                return Err(Shared::cooldown_error(&state));
            }
        }
        let refreshed = self.shared.refresh(slot, false).await;
        let now = self.shared.clock.now();
        let state = slot.read();
        match (self.shared.view(slot, &state, now), refreshed) {
            (Some(view), _) => Ok(view),
            (None, Err(error)) => Err(error),
            (None, Ok(())) => Err(RegistryError::Malformed {
                what: "catalog",
                detail: "the source produced no catalog".to_owned(),
            }),
        }
    }

    async fn views(
        &self,
        registries: &[String],
        read: ReadPolicy,
    ) -> Result<(Vec<View>, Vec<Arc<Slot>>), RegistryError> {
        let selected = self.select(registries)?;
        let mut views = Vec::new();
        let mut first_error = None;
        for slot in &selected {
            match self.ensure(slot, read).await {
                Ok(view) => views.push(view),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        let mut answered = selected;
        if views.is_empty() && !self.baselines.is_empty() {
            for slot in &self.baselines {
                if let Ok(view) = self.ensure(slot, ReadPolicy::AllowStale).await {
                    views.push(view);
                    answered.push(Arc::clone(slot));
                }
            }
        }
        match first_error {
            Some(error) if views.is_empty() => Err(error),
            _ => Ok((views, answered)),
        }
    }

    fn statuses(&self, slots: &[Arc<Slot>]) -> Vec<SourceStatus> {
        let mut seen = Vec::new();
        slots
            .iter()
            .filter(|slot| {
                let new = !seen.contains(&slot.descriptor.id);
                seen.push(slot.descriptor.id.clone());
                new
            })
            .map(|slot| self.shared.status(slot))
            .collect()
    }

    /// Search the selected registries.
    ///
    /// Hits are ordered: featured ids in their configured order, then an
    /// exact name match, a name prefix, a name substring and a match in any
    /// other field; installable before not installable; then source order and
    /// catalog order.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownRegistry`] for an unknown id in
    /// [`SkillQuery::registries`]; otherwise the first source's load error
    /// when no selected source and no baseline has a catalog.
    pub async fn search(&self, query: &SkillQuery) -> Result<SkillPage, RegistryError> {
        let (views, answered) = self.views(&query.registries, query.read).await?;
        let page_size = query.page_size.clamp(1, self.max_page_size.max(1));
        let page = query.page.max(1);
        let mut hits: Vec<(Hit, usize)> = Vec::new();
        for (order, view) in views.iter().enumerate() {
            hits.extend(
                view.index
                    .search(query, order, &self.featured)
                    .into_iter()
                    .map(|hit| (hit, order)),
            );
        }
        hits.sort_by_key(|(hit, _)| hit.rank);
        let total = hits.len();
        let items = hits
            .iter()
            .skip((page - 1).saturating_mul(page_size))
            .take(page_size)
            .filter_map(|(hit, order)| {
                let view = &views[*order];
                view.index
                    .entry(hit.position)
                    .map(|entry| SkillSummary::from_entry(&view.slot.descriptor.id, entry))
            })
            .collect();
        Ok(SkillPage {
            items,
            page,
            page_size,
            total,
            total_pages: total.div_ceil(page_size),
            freshness: views
                .iter()
                .map(|view| view.freshness)
                .max()
                .unwrap_or_default(),
            fetched_at: views.iter().filter_map(|view| view.fetched_at).min(),
            sources: self.statuses(&answered),
        })
    }

    async fn locate(&self, key: &EntryKey) -> Result<(Arc<Slot>, RegistryEntry), RegistryError> {
        let registries: Vec<String> = key.registry.iter().cloned().collect();
        let (views, _) = self.views(&registries, ReadPolicy::AllowStale).await?;
        let mut named = Vec::new();
        for view in &views {
            match view.index.lookup(&key.id) {
                Some(Lookup::Found(entry)) => return Ok((Arc::clone(&view.slot), entry.clone())),
                Some(Lookup::Named(entries)) => {
                    named.extend(entries.into_iter().map(|entry| (view, entry)));
                }
                None => {}
            }
        }
        match named.as_slice() {
            [(view, entry)] => Ok((Arc::clone(&view.slot), (*entry).clone())),
            [] => {
                let mut closest = Vec::new();
                for view in &views {
                    for id in view.index.closest(&key.id) {
                        if closest.len() < MAX_SUGGESTED_IDS && !closest.contains(&id) {
                            closest.push(id);
                        }
                    }
                }
                Err(RegistryError::NotFound {
                    id: key.id.trim().to_owned(),
                    closest,
                })
            }
            many => Err(RegistryError::Ambiguous {
                name: key.id.trim().to_owned(),
                count: many.len(),
                ids: many
                    .iter()
                    .take(MAX_SUGGESTED_IDS)
                    .map(|(_, entry)| entry.entry.id.clone())
                    .collect(),
            }),
        }
    }

    /// Everything known about one entry.
    ///
    /// # Errors
    ///
    /// [`RegistryError::NotFound`], [`RegistryError::Ambiguous`] for a name
    /// several entries carry, [`RegistryError::UnknownRegistry`], or the load
    /// error when no catalog is available.
    pub async fn detail(&self, key: &EntryKey) -> Result<SkillDetail, RegistryError> {
        let (slot, entry) = self.locate(key).await?;
        Ok(SkillDetail::from_entry(&slot.descriptor.id, &entry))
    }

    /// Upstream and category facets of one registry, or of all when `None`.
    ///
    /// # Errors
    ///
    /// As [`search`](Self::search).
    pub async fn facets(&self, registry: Option<&str>) -> Result<RegistryFacets, RegistryError> {
        let registries: Vec<String> = registry.map(str::to_owned).into_iter().collect();
        let (views, _) = self.views(&registries, ReadPolicy::AllowStale).await?;
        let upstreams: Vec<_> = views
            .iter()
            .map(|view| view.index.upstream_facets())
            .collect();
        let categories: Vec<_> = views
            .iter()
            .map(|view| view.index.category_facets())
            .collect();
        Ok(RegistryFacets {
            upstreams: merge_facets(&upstreams),
            categories: merge_facets(&categories),
            freshness: views
                .iter()
                .map(|view| view.freshness)
                .max()
                .unwrap_or_default(),
        })
    }

    /// The status of every source and baseline. Performs no I/O.
    #[must_use]
    pub fn sources(&self) -> Vec<SourceStatus> {
        let all: Vec<Arc<Slot>> = self.slots.iter().chain(&self.baselines).cloned().collect();
        self.statuses(&all)
    }

    /// Refresh one registry, or every source when `None`, and return their
    /// status. A fresh catalog is refetched only when `force` is set; a
    /// source in cooldown is not fetched. Load failures are reported in
    /// [`SourceStatus::last_error`], not returned.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownRegistry`] for an unknown id.
    pub async fn refresh(
        &self,
        registry: Option<&str>,
        force: bool,
    ) -> Result<Vec<SourceStatus>, RegistryError> {
        let registries: Vec<String> = registry.map(str::to_owned).into_iter().collect();
        let selected = self.select(&registries)?;
        for slot in &selected {
            let _ = self.shared.refresh(slot, force).await;
        }
        Ok(self.statuses(&selected))
    }

    /// Load every source's catalog, from the store when possible, and return
    /// their status. Stale catalogs are refreshed in the background.
    pub async fn warm(&self) -> Vec<SourceStatus> {
        for slot in &self.slots {
            let _ = self.ensure(slot, ReadPolicy::AllowStale).await;
        }
        self.sources()
    }

    /// Fetch, validate and scan one entry's `SKILL.md`.
    ///
    /// The source resolves the URL (probing `skills.sh` locations, for
    /// example), which is normalized with [`normalize_registry_document_url`]
    /// and fetched through the guard within [`RegistryTimeouts::document`] and
    /// [`RegistryLimits::max_document_bytes`]. A document whose scan blocks is
    /// returned; [`RegistryDocument::is_blocked`] reports it.
    ///
    /// # Errors
    ///
    /// The [`detail`](Self::detail) errors,
    /// [`RegistryError::NoDirectDownload`],
    /// [`RegistryError::UpstreamAmbiguous`], [`RegistryError::UnsafeUrl`],
    /// [`RegistryError::Unavailable`], [`RegistryError::RateLimited`],
    /// [`RegistryError::TooLarge`], [`RegistryError::Timeout`],
    /// [`RegistryError::Transport`] and [`RegistryError::InvalidDocument`].
    pub async fn fetch_document(&self, key: &EntryKey) -> Result<RegistryDocument, RegistryError> {
        let (slot, entry) = self.locate(key).await?;
        let ctx = &self.shared.ctx;
        let url = slot.source.resolve_document_url(ctx, &entry).await?;
        let url = normalize_registry_document_url(&url).map_err(RegistryError::UnsafeUrl)?;
        let response = ctx
            .get(
                &url,
                &[],
                ctx.limits().max_document_bytes,
                "document",
                ctx.timeouts().document,
            )
            .await?;
        if !response.is_success() {
            return Err(slot
                .source
                .document_status_error(&entry, response.status)
                .unwrap_or_else(|| response.status_error()));
        }
        build_document(
            Some(SkillSummary::from_entry(&slot.descriptor.id, &entry)),
            &response.url,
            &response.body,
        )
    }
}

/// Configures a [`SkillRegistry`].
pub struct SkillRegistryBuilder {
    transport: Arc<dyn RegistryTransport>,
    sources: Vec<Arc<dyn SkillSource>>,
    baselines: Vec<Arc<dyn SkillSource>>,
    store: Arc<dyn CatalogStore>,
    resolver: Arc<dyn Resolver>,
    clock: Arc<dyn Clock>,
    timeouts: RegistryTimeouts,
    limits: RegistryLimits,
    policy: FetchPolicy,
    ttl: Duration,
    featured: Vec<String>,
}

impl fmt::Debug for SkillRegistryBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillRegistryBuilder")
            .field("sources", &self.sources.len())
            .field("baselines", &self.baselines.len())
            .field("timeouts", &self.timeouts)
            .field("limits", &self.limits)
            .field("policy", &self.policy)
            .field("ttl", &self.ttl)
            .field("featured", &self.featured)
            .finish_non_exhaustive()
    }
}

impl SkillRegistryBuilder {
    /// Add a source. Sources are searched in the order added; a source whose
    /// id repeats an earlier one is ignored.
    #[must_use]
    pub fn source(mut self, source: impl SkillSource + 'static) -> Self {
        self.sources.push(Arc::new(source));
        self
    }

    /// Add a baseline, served only when no selected source has a catalog.
    #[must_use]
    pub fn baseline(mut self, baseline: impl SkillSource + 'static) -> Self {
        self.baselines.push(Arc::new(baseline));
        self
    }

    /// Persist catalogs in `store`. The default keeps them in memory.
    #[must_use]
    pub fn store(mut self, store: impl CatalogStore + 'static) -> Self {
        self.store = Arc::new(store);
        self
    }

    /// Resolve host names with `resolver`. The default is [`SystemResolver`].
    #[must_use]
    pub fn resolver(mut self, resolver: impl Resolver + 'static) -> Self {
        self.resolver = Arc::new(resolver);
        self
    }

    /// Age catalogs by `clock`. The default is [`SystemClock`].
    #[must_use]
    pub fn clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Set the time budgets.
    #[must_use]
    pub fn timeouts(mut self, timeouts: RegistryTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Set the limits.
    #[must_use]
    pub fn limits(mut self, limits: RegistryLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Set the network policy.
    #[must_use]
    pub fn policy(mut self, policy: FetchPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Set how long a fetched catalog stays fresh. Default one hour.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Entry ids that rank first, in this order, whenever they match a query.
    #[must_use]
    pub fn featured(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.featured = ids.into_iter().map(Into::into).collect();
        self
    }

    /// Build the registry. No I/O happens until the first query.
    #[must_use]
    pub fn build(self) -> Arc<SkillRegistry> {
        let fetcher = GuardedFetcher::new(
            self.transport,
            self.resolver,
            self.policy,
            &self.timeouts,
            &self.limits,
        );
        let ctx = SourceContext::new(Arc::new(fetcher), self.timeouts, self.limits);
        let mut ids: Vec<String> = Vec::new();
        let mut slots = Vec::new();
        let mut baselines = Vec::new();
        for (source, baseline) in self
            .sources
            .into_iter()
            .map(|source| (source, false))
            .chain(self.baselines.into_iter().map(|source| (source, true)))
        {
            let slot = Slot::new(source, baseline);
            if ids.contains(&slot.descriptor.id) {
                continue;
            }
            ids.push(slot.descriptor.id.clone());
            if baseline {
                baselines.push(Arc::new(slot));
            } else {
                slots.push(Arc::new(slot));
            }
        }
        let mut featured = HashMap::new();
        for (position, id) in self.featured.into_iter().enumerate() {
            featured.entry(id).or_insert(position);
        }
        Arc::new(SkillRegistry {
            slots,
            baselines,
            shared: Arc::new(Shared {
                store: self.store,
                ctx,
                clock: self.clock,
                ttl: self.ttl,
                cooldown: self.timeouts.cooldown,
            }),
            featured,
            max_page_size: self.limits.max_page_size,
        })
    }
}
