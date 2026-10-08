#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn entry(id: &str, name: &str, description: &str, installable: bool) -> RegistryEntry {
    let mut entry = RegistryEntry::default();
    entry.entry.id = id.to_owned();
    entry.entry.name = name.to_owned();
    entry.entry.description = description.to_owned();
    entry.entry.source = "Up".to_owned();
    entry.entry.category = "cat".to_owned();
    if installable {
        entry.entry.download_url = format!("https://x.test/{id}/SKILL.md");
    }
    entry
}

fn ids(index: &CatalogIndex, query: &SkillQuery, featured: &HashMap<String, usize>) -> Vec<String> {
    let mut hits = index.search(query, 0, featured);
    hits.sort_by_key(|hit| hit.rank);
    hits.iter()
        .map(|hit| index.entry(hit.position).unwrap().entry.id.clone())
        .collect()
}

#[test]
fn ranks_exact_prefix_substring_then_other_fields() {
    let index = CatalogIndex::new(vec![
        entry("other", "zzz", "mentions notes here", true),
        entry("sub", "my-notes", "", true),
        entry("prefix", "notes-pro", "", true),
        entry("exact-off", "Notes", "", false),
        entry("exact", "notes", "", true),
        entry("none", "unrelated", "", true),
    ]);
    let order = ids(&index, &SkillQuery::text("NOTES"), &HashMap::new());
    assert_eq!(order, ["exact", "exact-off", "prefix", "sub", "other"]);

    let featured = HashMap::from([("other".to_owned(), 0), ("sub".to_owned(), 1)]);
    let order = ids(&index, &SkillQuery::text("notes"), &featured);
    assert_eq!(order[..2], ["other", "sub"]);
}

#[test]
fn empty_text_keeps_installable_first_then_catalog_order() {
    let index = CatalogIndex::new(vec![
        entry("a", "a", "", false),
        entry("b", "b", "", true),
        entry("c", "c", "", true),
    ]);
    assert_eq!(
        ids(&index, &SkillQuery::default(), &HashMap::new()),
        ["b", "c", "a"]
    );
}

#[test]
fn matches_the_fields_filter_catalog_matches() {
    let mut tagged = entry("t", "t", "", true);
    tagged.entry.tags = vec!["Rust".to_owned()];
    let mut authored = entry("au", "au", "", true);
    authored.entry.author = Some("Ada".to_owned());
    let mut labelled = entry("l", "l", "", true);
    labelled.category_label = Some("Hidden Label".to_owned());
    let index = CatalogIndex::new(vec![tagged, authored, labelled]);
    assert_eq!(
        ids(&index, &SkillQuery::text("rust"), &HashMap::new()),
        ["t"]
    );
    assert_eq!(
        ids(&index, &SkillQuery::text("ada"), &HashMap::new()),
        ["au"]
    );
    assert_eq!(
        ids(&index, &SkillQuery::text("CAT"), &HashMap::new()).len(),
        3
    );
    assert_eq!(
        ids(&index, &SkillQuery::text("hidden"), &HashMap::new()).len(),
        0
    );
}

#[test]
fn filters_apply_before_ranking() {
    let mut other = entry("o", "o", "", true);
    other.entry.source = "Other".to_owned();
    other.entry.category = "misc".to_owned();
    let index = CatalogIndex::new(vec![entry("u", "u", "", false), other]);
    let mut query = SkillQuery {
        upstreams: vec!["other".to_owned()],
        ..SkillQuery::default()
    };
    assert_eq!(ids(&index, &query, &HashMap::new()), ["o"]);
    query.upstreams.clear();
    query.categories = vec!["CAT".to_owned()];
    assert_eq!(ids(&index, &query, &HashMap::new()), ["u"]);
    query.categories.clear();
    query.installable_only = true;
    assert_eq!(ids(&index, &query, &HashMap::new()), ["o"]);
}

#[test]
fn facets_count_and_label() {
    let mut a = entry("a", "a", "", true);
    a.category_label = Some("Category".to_owned());
    let b = entry("b", "b", "", true);
    let mut c = entry("c", "c", "", true);
    c.entry.source = "Second".to_owned();
    c.entry.category = String::new();
    let index = CatalogIndex::new(vec![b, a, c]);
    assert_eq!(index.len(), 3);
    assert_eq!(index.upstream_facets()[0].value, "Up");
    assert_eq!(index.upstream_facets()[0].count, 2);
    assert_eq!(index.category_facets().len(), 1);
    assert_eq!(index.category_facets()[0].label, "Category");

    let merged = merge_facets(&[index.upstream_facets(), index.upstream_facets()]);
    assert_eq!(merged[0].count, 4);
    assert_eq!(merged[1].value, "Second");
}

#[test]
fn lookup_by_id_then_name() {
    let index = CatalogIndex::new(vec![
        entry("x/one", "dup", "", true),
        entry("x/two", "dup", "", true),
        entry("x/three", "solo", "", true),
    ]);
    assert!(matches!(index.lookup(" x/one "), Some(Lookup::Found(e)) if e.entry.id == "x/one"));
    assert!(matches!(index.lookup("dup"), Some(Lookup::Named(v)) if v.len() == 2));
    assert!(matches!(index.lookup("solo"), Some(Lookup::Named(v)) if v.len() == 1));
    assert!(index.lookup("missing").is_none());
    assert_eq!(index.closest("x three"), ["x/three", "x/one", "x/two"]);
    assert!(index.entry(9).is_none());
}
