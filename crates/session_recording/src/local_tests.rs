use std::io::Cursor;

use super::*;
use crate::asciicast::{EventData, Header};

fn request(name: &str) -> CreateRecording {
    CreateRecording {
        header: Header {
            timestamp: Some(1_700_000_000),
            title: Some("zsh: ~/dev".to_owned()),
            ..Header::new(80, 24)
        },
        name: name.to_owned(),
    }
}

#[test]
fn round_trips_a_recording() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalDirStore::new(dir.path().to_owned());
    let id = store.create("alice", &request("zsh")).unwrap();

    store
        .append("alice", &id, 0, "[0.5,\"o\",\"hi \"]\n")
        .unwrap();
    // A resent batch is acknowledged but not written twice.
    store
        .append("alice", &id, 0, "[0.5,\"o\",\"hi \"]\n")
        .unwrap();
    store
        .append("alice", &id, 1, "[1.25,\"o\",\"there\"]")
        .unwrap();
    assert!(matches!(
        store.append("alice", &id, 3, "").unwrap_err(),
        StoreError::OutOfOrder { expected: 2 }
    ));
    assert!(matches!(
        store.append("alice", &id, 2, "not json\n").unwrap_err(),
        StoreError::Invalid(_)
    ));

    let finished = store.finish("alice", &id).unwrap();
    assert_eq!(finished.title, "zsh: ~/dev");
    assert_eq!(finished.duration, 1.25);
    assert_eq!(finished.started_at, Some(1_700_000_000));
    assert!(
        finished
            .local_path
            .as_ref()
            .unwrap()
            .starts_with(dir.path())
    );

    let content = store.read("alice", &id, None).unwrap();
    assert_eq!(content.total_len, finished.size_bytes);
    let cast = asciicast::parse(Cursor::new(content.bytes)).unwrap();
    let data: Vec<_> = cast.events.into_iter().map(|event| event.data).collect();
    assert_eq!(
        data,
        [
            EventData::Output("hi ".to_owned()),
            EventData::Output("there".to_owned())
        ]
    );

    assert_eq!(store.list("alice").unwrap(), vec![finished]);
}

#[test]
fn reads_byte_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalDirStore::new(dir.path().to_owned());
    let id = store.create("alice", &request("r")).unwrap();
    let total = store.read("alice", &id, None).unwrap().total_len;

    let head = store
        .read(
            "alice",
            &id,
            Some(ByteRange::parse_header("bytes=0-1").unwrap()),
        )
        .unwrap();
    assert_eq!(head.bytes, b"{\"");
    let tail = store
        .read(
            "alice",
            &id,
            Some(ByteRange::parse_header("bytes=2-").unwrap()),
        )
        .unwrap();
    assert_eq!(tail.start, 2);
    assert_eq!(tail.bytes.len() as u64, total - 2);
    assert!(matches!(
        store.read(
            "alice",
            &id,
            Some(ByteRange {
                start: total,
                end: None
            })
        ),
        Err(StoreError::RangeNotSatisfiable { .. })
    ));
    assert_eq!(ByteRange::parse_header("bytes=5-2"), None);
    assert_eq!(ByteRange::parse_header("items=0-1"), None);
}

#[test]
fn renames_deletes_and_isolates_owners() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalDirStore::new(dir.path().to_owned());
    let a = store.create("alice", &request("a")).unwrap();
    let b = store.create("alice", &request("b")).unwrap();

    assert!(store.list("bob").unwrap().is_empty());
    assert!(matches!(store.get("bob", &a), Err(StoreError::NotFound)));
    assert!(matches!(store.delete("bob", &a), Err(StoreError::NotFound)));

    let renamed = store.rename("alice", &a, "My demo.cast").unwrap();
    assert_eq!(renamed.id, "My-demo");
    assert!(matches!(
        store.rename("alice", &b, "My demo"),
        Err(StoreError::AlreadyExists(_))
    ));

    store.delete("alice", &renamed.id).unwrap();
    assert!(matches!(
        store.get("alice", &renamed.id),
        Err(StoreError::NotFound)
    ));
    assert_eq!(store.list("alice").unwrap().len(), 1);
}

#[test]
fn rejects_ids_outside_the_owner_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalDirStore::new(dir.path().to_owned());
    for id in ["", "..", "../x", "a/b", ".hidden"] {
        assert!(matches!(store.get("alice", id), Err(StoreError::NotFound)));
    }
}

#[test]
fn follows_root_changes() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let store = LocalDirStore::new(first.path().to_owned());
    store.create("alice", &request("x")).unwrap();
    store.set_root(second.path().to_owned());
    assert!(store.list("alice").unwrap().is_empty());
}
