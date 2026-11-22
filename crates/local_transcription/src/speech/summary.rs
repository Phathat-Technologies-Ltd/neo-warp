//! Short spoken summaries of tool calls and their output, which are never read out in full.

use super::tts_text;

/// Something in an agent reply that is summarized rather than read.
pub trait SpokenSummary {
    /// The sentence to speak, or `None` to stay silent.
    fn spoken_summary(&self) -> Option<String>;
}

/// The words of a command that name it, such as "cargo check" for `cargo check -p app`.
pub fn command_name(command: &str) -> String {
    let words: Vec<&str> = command
        .split_whitespace()
        .take_while(|word| !word.starts_with('-') && !matches!(*word, "|" | "&&" | ";" | ">"))
        .take(2)
        .collect();
    tts_text::normalize(&words.join(" "))
}

/// Summarizes a finished command, for example "Ran cargo check: 2 errors."
pub fn command_result_summary(command: &str, exit_code: Option<i32>, output: &str) -> String {
    let name = command_name(command);
    let errors = count_lines(output, &["error:", "error[", "FAILED", "panicked"]);
    let warnings = count_lines(output, &["warning:"]);
    let detail = match (errors, warnings, exit_code) {
        (0, 0, Some(0) | None) => "it succeeded".to_owned(),
        (0, 0, Some(code)) => format!("it failed with exit code {code}"),
        (errors, 0, _) if errors > 0 => plural(errors, "error"),
        (0, warnings, _) => plural(warnings, "warning"),
        (errors, warnings, _) => {
            format!(
                "{}, {}",
                plural(errors, "error"),
                plural(warnings, "warning")
            )
        }
    };
    format!("Ran {name}: {detail}.")
}

/// Output at most this long (in lines and characters) is read aloud rather than summarized.
const MAX_READ_OUTPUT_LINES: usize = 2;
const MAX_READ_OUTPUT_CHARS: usize = 120;

/// What to say when a command finishes: short successful output is read out ("Ran pwd:
/// /Users/me/app."); anything longer, or a failure, only gets [`command_result_summary`].
pub fn command_result_readout(command: &str, exit_code: Option<i32>, output: &str) -> String {
    let trimmed = output.trim();
    let lines = trimmed.lines().count();
    let short = !trimmed.is_empty()
        && lines <= MAX_READ_OUTPUT_LINES
        && trimmed.chars().count() <= MAX_READ_OUTPUT_CHARS;
    let summary = command_result_summary(command, exit_code, output);
    if short && matches!(exit_code, Some(0) | None) && summary.ends_with(": it succeeded.") {
        let spoken = tts_text::normalize(&trimmed.lines().collect::<Vec<_>>().join(". "));
        format!("Ran {}: {spoken}.", command_name(command))
    } else {
        summary
    }
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

/// "1 error", "3 errors".
pub fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
