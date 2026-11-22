use chrono::{Duration, Local, TimeZone as _};

use super::{CommandRun, format_command_duration};
use crate::terminal::history::HistoryEntry;

#[test]
fn command_duration_is_compact() {
    assert_eq!(
        format_command_duration(Duration::milliseconds(120)),
        "120ms"
    );
    assert_eq!(format_command_duration(Duration::seconds(4)), "4s");
    assert_eq!(format_command_duration(Duration::seconds(123)), "2m 3s");
    assert_eq!(format_command_duration(Duration::minutes(65)), "1h 5m");
    assert_eq!(format_command_duration(Duration::seconds(-3)), "0ms");
}

#[test]
fn command_run_needs_full_block_metadata() {
    assert_eq!(
        CommandRun::from_entry(&HistoryEntry::command_only("ls")),
        None
    );

    let start = Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let mut entry = HistoryEntry::command_only("make");
    entry.start_ts = Some(start);
    entry.completed_ts = Some(start + Duration::seconds(7));
    entry.exit_code = Some(2.into());
    assert_eq!(
        CommandRun::from_entry(&entry),
        Some(CommandRun {
            duration: Duration::seconds(7),
            succeeded: false,
        })
    );

    entry.exit_code = Some(0.into());
    assert!(CommandRun::from_entry(&entry).unwrap().succeeded);
}
