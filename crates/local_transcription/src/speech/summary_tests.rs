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
fn grouped_approval_prompt_counts_commands() {
    assert_eq!(
        grouped_approval_prompt(2),
        "Two commands need your approval. Approve on screen."
    );
    assert_eq!(
        grouped_approval_prompt(12),
        "Several commands need your approval. Approve on screen."
    );
}
