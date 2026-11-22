use super::*;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn output_parsing_keeps_fragment_files_in_order_once() {
    let output = format!(
        "-- query 1/1: config --\n1. ./{B}.txt:3-9 (0.91)\n2. {A}.txt\n3. {B}.txt:12\nnotes.txt\n"
    );
    assert_eq!(fragment_hashes_in_output(&output), [B, A]);
}

#[test]
fn a_missing_binary_is_an_error() {
    let zg = ZvecGrep::new("definitely-not-an-installed-zg".into());
    assert!(zg.search(Path::new("."), "query", 5).is_err());
}

#[cfg(unix)]
#[test]
fn search_runs_zg_in_the_workspace() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zg");
    std::fs::write(
        &script,
        format!("#!/bin/sh\n[ \"$1\" = --hybrid ] && echo \"$(pwd)/{A}.txt:1\"\nexit 0\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let zg = ZvecGrep::new(script.into_os_string());
    zg.index(dir.path()).unwrap();
    assert_eq!(zg.search(dir.path(), "query", 5).unwrap(), [A]);
}

#[cfg(unix)]
#[test]
fn a_hung_zg_is_killed_after_its_timeout() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zg");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let timeout = Duration::from_millis(200);
    let zg = ZvecGrep::with_timeouts(script.into_os_string(), timeout, timeout);
    let started = Instant::now();
    assert!(zg.index(dir.path()).is_err());
    assert!(zg.search(dir.path(), "query", 5).is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}
