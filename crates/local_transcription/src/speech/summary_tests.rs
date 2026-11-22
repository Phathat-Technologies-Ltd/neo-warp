use super::*;

const FIND: &str = "find . -type d -not -path '*/.git*' -not -path '*/node_modules*' -maxdepth 3";
const RISKY: [&str; 4] = [
    FIND,
    "rm -rf target",
    "git push --force origin main",
    "kubectl apply -f x.yaml",
];

fn assert_no_raw_command(spoken: &str) {
    for fragment in [
        "-",
        "'",
        "*",
        "/",
        "git*",
        "node_modules",
        "yaml",
        "origin",
        "target",
        "maxdepth",
    ] {
        assert!(
            !spoken.contains(fragment),
            "{spoken:?} contains {fragment:?}"
        );
    }
}

#[test]
fn names_only_the_program() {
    assert_eq!(program_name("cargo check -p app").as_deref(), Some("cargo"));
    assert_eq!(program_name("ls -la").as_deref(), Some("ls"));
    assert_eq!(
        program_name("FOO=1 sudo /usr/bin/find .").as_deref(),
        Some("find")
    );
    assert_eq!(program_name("$(which foo)"), None);
}

#[test]
fn describes_commands_without_arguments() {
    assert_eq!(command_description(FIND), "a find command");
    assert_eq!(
        command_description("rm -rf target"),
        "a command that deletes files"
    );
    assert_eq!(
        command_description("git push --force origin main"),
        "a command that force pushes to a git remote"
    );
    assert_eq!(
        command_description("kubectl apply -f x.yaml"),
        "a command that changes your Kubernetes cluster"
    );
    assert_eq!(command_description("ls -la"), "an ls command");
    for command in RISKY {
        assert_no_raw_command(&command_description(command));
        assert_no_raw_command(&command_approval_subject(command));
    }
}

#[test]
fn result_readouts_count_instead_of_reading() {
    let output = (0..14)
        .map(|n| format!("./dir{n}/sub"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        command_result_readout(FIND, Some(0), &output),
        "Ran a find command: 14 folders."
    );
    assert_eq!(
        command_result_readout("cargo check", Some(101), "error: bad\nerror: worse"),
        "The cargo command failed with 2 errors."
    );
    assert_eq!(
        command_result_readout("false", Some(1), ""),
        "The false command failed with exit code 1."
    );
    assert_eq!(
        command_result_readout("pwd", Some(0), "/Users/me/app\n"),
        "Ran a pwd command: it succeeded."
    );
    assert_eq!(
        command_result_readout("rm -rf target", Some(0), ""),
        "Ran a command that deletes files: it succeeded."
    );
    for command in RISKY {
        assert_no_raw_command(&command_result_readout(command, Some(0), "/a/b\n"));
        assert_no_raw_command(&command_result_readout(command, Some(1), "error: /a/b"));
    }
}

#[test]
fn denylisted_readout_does_not_read_the_command() {
    assert_eq!(
        command_denied_readout("rm -rf /"),
        "A command that deletes files is on the deny list, so it didn't run."
    );
}
