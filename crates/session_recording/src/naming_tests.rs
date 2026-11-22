use std::io::BufWriter;

use super::*;
use crate::asciicast::{CastWriter, Header};

fn write_cast(dir: &Path, name: &str, timestamp: i64, title: Option<&str>, last_time: f64) {
    let header = Header {
        timestamp: Some(timestamp),
        title: title.map(str::to_owned),
        ..Header::new(80, 24)
    };
    let file = File::create(dir.join(name)).unwrap();
    let mut writer = CastWriter::new(BufWriter::new(file), &header).unwrap();
    writer.write_output(last_time, b"x").unwrap();
    writer.into_inner().unwrap();
}

#[test]
fn sanitizes_titles() {
    assert_eq!(sanitize_title("zsh: ~/dev/warp"), "zsh-dev-warp");
    assert_eq!(sanitize_title("  ...  "), "session");
    assert_eq!(sanitize_title("a/b\\c:d"), "a-b-c-d");
    assert_eq!(sanitize_title(&"x".repeat(100)).len(), 60);
}

#[test]
fn names_new_recordings_without_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let started_at = Local.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap();
    let first = new_recording_path(dir.path(), started_at, "vim");
    assert_eq!(first.file_name().unwrap(), "2026-03-04_05-06-07-vim.cast");
    File::create(&first).unwrap();
    let second = new_recording_path(dir.path(), started_at, "vim");
    assert_eq!(
        second.file_name().unwrap(),
        "2026-03-04_05-06-07-vim-2.cast"
    );
}

#[test]
fn lists_recordings_newest_first_and_skips_others() {
    let dir = tempfile::tempdir().unwrap();
    write_cast(dir.path(), "old.cast", 1_000, Some("Old"), 1.5);
    write_cast(dir.path(), "new.cast", 2_000, None, 65.);
    fs::write(dir.path().join("notes.txt"), "hi").unwrap();
    fs::write(dir.path().join("broken.cast"), "not json").unwrap();

    let recordings = list_recordings(dir.path()).unwrap();
    let titles: Vec<_> = recordings.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["new", "Old"]);
    assert_eq!(recordings[0].duration, 65.);
    assert_eq!(recordings[0].started_at.unwrap().timestamp(), 2_000);
    assert!(recordings[0].size_bytes > 0);
}

#[test]
fn missing_directory_lists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(list_recordings(&dir.path().join("nope")).unwrap(), vec![]);
}

#[test]
fn renames_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    write_cast(dir.path(), "a.cast", 1, None, 1.);
    write_cast(dir.path(), "b.cast", 1, None, 1.);

    let renamed = rename_recording(&dir.path().join("a.cast"), "My demo.cast").unwrap();
    assert_eq!(renamed, dir.path().join("My-demo.cast"));
    assert!(renamed.exists());

    let error = rename_recording(&renamed, "b").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

    delete_recording(&renamed).unwrap();
    assert!(!renamed.exists());
}

#[test]
fn formats_duration_and_size() {
    assert_eq!(format_duration(65.4), "1:05");
    assert_eq!(format_duration(3723.), "1:02:03");
    assert_eq!(format_size(512), "512 B");
    assert_eq!(format_size(2048), "2.0 KB");
    assert_eq!(format_size(3 * 1024 * 1024), "3.0 MB");
}
