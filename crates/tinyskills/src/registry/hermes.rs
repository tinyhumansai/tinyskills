//! The aggregated Hermes skills index as a [`SkillSource`].

use std::fmt;

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::contract::{RegistryEntry, Validators};
use super::error::RegistryError;
use super::source::{
    SkillSource, SourceContext, SourceDescriptor, SourceLoad, direct_download_url,
};
use super::transport::BoxFuture;
use crate::{CatalogEntry, SkillsShRef, catalog_entry_id, derive_download_url};

const HERMES_INDEX_URL: &str = "https://hermes-agent.nousresearch.com/docs/api/skills.json";

/// The Hermes skills index: one JSON array aggregating Hermes' built-in and
/// optional skills, `ClawHub`, `skills.sh`, `LobeHub`, browse.sh and
/// GitHub-hosted collections.
///
/// Loads are conditional (`If-None-Match` / `If-Modified-Since`) and parse the
/// array one element at a time, so a large index never exists as a JSON tree.
/// Each entry's [`CatalogEntry`] equals what
/// [`parse_hermes_entry`](crate::parse_hermes_entry) produces. The upstream
/// `installCmd` shell line is dropped.
///
/// Documents resolve as follows: a `skills.sh` entry probes the conventional
/// locations concurrently and falls back to one repository tree listing; a
/// `ClawHub` `409` is [`RegistryError::UpstreamAmbiguous`]; an entry with no
/// `SKILL.md` (`LobeHub`) is [`RegistryError::NoDirectDownload`].
#[derive(Debug, Clone)]
pub struct HermesIndexSource {
    descriptor: SourceDescriptor,
    url: String,
    download_base: Option<String>,
}

impl HermesIndexSource {
    /// A source with registry id `id` reading the index at `url`.
    #[must_use]
    pub fn new(id: impl Into<String>, url: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            descriptor: SourceDescriptor::new(id.clone(), id),
            url: url.into(),
            download_base: None,
        }
    }

    /// The public Hermes index, registry id `hermes`.
    #[must_use]
    pub fn hermes() -> Self {
        Self::new("hermes", HERMES_INDEX_URL).with_label("Hermes Skills Hub")
    }

    /// Set the display label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor.label = label.into();
        self
    }

    /// Serve every `SKILL.md` from `<base>/<name>/SKILL.md`, for mirrors and
    /// tests; see [`derive_download_url`].
    #[must_use]
    pub fn with_download_base(mut self, base: impl Into<String>) -> Self {
        self.download_base = Some(base.into());
        self
    }

    /// The index URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl SkillSource for HermesIndexSource {
    fn descriptor(&self) -> SourceDescriptor {
        self.descriptor.clone()
    }

    fn load<'a>(
        &'a self,
        ctx: &'a SourceContext,
        prior: Option<&'a Validators>,
    ) -> BoxFuture<'a, Result<SourceLoad, RegistryError>> {
        Box::pin(async move {
            let headers = prior
                .map(Validators::conditional_headers)
                .unwrap_or_default();
            let response = ctx
                .get(
                    &self.url,
                    &headers,
                    ctx.limits().max_catalog_bytes,
                    "catalog",
                    ctx.timeouts().catalog,
                )
                .await?;
            if response.status == 304 && prior.is_some() {
                return Ok(SourceLoad::NotModified);
            }
            let response = response.error_for_status()?;
            let validators = Validators::from_headers(&response.headers);
            let max_entries = ctx.limits().max_entries;
            let base = self.download_base.clone();
            let body = response.body;
            let parsed = tokio::task::spawn_blocking(move || {
                parse_hermes_index(&body, max_entries, base.as_deref())
            })
            .await
            .map_err(|error| RegistryError::Malformed {
                what: "catalog",
                detail: error.to_string(),
            })??;
            Ok(SourceLoad::Fresh {
                entries: parsed.entries,
                validators,
                skipped: parsed.skipped,
            })
        })
    }

    fn resolve_document_url<'a>(
        &'a self,
        ctx: &'a SourceContext,
        entry: &'a RegistryEntry,
    ) -> BoxFuture<'a, Result<String, RegistryError>> {
        Box::pin(async move {
            let url = direct_download_url(entry)?;
            match entry
                .entry
                .source_url
                .as_deref()
                .and_then(SkillsShRef::parse)
            {
                Some(skill) if skill.candidate_urls().first() == Some(&url) => {
                    resolve_skills_sh(ctx, entry, &skill).await
                }
                _ => Ok(url),
            }
        })
    }

    fn document_status_error(&self, entry: &RegistryEntry, status: u16) -> Option<RegistryError> {
        (status == 409 && entry.entry.source.eq_ignore_ascii_case("clawhub")).then(|| {
            RegistryError::UpstreamAmbiguous {
                name: entry.entry.name.clone(),
            }
        })
    }
}

async fn resolve_skills_sh(
    ctx: &SourceContext,
    entry: &RegistryEntry,
    skill: &SkillsShRef<'_>,
) -> Result<String, RegistryError> {
    let candidates = skill.candidate_urls();
    let budget = ctx.timeouts().probe;
    let mut probes = tokio::task::JoinSet::new();
    for (position, url) in candidates.iter().enumerate() {
        let ctx = ctx.clone();
        let url = url.clone();
        probes.spawn(async move {
            let found = ctx
                .head(&url, "probe", budget)
                .await
                .is_ok_and(|response| response.is_success());
            (position, found)
        });
    }
    let mut found = vec![false; candidates.len()];
    while let Some(result) = probes.join_next().await {
        if let Ok((position, hit)) = result {
            found[position] = hit;
        }
    }
    if let Some(position) = found.iter().position(|hit| *hit) {
        return Ok(candidates[position].clone());
    }

    let listing = ctx
        .get(
            &skill.tree_api_url(),
            &[(
                "Accept".to_owned(),
                "application/vnd.github+json".to_owned(),
            )],
            ctx.limits().max_tree_listing_bytes,
            "tree listing",
            ctx.timeouts().tree_listing,
        )
        .await?
        .error_for_status()?;
    let tree: serde_json::Value =
        serde_json::from_slice(&listing.body).map_err(|error| RegistryError::Malformed {
            what: "tree listing",
            detail: error.to_string(),
        })?;
    skill.locate_in_tree(&tree).map_err(|miss| match miss {
        crate::TreeMiss::Ambiguous(_) => RegistryError::UpstreamAmbiguous {
            name: entry.entry.name.clone(),
        },
        _ => RegistryError::NoDirectDownload {
            name: entry.entry.name.clone(),
            source_url: entry.entry.source_url.clone(),
        },
    })
}

pub(crate) struct ParsedIndex {
    pub(crate) entries: Vec<RegistryEntry>,
    pub(crate) skipped: usize,
}

pub(crate) fn parse_hermes_index(
    body: &[u8],
    max_entries: usize,
    download_base: Option<&str>,
) -> Result<ParsedIndex, RegistryError> {
    let mut parsed = ParsedIndex {
        entries: Vec::new(),
        skipped: 0,
    };
    let mut exceeded = false;
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let seed = IndexSeed {
        out: &mut parsed,
        exceeded: &mut exceeded,
        max_entries,
        download_base,
    };
    let result = seed
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end());
    if exceeded {
        return Err(RegistryError::TooLarge {
            what: "catalog entries",
            limit: max_entries as u64,
        });
    }
    result.map_err(|error| RegistryError::Malformed {
        what: "catalog",
        detail: error.to_string(),
    })?;
    Ok(parsed)
}

struct IndexSeed<'a> {
    out: &'a mut ParsedIndex,
    exceeded: &'a mut bool,
    max_entries: usize,
    download_base: Option<&'a str>,
}

impl<'de> DeserializeSeed<'de> for IndexSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for IndexSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON array of catalog items")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(MaybeItem(item)) = seq.next_element()? {
            match item.and_then(|item| item.into_entry(self.download_base)) {
                Some(entry) => {
                    if self.out.entries.len() >= self.max_entries {
                        *self.exceeded = true;
                        return Err(de::Error::custom("too many catalog entries"));
                    }
                    self.out.entries.push(entry);
                }
                None => self.out.skipped += 1,
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct HermesItem {
    name: Option<String>,
    description: Option<String>,
    overview: Option<String>,
    source: Option<String>,
    category: Option<String>,
    category_label: Option<String>,
    fixed_category: Option<bool>,
    author: Option<String>,
    version: Option<String>,
    license: Option<String>,
    tags: Vec<String>,
    platforms: Vec<String>,
    commands: Vec<String>,
    env_vars: Vec<String>,
    docs_path: Option<String>,
    source_url: Option<String>,
    identifier: Option<String>,
    install_identifier: Option<String>,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

impl HermesItem {
    fn into_entry(self, download_base: Option<&str>) -> Option<RegistryEntry> {
        let name = self.name?;
        let source = self.source.unwrap_or_else(|| "hermes".to_owned());
        let docs_path = non_empty(self.docs_path);
        let source_url = non_empty(self.source_url);
        let identifier = self
            .identifier
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let download_url = derive_download_url(
            &source,
            identifier,
            &name,
            docs_path.as_deref(),
            source_url.as_deref(),
            download_base,
        );
        let entry = CatalogEntry {
            id: catalog_entry_id(&source, identifier, &name),
            author: self.author,
            version: self.version,
            license: self.license,
            tags: self.tags,
            platforms: self.platforms,
            commands: self.commands,
            env_vars: self.env_vars,
            name,
            description: self.description.unwrap_or_default(),
            source,
            category: self.category.unwrap_or_default(),
            download_url,
            source_url,
            docs_path,
        };
        Some(RegistryEntry {
            entry,
            overview: self.overview.unwrap_or_default(),
            category_label: non_empty(self.category_label),
            fixed_category: self.fixed_category,
            install_identifier: non_empty(self.install_identifier),
        })
    }
}

#[derive(Default)]
struct MaybeItem(Option<HermesItem>);

impl<'de> Deserialize<'de> for MaybeItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ItemVisitor)
    }
}

struct ItemVisitor;

macro_rules! ignore_scalars {
    ($value:ty; $($method:ident: $ty:ty),* $(,)?) => {
        $(
            fn $method<E: de::Error>(self, _: $ty) -> Result<$value, E> {
                Ok(Default::default())
            }
        )*
    };
}

macro_rules! lenient_visitor_common {
    ($value:ty) => {
        ignore_scalars!($value; visit_bool: bool, visit_i64: i64, visit_u64: u64, visit_f64: f64);

        fn visit_unit<E: de::Error>(self) -> Result<$value, E> {
            Ok(Default::default())
        }
    };
}

impl<'de> Visitor<'de> for ItemVisitor {
    type Value = MaybeItem;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a catalog item")
    }

    ignore_scalars!(MaybeItem; visit_bool: bool, visit_i64: i64, visit_u64: u64, visit_f64: f64, visit_str: &str);

    fn visit_unit<E: de::Error>(self) -> Result<MaybeItem, E> {
        Ok(MaybeItem(None))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<MaybeItem, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(MaybeItem(None))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MaybeItem, A::Error> {
        let mut item = HermesItem::default();
        while let Some(key) = map.next_key::<Field>()? {
            match key {
                Field::Name => item.name = map.next_value::<LenientStr>()?.0,
                Field::Description => item.description = map.next_value::<LenientStr>()?.0,
                Field::Overview => item.overview = map.next_value::<LenientStr>()?.0,
                Field::Source => item.source = map.next_value::<LenientStr>()?.0,
                Field::Category => item.category = map.next_value::<LenientStr>()?.0,
                Field::CategoryLabel => item.category_label = map.next_value::<LenientStr>()?.0,
                Field::FixedCategory => item.fixed_category = map.next_value::<LenientBool>()?.0,
                Field::Author => item.author = map.next_value::<LenientStr>()?.0,
                Field::Version => item.version = map.next_value::<LenientStr>()?.0,
                Field::License => item.license = map.next_value::<LenientStr>()?.0,
                Field::Tags => item.tags = map.next_value::<LenientList>()?.0,
                Field::Platforms => item.platforms = map.next_value::<LenientList>()?.0,
                Field::Commands => item.commands = map.next_value::<LenientList>()?.0,
                Field::EnvVars => item.env_vars = map.next_value::<LenientList>()?.0,
                Field::DocsPath => item.docs_path = map.next_value::<LenientStr>()?.0,
                Field::SourceUrl => item.source_url = map.next_value::<LenientStr>()?.0,
                Field::Identifier => item.identifier = map.next_value::<LenientStr>()?.0,
                Field::InstallIdentifier => {
                    item.install_identifier = map.next_value::<LenientStr>()?.0;
                }
                Field::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(MaybeItem(Some(item)))
    }
}

enum Field {
    Name,
    Description,
    Overview,
    Source,
    Category,
    CategoryLabel,
    FixedCategory,
    Author,
    Version,
    License,
    Tags,
    Platforms,
    Commands,
    EnvVars,
    DocsPath,
    SourceUrl,
    Identifier,
    InstallIdentifier,
    Other,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

struct FieldVisitor;

impl Visitor<'_> for FieldVisitor {
    type Value = Field;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a field name")
    }

    fn visit_str<E: de::Error>(self, key: &str) -> Result<Field, E> {
        Ok(match key {
            "name" => Field::Name,
            "description" => Field::Description,
            "overview" => Field::Overview,
            "source" => Field::Source,
            "category" => Field::Category,
            "categoryLabel" => Field::CategoryLabel,
            "fixedCategory" => Field::FixedCategory,
            "author" => Field::Author,
            "version" => Field::Version,
            "license" => Field::License,
            "tags" => Field::Tags,
            "platforms" => Field::Platforms,
            "commands" => Field::Commands,
            "envVars" => Field::EnvVars,
            "docsPath" => Field::DocsPath,
            "sourceUrl" => Field::SourceUrl,
            "identifier" => Field::Identifier,
            "installIdentifier" => Field::InstallIdentifier,
            _ => Field::Other,
        })
    }
}

#[derive(Default)]
struct LenientStr(Option<String>);

impl<'de> Deserialize<'de> for LenientStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LenientStrVisitor)
    }
}

struct LenientStrVisitor;

impl<'de> Visitor<'de> for LenientStrVisitor {
    type Value = LenientStr;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    lenient_visitor_common!(LenientStr);

    fn visit_str<E: de::Error>(self, value: &str) -> Result<LenientStr, E> {
        Ok(LenientStr(Some(value.to_owned())))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<LenientStr, E> {
        Ok(LenientStr(Some(value)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LenientStr, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(LenientStr(None))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LenientStr, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(LenientStr(None))
    }
}

#[derive(Default)]
struct LenientBool(Option<bool>);

impl<'de> Deserialize<'de> for LenientBool {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LenientBoolVisitor)
    }
}

struct LenientBoolVisitor;

impl<'de> Visitor<'de> for LenientBoolVisitor {
    type Value = LenientBool;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    ignore_scalars!(LenientBool; visit_i64: i64, visit_u64: u64, visit_f64: f64, visit_str: &str);

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<LenientBool, E> {
        Ok(LenientBool(Some(value)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<LenientBool, E> {
        Ok(LenientBool(None))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LenientBool, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(LenientBool(None))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LenientBool, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(LenientBool(None))
    }
}

#[derive(Default)]
struct LenientList(Vec<String>);

impl<'de> Deserialize<'de> for LenientList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LenientListVisitor)
    }
}

struct LenientListVisitor;

impl<'de> Visitor<'de> for LenientListVisitor {
    type Value = LenientList;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    lenient_visitor_common!(LenientList);

    fn visit_str<E: de::Error>(self, _: &str) -> Result<LenientList, E> {
        Ok(LenientList::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LenientList, A::Error> {
        let mut values = Vec::new();
        while let Some(LenientStr(value)) = seq.next_element()? {
            values.extend(value);
        }
        Ok(LenientList(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LenientList, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(LenientList::default())
    }
}

#[cfg(test)]
#[path = "hermes_tests.rs"]
mod tests;
