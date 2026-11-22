use super::{HistoryScope, HistorySource, is_warp_internal_command};
use crate::terminal::HistoryEntry;
use crate::terminal::model::session::SessionId;

fn entry(session_id: Option<SessionId>, pwd: Option<&str>) -> HistoryEntry {
    let mut entry = HistoryEntry::command_only("ls");
    entry.session_id = session_id;
    entry.pwd = pwd.map(str::to_owned);
    entry
}

#[test]
fn global_and_host_keep_every_candidate() {
    let current = SessionId::from(1);
    for scope in [HistoryScope::Global, HistoryScope::Host] {
        assert!(scope.includes(&entry(None, None), current, None));
        assert!(scope.includes(
            &entry(Some(SessionId::from(2)), Some("/tmp")),
            current,
            None
        ));
    }
}

#[test]
fn session_keeps_only_current_session_entries() {
    let current = SessionId::from(1);
    let scope = HistoryScope::Session;
    assert!(scope.includes(&entry(Some(current), None), current, None));
    assert!(!scope.includes(&entry(Some(SessionId::from(2)), None), current, None));
    assert!(
        !scope.includes(&entry(None, None), current, None),
        "history file lines have no session"
    );
}

#[test]
fn directory_matches_pwd_ignoring_trailing_separator() {
    let current = SessionId::from(1);
    let scope = HistoryScope::Directory;
    assert!(scope.includes(&entry(None, Some("/repo")), current, Some("/repo/")));
    assert!(scope.includes(
        &entry(Some(SessionId::from(2)), Some("/repo/")),
        current,
        Some("/repo")
    ));
    assert!(!scope.includes(&entry(None, Some("/repo/sub")), current, Some("/repo")));
    assert!(scope.includes(&entry(None, Some("/")), current, Some("/")));
}

#[test]
fn directory_excludes_entries_or_sessions_without_a_pwd() {
    let current = SessionId::from(1);
    let scope = HistoryScope::Directory;
    assert!(!scope.includes(&entry(None, None), current, Some("/repo")));
    assert!(!scope.includes(&entry(None, Some("/repo")), current, None));
}

#[test]
fn next_cycles_through_all_scopes_in_order() {
    let mut scope = HistoryScope::ALL[0];
    for expected in HistoryScope::ALL.into_iter().cycle().skip(1).take(4) {
        scope = scope.next();
        assert_eq!(scope, expected);
    }
    assert_eq!(scope, HistoryScope::Global);
}

#[test]
fn warp_shell_integration_helpers_are_internal() {
    for command in [
        "warp_run_external_ctrl_r_widget 0:",
        "warp_run_external_ctrl_t_widget",
        "_warp_execute_command foo",
        "__warp_on_exit",
        "  warp_precmd",
    ] {
        assert!(is_warp_internal_command(command), "{command}");
    }
    for command in [
        "warp",
        "warp_",
        "ls warp_precmd",
        "echo warp_x",
        "cargo run",
        "",
    ] {
        assert!(!is_warp_internal_command(command), "{command}");
    }
}

#[test]
fn source_filters_by_who_ran_the_command() {
    let mine = HistoryEntry::command_only("ls");
    let mut agents = HistoryEntry::command_only("ls");
    agents.is_agent_executed = true;
    assert!(HistorySource::All.includes(&mine) && HistorySource::All.includes(&agents));
    assert!(HistorySource::Me.includes(&mine) && !HistorySource::Me.includes(&agents));
    assert!(!HistorySource::Agent.includes(&mine) && HistorySource::Agent.includes(&agents));
}

#[test]
fn source_next_cycles_through_all() {
    let mut source = HistorySource::default();
    for expected in [HistorySource::Me, HistorySource::Agent, HistorySource::All] {
        source = source.next();
        assert_eq!(source, expected);
    }
}
