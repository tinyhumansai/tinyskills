//! The in-memory search index over one source's catalog.

use std::cmp::Reverse;
use std::collections::HashMap;

use super::contract::{Facet, RegistryEntry, SkillQuery};
use crate::{CatalogEntry, closest_entry_ids};

pub(crate) const MAX_SUGGESTED_IDS: usize = 5;
const FIELD_SEPARATOR: char = '\u{0}';

#[derive(Debug)]
pub(crate) struct CatalogIndex {
    entries: Vec<RegistryEntry>,
    names: Vec<String>,
    haystacks: Vec<String>,
    upstreams: Vec<Facet>,
    categories: Vec<Facet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Rank {
    featured: usize,
    tier: u8,
    not_installable: bool,
    source: usize,
    position: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Hit {
    pub(crate) rank: Rank,
    pub(crate) position: usize,
}

pub(crate) enum Lookup<'a> {
    Found(&'a RegistryEntry),
    Named(Vec<&'a RegistryEntry>),
}

impl CatalogIndex {
    pub(crate) fn new(entries: Vec<RegistryEntry>) -> Self {
        let names = entries
            .iter()
            .map(|entry| entry.entry.name.to_lowercase())
            .collect();
        let haystacks = entries.iter().map(|entry| haystack(&entry.entry)).collect();
        let mut upstreams: HashMap<String, Facet> = HashMap::new();
        let mut categories: HashMap<String, Facet> = HashMap::new();
        for entry in &entries {
            let upstream = &entry.entry.source;
            if !upstream.is_empty() {
                count_facet(&mut upstreams, upstream, None);
            }
            let category = &entry.entry.category;
            if !category.is_empty() {
                count_facet(&mut categories, category, entry.category_label.as_deref());
            }
        }
        Self {
            entries,
            names,
            haystacks,
            upstreams: sorted_facets(upstreams),
            categories: sorted_facets(categories),
        }
    }

    pub(crate) fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn entry(&self, position: usize) -> Option<&RegistryEntry> {
        self.entries.get(position)
    }

    pub(crate) fn upstream_facets(&self) -> &[Facet] {
        &self.upstreams
    }

    pub(crate) fn category_facets(&self) -> &[Facet] {
        &self.categories
    }

    pub(crate) fn search(
        &self,
        query: &SkillQuery,
        source: usize,
        featured: &HashMap<String, usize>,
    ) -> Vec<Hit> {
        let text = query.text.trim().to_lowercase();
        let mut hits = Vec::new();
        for (position, entry) in self.entries.iter().map(|entry| &entry.entry).enumerate() {
            if !query.upstreams.is_empty()
                && !query
                    .upstreams
                    .iter()
                    .any(|upstream| entry.source.eq_ignore_ascii_case(upstream))
            {
                continue;
            }
            if !query.categories.is_empty()
                && !query
                    .categories
                    .iter()
                    .any(|category| entry.category.eq_ignore_ascii_case(category))
            {
                continue;
            }
            let installable = entry.has_direct_download();
            if query.installable_only && !installable {
                continue;
            }
            let name = &self.names[position];
            let tier = if text.is_empty() || *name == text {
                0
            } else if name.starts_with(&text) {
                1
            } else if name.contains(&text) {
                2
            } else if self.haystacks[position].contains(&text) {
                3
            } else {
                continue;
            };
            hits.push(Hit {
                rank: Rank {
                    featured: featured.get(&entry.id).copied().unwrap_or(usize::MAX),
                    tier,
                    not_installable: !installable,
                    source,
                    position,
                },
                position,
            });
        }
        hits
    }

    pub(crate) fn lookup(&self, id: &str) -> Option<Lookup<'_>> {
        let id = id.trim();
        if let Some(entry) = self.entries.iter().find(|entry| entry.entry.id == id) {
            return Some(Lookup::Found(entry));
        }
        let named: Vec<&RegistryEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.entry.name == id)
            .collect();
        (!named.is_empty()).then_some(Lookup::Named(named))
    }

    pub(crate) fn closest(&self, id: &str) -> Vec<String> {
        let catalog: Vec<CatalogEntry> = self
            .entries
            .iter()
            .map(|entry| entry.entry.clone())
            .collect();
        closest_entry_ids(&catalog, id.trim())
    }
}

pub(crate) fn merge_facets(lists: &[&[Facet]]) -> Vec<Facet> {
    let mut merged: HashMap<String, Facet> = HashMap::new();
    for list in lists {
        for facet in *list {
            let slot = merged
                .entry(facet.value.to_lowercase())
                .or_insert_with(|| Facet {
                    value: facet.value.clone(),
                    label: facet.label.clone(),
                    count: 0,
                });
            slot.count += facet.count;
        }
    }
    sorted_facets(merged)
}

fn count_facet(facets: &mut HashMap<String, Facet>, value: &str, label: Option<&str>) {
    let slot = facets.entry(value.to_lowercase()).or_insert_with(|| Facet {
        value: value.to_owned(),
        label: value.to_owned(),
        count: 0,
    });
    if slot.label == slot.value
        && let Some(label) = label.filter(|label| !label.is_empty())
    {
        label.clone_into(&mut slot.label);
    }
    slot.count += 1;
}

fn sorted_facets(facets: HashMap<String, Facet>) -> Vec<Facet> {
    let mut facets: Vec<Facet> = facets.into_values().collect();
    facets.sort_by(|a, b| {
        Reverse(a.count)
            .cmp(&Reverse(b.count))
            .then_with(|| a.value.cmp(&b.value))
    });
    facets
}

fn haystack(entry: &CatalogEntry) -> String {
    let mut text = String::new();
    for field in [&entry.name, &entry.description, &entry.category]
        .into_iter()
        .chain(entry.tags.iter())
        .chain(entry.author.iter())
    {
        text.push_str(&field.to_lowercase());
        text.push(FIELD_SEPARATOR);
    }
    text
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
