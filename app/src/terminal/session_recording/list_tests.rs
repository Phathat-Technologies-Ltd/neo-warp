use anyhow::anyhow;

use super::*;

fn recording(id: &str, started_at: i64, in_progress: bool) -> RecordingMetadata {
    RecordingMetadata {
        id: id.to_owned(),
        title: id.to_owned(),
        started_at: Some(started_at),
        duration: 65.,
        size_bytes: 2048,
        local_path: None,
        in_progress,
    }
}

fn ids(list: &RecordingsList) -> Vec<&str> {
    list.rows().iter().map(|row| row.info.id.as_str()).collect()
}

#[test]
fn lists_newest_first_and_reports_errors() {
    let mut list = RecordingsList::default();
    list.set_listing(Ok(vec![
        recording("old", 100, false),
        recording("new", 300, true),
        recording("mid", 200, false),
    ]));
    assert_eq!(ids(&list), vec!["new", "mid", "old"]);
    assert!(list.error().is_none());
    assert!(list.rows()[1].details().contains("1:05"));

    list.set_listing(Err(anyhow!("offline")));
    assert!(list.is_empty());
    assert_eq!(list.error(), Some("Couldn't list recordings: offline"));
}

#[test]
fn delete_needs_confirmation() {
    let mut list = RecordingsList::default();
    list.set_listing(Ok(vec![recording("a", 2, false), recording("b", 1, false)]));
    assert_eq!(list.request_delete(0), None);
    assert!(list.is_confirming_delete(0));
    assert_eq!(list.request_delete(1), None);
    assert!(!list.is_confirming_delete(0));
    assert_eq!(list.request_delete(1), Some("b".to_owned()));
    assert_eq!(list.request_delete(5), None);
}

#[test]
fn in_progress_recordings_cant_be_renamed() {
    let mut list = RecordingsList::default();
    list.set_listing(Ok(vec![
        recording("live", 2, true),
        recording("done", 1, false),
    ]));
    assert_eq!(list.start_rename(0), None);
    assert_eq!(list.start_rename(1), Some("done".to_owned()));
    assert_eq!(list.renaming(), Some(1));
    assert_eq!(list.finish_rename(), Some("done".to_owned()));
    assert_eq!(list.renaming(), None);

    list.start_rename(1);
    list.set_listing(Ok(vec![]));
    assert_eq!(list.finish_rename(), None);
}
