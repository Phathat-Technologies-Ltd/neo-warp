use super::*;

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

#[test]
fn nodes_need_known_children_and_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::new(dir.path().to_owned());
    let namespace = store.namespace("user", "VOYAGE_CODE_3_512").unwrap();
    assert!(!namespace.add_node(&hash('1'), vec![hash('a')]).unwrap());
    namespace.add_fragment(&hash('a'), "fn a() {}").unwrap();
    assert!(namespace.add_node(&hash('1'), vec![hash('a')]).unwrap());

    let mut reopened = IndexStore::new(dir.path().to_owned());
    let namespace = reopened.namespace("user", "VOYAGE_CODE_3_512").unwrap();
    assert!(namespace.knows(&hash('1')));
    assert_eq!(namespace.fragment(&hash('a')).as_deref(), Some("fn a() {}"));
    assert_eq!(namespace.fragments_under(&hash('1')), [hash('a')]);
}

#[test]
fn users_and_configs_are_separate() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::new(dir.path().to_owned());
    store
        .namespace("ada", "VOYAGE_CODE_3_512")
        .unwrap()
        .add_fragment(&hash('a'), "x")
        .unwrap();
    assert!(
        !store
            .namespace("bob", "VOYAGE_CODE_3_512")
            .unwrap()
            .knows(&hash('a'))
    );
    assert!(
        !store
            .namespace("ada", "VOYAGE_4_512")
            .unwrap()
            .knows(&hash('a'))
    );
}

#[test]
fn malformed_hashes_and_configs_never_reach_the_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = IndexStore::new(dir.path().to_owned());
    assert!(store.namespace("ada", "../escape").is_err());
    let namespace = store.namespace("ada", "VOYAGE_CODE_3_512").unwrap();
    assert!(namespace.add_fragment("../../etc/passwd", "x").is_err());
    assert!(namespace.add_fragment(&hash('A'), "x").is_err());
}
