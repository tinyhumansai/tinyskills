#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn catalog() -> StoredCatalog {
    let mut entry = RegistryEntry::default();
    entry.entry.id = "a".to_owned();
    entry.entry.name = "a".to_owned();
    entry.overview = "long".to_owned();
    let validators = Validators {
        etag: Some("\"e\"".to_owned()),
        ..Validators::default()
    };
    StoredCatalog::new(vec![entry], 42, validators, 3)
}

#[tokio::test]
async fn memory_store_round_trips() {
    let store = MemoryCatalogStore::new();
    assert_eq!(store.load("r").await.unwrap(), None);
    store.save("r", &catalog()).await.unwrap();
    assert_eq!(store.load("r").await.unwrap(), Some(catalog()));
    let shared = std::sync::Arc::new(store);
    assert_eq!(shared.load("r").await.unwrap(), Some(catalog()));
    shared.save("s", &catalog()).await.unwrap();
}

#[tokio::test]
async fn file_store_writes_atomically_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCatalogStore::new(dir.path().join("nested"));
    assert_eq!(store.load("example").await.unwrap(), None);
    store.save("example", &catalog()).await.unwrap();
    store.save("example", &catalog()).await.unwrap();
    assert_eq!(store.load("example").await.unwrap(), Some(catalog()));
    let names: Vec<_> = std::fs::read_dir(dir.path().join("nested"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["example.json"]);
}

#[tokio::test]
async fn file_store_refuses_unsafe_ids() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCatalogStore::new(dir.path());
    for id in ["", "..", "a/b", "a b"] {
        assert_eq!(
            store.load(id).await,
            Err(StoreError::InvalidId(id.to_owned()))
        );
        assert_eq!(
            store.save(id, &catalog()).await,
            Err(StoreError::InvalidId(id.to_owned()))
        );
    }
}

#[tokio::test]
async fn file_store_reads_other_formats_as_absent_and_reports_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCatalogStore::new(dir.path());
    let path = dir.path().join("r.json");
    std::fs::write(&path, br#"{"format": 99, "entries": []}"#).unwrap();
    assert_eq!(store.load("r").await.unwrap(), None);
    std::fs::write(&path, br#"{"entries": []}"#).unwrap();
    assert_eq!(store.load("r").await.unwrap(), None);
    std::fs::write(&path, b"{ nope").unwrap();
    assert!(matches!(store.load("r").await, Err(StoreError::Corrupt(_))));
    std::fs::write(&path, br#"{"format": 1, "entries": 5}"#).unwrap();
    assert!(matches!(store.load("r").await, Err(StoreError::Corrupt(_))));
}

#[tokio::test]
async fn file_store_caps_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCatalogStore::new(dir.path()).with_max_bytes(16);
    store.save("r", &catalog()).await.unwrap();
    assert_eq!(
        store.load("r").await,
        Err(StoreError::TooLarge { limit: 16 })
    );
}

#[tokio::test]
async fn file_store_reads_with_an_unbounded_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCatalogStore::new(dir.path()).with_max_bytes(u64::MAX);
    store.save("r", &catalog()).await.unwrap();
    assert_eq!(store.load("r").await.unwrap(), Some(catalog()));
}

#[cfg(unix)]
#[tokio::test]
async fn file_store_refuses_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.json");
    std::fs::write(&target, serde_json::to_vec(&catalog()).unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("r.json")).unwrap();
    let store = FileCatalogStore::new(dir.path());
    assert_eq!(store.load("r").await, Err(StoreError::Symlink));
}

#[test]
fn clocks_tell_time() {
    let system: &dyn Clock = &SystemClock;
    let shared: &dyn Clock = &std::sync::Arc::new(SystemClock);
    assert!(system.now() > SystemTime::UNIX_EPOCH);
    assert!(shared.now() > SystemTime::UNIX_EPOCH);
    assert_eq!(StoredCatalog::default().format, StoredCatalog::FORMAT);
}

#[cfg(unix)]
#[tokio::test]
async fn file_store_refuses_a_symlinked_root() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let store = FileCatalogStore::new(&link);
    assert_eq!(store.save("r", &catalog()).await, Err(StoreError::Symlink));
    assert_eq!(store.load("r").await, Err(StoreError::Symlink));
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}

#[test]
fn shared_clocks_delegate() {
    struct Fixed(SystemTime);
    impl Clock for Fixed {
        fn now(&self) -> SystemTime {
            self.0
        }
    }
    let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(7);
    assert_eq!(std::sync::Arc::new(Fixed(at)).now(), at);
}

#[cfg(unix)]
#[test]
fn the_store_handle_refuses_a_symlinked_root_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(open_store_dir(&link, false).is_err());
    assert!(open_store_dir(&link, true).is_err());
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn file_store_follows_a_symlinked_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let store = FileCatalogStore::new(link.join("store"));
    store.save("r", &catalog()).await.unwrap();
    assert_eq!(store.load("r").await.unwrap(), Some(catalog()));
    assert!(real.join("store").join("r.json").is_file());
}

#[cfg(unix)]
#[test]
fn reads_refuse_a_symlinked_file_through_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.json"), b"{}").unwrap();
    std::os::unix::fs::symlink("target.json", dir.path().join("r.json")).unwrap();
    let root = open_store_dir(dir.path(), false).unwrap().unwrap();
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    assert!(root.open_with("r.json", &options).is_err());
}

#[tokio::test]
async fn file_store_accepts_a_root_with_no_file_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let root = dir.path().join("sub").join("..");
    assert_eq!(root.file_name(), None);
    let store = FileCatalogStore::new(&root);
    store.save("r", &catalog()).await.unwrap();
    assert_eq!(store.load("r").await.unwrap(), Some(catalog()));
    assert!(dir.path().join("r.json").is_file());
    assert!(
        open_store_dir(&dir.path().join("missing").join(".."), false)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn file_store_loads_nothing_under_a_missing_parent() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCatalogStore::new(dir.path().join("missing").join("store"));
    assert_eq!(store.load("r").await.unwrap(), None);
    assert!(!dir.path().join("missing").exists());
}

#[test]
fn a_relative_single_component_root_opens_against_the_working_directory() {
    let name = format!("tinyskills-store-{}", std::process::id());
    assert!(open_store_dir(Path::new(&name), false).unwrap().is_none());
}

#[tokio::test]
async fn a_failed_rename_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("r.json");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"keep").unwrap();
    let store = FileCatalogStore::new(dir.path());
    assert!(matches!(
        store.save("r", &catalog()).await,
        Err(StoreError::Io(_))
    ));
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["r.json"]);
    assert_eq!(std::fs::read(blocker.join("keep")).unwrap(), b"keep");
}

#[tokio::test]
async fn a_store_dir_that_is_a_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain");
    std::fs::write(&file, b"x").unwrap();
    let store = FileCatalogStore::new(&file);
    assert!(matches!(store.load("r").await, Err(StoreError::Io(_))));
    assert!(matches!(
        store.save("r", &catalog()).await,
        Err(StoreError::Io(_))
    ));
}
