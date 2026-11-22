use super::*;

#[test]
fn names_commands_by_their_leading_words() {
    assert_eq!(command_name("cargo check -p app"), "cargo check");
    assert_eq!(command_name("ls -la"), "ls");
    assert_eq!(command_name("git status | head"), "git status");
}

#[test]
fn summarizes_command_output_without_reading_it() {
    let output = "warning: unused\nerror[E0308]: mismatched\n  error: second\nok";
    assert_eq!(
        command_result_summary("cargo check -p app", Some(101), output),
        "Ran cargo check: 2 errors, 1 warning."
    );
    assert_eq!(
        command_result_summary("cargo check", Some(0), "Finished"),
        "Ran cargo check: it succeeded."
    );
    assert_eq!(
        command_result_summary("false", Some(1), ""),
        "Ran false: it failed with exit code 1."
    );
}

#[test]
fn reads_short_output_and_summarizes_long_output() {
    let readout = command_result_readout("git branch --show-current", Some(0), "main\n");
    assert!(readout.starts_with("Ran git branch: "), "{readout}");
    assert!(readout.contains("main"), "{readout}");
    let long = (0..50)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        command_result_readout("cargo check", Some(0), &long),
        "Ran cargo check: it succeeded."
    );
    assert_eq!(
        command_result_readout("cargo check", Some(101), "error: bad\nerror: worse"),
        "Ran cargo check: 2 errors."
    );
    assert_eq!(
        command_result_readout("true", Some(0), ""),
        "Ran true: it succeeded."
    );
}
