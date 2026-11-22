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
