use super::*;

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[test]
fn nodes_need_known_children_and_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let fragment = sha("fn a() {}");
    let mut store = IndexStore::new(dir.path().to_owned());
    let handle = store.namespace("user", "VOYAGE_CODE_3_512").unwrap();
    let mut namespace = handle.state.lock().unwrap();
    assert!(
        !namespace
            .add_node(&hash('1'), vec![fragment.clone()])
            .unwrap()
    );
    namespace.add_fragment(&fragment, "fn a() {}").unwrap();
    assert!(
        namespace
            .add_node(&hash('1'), vec![fragment.clone()])
            .unwrap()
    );

    let mut reopened = IndexStore::new(dir.path().to_owned());
    let handle = reopened.namespace("user", "VOYAGE_CODE_3_512").unwrap();
    let namespace = handle.state.lock().unwrap();
    assert!(namespace.knows(&hash('1')));
    assert_eq!(namespace.fragment(&fragment).as_deref(), Some("fn a() {}"));
    assert_eq!(namespace.fragments_under(&hash('1')), [fragment]);
}

#[test]
fn users_and_configs_are_separate() {
    let dir = tempfile::tempdir().unwrap();
    let fragment = sha("x");
    let mut store = IndexStore::new(dir.path().to_owned());
    let knows = |store: &mut IndexStore, uid, config| {
        let handle = store.namespace(uid, config).unwrap();
        handle.state.lock().unwrap().knows(&fragment)
    };
    store
        .namespace("ada", "VOYAGE_CODE_3_512")
        .unwrap()
        .state
        .lock()
        .unwrap()
        .add_fragment(&fragment, "x")
        .unwrap();
    assert!(knows(&mut store, "ada", "VOYAGE_CODE_3_512"));
    assert!(!knows(&mut store, "bob", "VOYAGE_CODE_3_512"));
    assert!(!knows(&mut store, "ada", "VOYAGE_4_512"));
}

#[test]
fn malformed_hashes_and_configs_never_reach_the_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::new(dir.path().to_owned());
    assert!(store.namespace("ada", "../escape").is_err());
    let handle = store.namespace("ada", "VOYAGE_CODE_3_512").unwrap();
    let mut namespace = handle.state.lock().unwrap();
    assert!(namespace.add_fragment("../../etc/passwd", "x").is_err());
    assert!(
        namespace
            .add_fragment(&sha("x").to_uppercase(), "x")
            .is_err()
    );
}

#[test]
fn fragments_whose_hash_doesnt_match_their_content_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::new(dir.path().to_owned());
    let handle = store.namespace("ada", "VOYAGE_CODE_3_512").unwrap();
    let mut namespace = handle.state.lock().unwrap();
    assert!(namespace.add_fragment(&sha("x"), "y").is_err());
    assert!(!namespace.knows(&sha("x")));
    assert_eq!(namespace.generation(), 0);
}

#[test]
fn only_finished_fragments_are_in_the_zvec_grep_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::new(dir.path().to_owned());
    let handle = store.namespace("ada", "VOYAGE_CODE_3_512").unwrap();
    let mut namespace = handle.state.lock().unwrap();
    namespace.add_fragment(&sha("x"), "x").unwrap();
    let names: Vec<_> = fs::read_dir(namespace.fragments_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [format!("{}.txt", sha("x"))]);
    assert!(namespace.needs_reindex());
    let generation = namespace.generation();
    namespace.mark_indexed(generation);
    assert!(!namespace.needs_reindex());
}
