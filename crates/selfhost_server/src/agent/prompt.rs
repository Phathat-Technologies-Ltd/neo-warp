//! The agent's system prompt: fixed instructions plus the session context the client sends with
//! every request.

use warp_multi_agent_api as api;

const INSTRUCTIONS: &str = "\
You are a coding agent working in the user's terminal. You help with software engineering: \
reading and changing code, running commands, debugging, and explaining.

How to work:
- Use the tools to look at the real state of the project before answering or changing it. \
Don't guess file contents or command output.
- Read a file before editing it, and keep edits small and focused on what was asked.
- After a change, run the relevant build, test, or lint command when one exists, and fix what \
you broke.
- Commands run in the user's own terminal. Prefer read-only commands for exploring, and mark \
each command's read_only and risky flags honestly; the user may be asked to approve the rest.
- Don't run destructive commands (deleting data, force-pushing, rewriting history) unless the \
user asked for exactly that.
- If a tool call is cancelled or denied, don't retry it the same way; ask the user how to \
proceed.

How to answer:
- Be concise and direct. Lead with the result. Use Markdown, with code in fenced blocks.
- Write in simple technical English: short sentences, common words, and precise technical terms. Avoid idioms, jargon you don't explain, and filler.
- When you finish a task, say briefly what you changed and how you checked it.";

/// The request metadata key the client sets while voice conversation mode reads replies aloud.
pub const VOICE_CONVERSATION_METADATA_KEY: &str = "voice_conversation";

const SPOKEN_SUMMARY_INSTRUCTIONS: &str = "\n\n# Voice conversation
The user is listening to your replies read aloud. Start every reply, including replies after \
tool calls, with one or two short plain-English sentences meant to be heard, wrapped in \
<spoken>...</spoken>. Inside the tag: no file paths, commands, code, flags, hashes, URLs, long \
numbers, or lists. Describe the result and the next step in plain words. When you ask the user \
a question, keep the spoken part to one sentence. After the closing tag, write the full answer \
as usual; it is shown on screen and not read aloud.";

/// Whether the request's metadata says voice conversation mode is on.
pub fn wants_spoken_summary(metadata: Option<&api::request::Metadata>) -> bool {
    metadata
        .and_then(|metadata| metadata.logging.get(VOICE_CONVERSATION_METADATA_KEY))
        .is_some_and(|value| matches!(value.kind, Some(prost_types::value::Kind::BoolValue(true))))
}

pub fn system_prompt(context: Option<&api::InputContext>, spoken_summary: bool) -> String {
    let mut prompt = INSTRUCTIONS.to_owned();
    if spoken_summary {
        prompt.push_str(SPOKEN_SUMMARY_INSTRUCTIONS);
    }
    let Some(context) = context else {
        return prompt;
    };

    let mut environment = Vec::new();
    if let Some(directory) = &context.directory {
        if !directory.pwd.is_empty() {
            environment.push(format!("Working directory: {}", directory.pwd));
        }
        if !directory.home.is_empty() {
            environment.push(format!("Home directory: {}", directory.home));
        }
    }
    if let Some(os) = &context.operating_system
        && !os.platform.is_empty()
    {
        environment.push(format!("Operating system: {}", os.platform));
    }
    if let Some(shell) = &context.shell
        && !shell.name.is_empty()
    {
        environment.push(
            format!("Shell: {} {}", shell.name, shell.version)
                .trim_end()
                .to_owned(),
        );
    }
    if let Some(git) = &context.git
        && !git.head.is_empty()
    {
        environment.push(format!("Git branch: {}", git.head));
    }
    if let Some(time) = &context.current_time {
        environment.push(format!("Current time (Unix seconds): {}", time.seconds));
    }
    if !environment.is_empty() {
        prompt.push_str("\n\n# Environment\n");
        prompt.push_str(&environment.join("\n"));
    }

    for rules in &context.project_rules {
        let files = rules
            .active_rule_files
            .iter()
            .filter(|file| !file.content.trim().is_empty())
            .collect::<Vec<_>>();
        if files.is_empty() {
            continue;
        }
        prompt.push_str("\n\n# Project rules\nFollow these instructions from the project:\n");
        for file in files {
            prompt.push_str(&format!("\n## {}\n{}\n", file.file_path, file.content));
        }
    }
    prompt
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
