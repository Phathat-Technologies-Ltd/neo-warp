//! Short spoken summaries of tool calls and their output. Raw command text (arguments, flags,
//! paths, globs, quoted strings) and raw output are never spoken: at most the program name and a
//! plain description.

use super::tts_text;

/// Something in an agent reply that is summarized rather than read.
pub trait SpokenSummary {
    /// The sentence to speak, or `None` to stay silent.
    fn spoken_summary(&self) -> Option<String>;
}

/// Leading words that wrap the real program rather than name it.
const WRAPPERS: &[&str] = &["sudo", "env", "time", "nohup", "command", "exec", "xargs"];

/// The longest program name spoken; anything longer is likely a generated path or hash.
const MAX_PROGRAM_NAME_CHARS: usize = 20;

fn program_words(command: &str) -> Vec<&str> {
    command
        .split_whitespace()
        .skip_while(|word| word.contains('=') || WRAPPERS.contains(word))
        .collect()
}

/// The program a command runs, such as "find" for `find . -type d`, or `None` when it can't be
/// named safely (a variable, a subshell, a long generated name).
pub fn program_name(command: &str) -> Option<String> {
    let word = *program_words(command).first()?;
    let name = word.rsplit('/').next().unwrap_or(word);
    let speakable = name.chars().count() <= MAX_PROGRAM_NAME_CHARS
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    speakable.then(|| tts_text::normalize(name))
}

fn has_flag(words: &[&str], long: &str, short: char) -> bool {
    words.iter().any(|word| {
        *word == long
            || (word.starts_with('-') && !word.starts_with("--") && word[1..].contains(short))
    })
}

/// A plain description of what a command does, such as "a find command" or "a command that
/// deletes files", without any of its arguments.
pub fn command_description(command: &str) -> String {
    let words = program_words(command);
    let program = words
        .first()
        .map(|word| word.rsplit('/').next().unwrap_or(word));
    let subcommand = words.get(1).copied().unwrap_or_default();
    let effect = match program {
        Some("rm" | "rmdir" | "shred" | "unlink") => Some("deletes files"),
        Some("git") if subcommand == "push" && has_flag(&words, "--force", 'f') => {
            Some("force pushes to a git remote")
        }
        Some("git") if subcommand == "push" => Some("pushes to a git remote"),
        Some("git") if subcommand == "reset" && words.contains(&"--hard") => {
            Some("discards git changes")
        }
        Some("git") if subcommand == "clean" => Some("deletes untracked files"),
        Some("kubectl") if matches!(subcommand, "apply" | "create" | "replace" | "patch") => {
            Some("changes your Kubernetes cluster")
        }
        Some("kubectl") if subcommand == "delete" => {
            Some("deletes resources from your Kubernetes cluster")
        }
        Some("dd" | "mkfs") => Some("overwrites a disk"),
        Some("chmod" | "chown") => Some("changes file permissions"),
        Some("mv") => Some("moves files"),
        _ => None,
    };
    if let Some(effect) = effect {
        return format!("a command that {effect}");
    }
    match program_name(command) {
        Some(name) => format!("{} {name} command", article(&name)),
        None => "a command".to_owned(),
    }
}

fn article(word: &str) -> &'static str {
    let lower = word.to_ascii_lowercase();
    // Short names without vowels are spelled out, so "ls" is "an L S".
    let spelled = lower.len() <= 3 && !lower.contains(['a', 'e', 'i', 'o', 'u']);
    match lower.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        Some('f' | 'h' | 'l' | 'm' | 'n' | 'r' | 's' | 'x') if spelled => "an",
        _ => "a",
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What to say when a command finishes, for example "Ran a find command: 14 folders." or "The
/// cargo command failed with 2 errors." The output itself is never read out, only counted.
pub fn command_result_readout(command: &str, exit_code: Option<i32>, output: &str) -> String {
    let errors = count_lines(output, &["error:", "error[", "FAILED", "panicked"]);
    let warnings = count_lines(output, &["warning:"]);
    let failed = errors > 0 || !matches!(exit_code, Some(0) | None);
    let description = command_description(command);
    if failed {
        let subject = match program_name(command) {
            Some(name) if description.ends_with(" command") => format!("The {name} command"),
            _ => "The command".to_owned(),
        };
        let detail = match (errors, warnings, exit_code) {
            (0, 0, Some(code)) => format!("exit code {code}"),
            (0, _, _) => "an error".to_owned(),
            (errors, 0, _) => plural(errors, "error"),
            (errors, warnings, _) => {
                format!(
                    "{} and {}",
                    plural(errors, "error"),
                    plural(warnings, "warning")
                )
            }
        };
        return format!("{subject} failed with {detail}.");
    }
    let lines = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let words = program_words(command);
    let noun = match program_name(command).as_deref() {
        Some("find" | "fd") => match words
            .iter()
            .position(|word| *word == "-type")
            .and_then(|index| words.get(index + 1))
        {
            Some(&"d") => Some("folder"),
            Some(&"f") => Some("file"),
            _ => Some("result"),
        },
        Some("grep" | "rg") => Some("match"),
        _ => None,
    };
    let detail = match noun {
        _ if warnings > 0 => plural(warnings, "warning"),
        Some(noun) => plural_noun(lines, noun),
        None => "it succeeded".to_owned(),
    };
    format!("Ran {description}: {detail}.")
}

/// What the agent asks to do when it wants to run `command`, naming only what it does.
pub fn command_approval_subject(command: &str) -> String {
    format!("run {}", command_description(command))
}

/// The spoken note for a denylisted command.
pub fn command_denied_readout(command: &str) -> String {
    format!(
        "{} is on the deny list, so it didn't run.",
        capitalize(&command_description(command))
    )
}

fn count_lines(output: &str, needles: &[&str]) -> usize {
    output
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            needles.iter().any(|needle| line.starts_with(needle))
        })
        .count()
}

fn plural_noun(count: usize, noun: &str) -> String {
    let plural = if noun.ends_with("ch") || noun.ends_with('s') {
        format!("{noun}es")
    } else {
        format!("{noun}s")
    };
    match count {
        0 => format!("no {plural}"),
        1 => format!("1 {noun}"),
        count => format!("{count} {plural}"),
    }
}

/// "1 error", "3 errors".
pub fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}

/// A kind of tool call, for grouped progress lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressKind {
    Read,
    Search,
    Command,
    Edit,
    Other,
}

/// One spoken line for a batch of tool calls, each with how many items it covers, such as
/// "Looking at three files." A batch of mixed kinds gets a generic line.
pub fn progress_line(batch: &[(ProgressKind, usize)]) -> Option<String> {
    let (first, _) = batch.first()?;
    if batch.iter().any(|(kind, _)| kind != first) {
        return Some("Checking a few things.".to_owned());
    }
    let count = batch.iter().map(|(_, count)| count).sum::<usize>();
    let line = match first {
        ProgressKind::Read => format!("Looking at {}.", counted(count, "file")),
        ProgressKind::Search => "Searching the project.".to_owned(),
        ProgressKind::Command => format!("Running {}.", counted(count, "command")),
        ProgressKind::Edit => format!("Editing {}.", counted(count, "file")),
        ProgressKind::Other => "Working on it.".to_owned(),
    };
    Some(line)
}

/// The one prompt for several approvals that came in together, such as "Two commands need your
/// approval. Approve on screen."
pub fn grouped_approval_prompt(count: usize) -> String {
    let subject = counted(count, "command");
    let mut chars = subject.chars();
    let subject = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => subject,
    };
    let verb = if count > 1 { "need" } else { "needs" };
    format!("{subject} {verb} your approval. Approve on screen.")
}

/// "a file", "three files", "several files".
fn counted(count: usize, noun: &str) -> String {
    const WORDS: [&str; 9] = [
        "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    match count {
        0 | 1 => format!("a {noun}"),
        count => match WORDS.get(count - 2) {
            Some(word) => format!("{word} {noun}s"),
            None => format!("several {noun}s"),
        },
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
