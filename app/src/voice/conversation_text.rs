//! What conversation mode says for an agent reply: its prose as markdown for the sentence chunker,
//! with tool calls replaced by short summaries.

use std::time::Duration;

use instant::Instant;
use local_transcription::speech::spoken::{fallback_readout, plain_speech};
use local_transcription::speech::summary::{
    ProgressKind, SpokenSummary, command_approval_subject, command_denied_readout,
    command_description, command_result_readout, plural, progress_line,
};

use crate::ai::agent::{
    AIAgentActionResultType, AIAgentActionType, AIAgentOutput, AIAgentOutputMessageType,
    AIAgentTextSection, RequestCommandOutputResult,
};

/// A tool call, as spoken.
pub struct SpokenAction<'a>(pub &'a AIAgentActionType);

impl SpokenSummary for SpokenAction<'_> {
    fn spoken_summary(&self) -> Option<String> {
        let summary = match self.0 {
            AIAgentActionType::RequestCommandOutput { command, .. } => {
                format!("Running {}.", command_description(command))
            }
            AIAgentActionType::RequestFileEdits { file_edits, .. } => {
                format!("Editing {}.", plural(file_edits.len(), "file"))
            }
            AIAgentActionType::ReadFiles(request) => {
                format!("Reading {}.", plural(request.locations.len(), "file"))
            }
            AIAgentActionType::Grep { .. }
            | AIAgentActionType::FileGlob { .. }
            | AIAgentActionType::FileGlobV2 { .. }
            | AIAgentActionType::SearchCodebase(_) => "Searching the code.".to_owned(),
            AIAgentActionType::CallMCPTool { name, .. } => {
                format!("Calling the {} tool.", name.replace(['_', '-'], " "))
            }
            AIAgentActionType::RunAgents(_) => "Starting agents.".to_owned(),
            AIAgentActionType::AskUserQuestion { .. } => {
                "I have a question for you on screen.".to_owned()
            }
            AIAgentActionType::TransferShellCommandControlToUser { .. } => {
                "The command needs your input.".to_owned()
            }
            AIAgentActionType::UseComputer(_) | AIAgentActionType::RequestComputerUse(_) => {
                "Using the computer.".to_owned()
            }
            AIAgentActionType::WriteToLongRunningShellCommand { .. }
            | AIAgentActionType::ReadShellCommandOutput { .. }
            | AIAgentActionType::UploadArtifact(_)
            | AIAgentActionType::ReadMCPResource { .. }
            | AIAgentActionType::SuggestNewConversation { .. }
            | AIAgentActionType::SuggestPrompt(_)
            | AIAgentActionType::InitProject
            | AIAgentActionType::OpenCodeReview
            | AIAgentActionType::ReadDocuments(_)
            | AIAgentActionType::EditDocuments(_)
            | AIAgentActionType::CreateDocuments(_)
            | AIAgentActionType::InsertCodeReviewComments { .. }
            | AIAgentActionType::StartRecording { .. }
            | AIAgentActionType::StopRecording { .. }
            | AIAgentActionType::ReadSkill(_)
            | AIAgentActionType::FetchConversation { .. }
            | AIAgentActionType::SendMessageToAgent { .. }
            | AIAgentActionType::WaitForEvents { .. } => return None,
        };
        Some(summary)
    }
}

/// What the agent asks to do in a spoken approval prompt (never raw command text), and whether
/// only the on-screen
/// buttons may approve it. Only commands the risk classification explicitly marks as safe may be
/// approved by voice; risky and unclassified ones need the screen.
pub fn spoken_action_prompt(action: &AIAgentActionType) -> (String, bool) {
    match action {
        AIAgentActionType::RequestCommandOutput {
            command, is_risky, ..
        } => (command_approval_subject(command), *is_risky != Some(false)),
        action => {
            let summary = SpokenAction(action)
                .spoken_summary()
                .unwrap_or_else(|| "this action.".to_owned());
            (
                format!(
                    "go ahead with {}",
                    summary.trim_end_matches('.').to_lowercase()
                ),
                // Only commands have a risk classification; anything else is approved on screen.
                true,
            )
        }
    }
}

/// What to say when an action finishes, if anything: a one-line count or failure note for a
/// command, never its text or output.
pub fn spoken_action_result(result: &AIAgentActionResultType) -> Option<String> {
    let AIAgentActionResultType::RequestCommandOutput(result) = result else {
        return None;
    };
    match result {
        RequestCommandOutputResult::Completed {
            command,
            output,
            exit_code,
            ..
        } => Some(command_result_readout(
            command,
            Some(exit_code.value()),
            output,
        )),
        RequestCommandOutputResult::ShellRecovered {
            command, output, ..
        } => Some(command_result_readout(command, Some(1), output)),
        RequestCommandOutputResult::Denylisted { command } => Some(command_denied_readout(command)),
        RequestCommandOutputResult::LongRunningCommandSnapshot { .. }
        | RequestCommandOutputResult::CancelledBeforeExecution
        | RequestCommandOutputResult::TerminalBusy { .. } => None,
    }
}

/// How a tool call is announced while it runs: grouped with nearby calls into one progress
/// line, said right away because the user must act, or not at all.
#[derive(Debug, PartialEq, Eq)]
pub enum ActionAnnouncement {
    Progress(ProgressKind, usize),
    Immediate(String),
    Silent,
}

pub fn action_announcement(action: &AIAgentActionType) -> ActionAnnouncement {
    use ActionAnnouncement::{Immediate, Progress, Silent};
    match action {
        AIAgentActionType::RequestCommandOutput { .. } => Progress(ProgressKind::Command, 1),
        AIAgentActionType::RequestFileEdits { file_edits, .. } => {
            Progress(ProgressKind::Edit, file_edits.len())
        }
        AIAgentActionType::ReadFiles(request) => {
            Progress(ProgressKind::Read, request.locations.len())
        }
        AIAgentActionType::Grep { .. }
        | AIAgentActionType::FileGlob { .. }
        | AIAgentActionType::FileGlobV2 { .. }
        | AIAgentActionType::SearchCodebase(_) => Progress(ProgressKind::Search, 1),
        AIAgentActionType::CallMCPTool { .. }
        | AIAgentActionType::RunAgents(_)
        | AIAgentActionType::UseComputer(_)
        | AIAgentActionType::RequestComputerUse(_) => Progress(ProgressKind::Other, 1),
        AIAgentActionType::AskUserQuestion { .. }
        | AIAgentActionType::TransferShellCommandControlToUser { .. } => {
            match SpokenAction(action).spoken_summary() {
                Some(summary) => Immediate(summary),
                None => Silent,
            }
        }
        AIAgentActionType::WriteToLongRunningShellCommand { .. }
        | AIAgentActionType::ReadShellCommandOutput { .. }
        | AIAgentActionType::UploadArtifact(_)
        | AIAgentActionType::ReadMCPResource { .. }
        | AIAgentActionType::SuggestNewConversation { .. }
        | AIAgentActionType::SuggestPrompt(_)
        | AIAgentActionType::InitProject
        | AIAgentActionType::OpenCodeReview
        | AIAgentActionType::ReadDocuments(_)
        | AIAgentActionType::EditDocuments(_)
        | AIAgentActionType::CreateDocuments(_)
        | AIAgentActionType::InsertCodeReviewComments { .. }
        | AIAgentActionType::StartRecording { .. }
        | AIAgentActionType::StopRecording { .. }
        | AIAgentActionType::ReadSkill(_)
        | AIAgentActionType::FetchConversation { .. }
        | AIAgentActionType::SendMessageToAgent { .. }
        | AIAgentActionType::WaitForEvents { .. } => Silent,
    }
}

/// The tool calls of `output`, in order.
pub fn reply_actions(output: &AIAgentOutput) -> impl Iterator<Item = &AIAgentActionType> {
    output
        .messages
        .iter()
        .filter_map(|message| match &message.message {
            AIAgentOutputMessageType::Action(action) => Some(&action.action),
            _ => None,
        })
}

/// How long to wait after a tool call starts for more to group with it.
const PROGRESS_DEBOUNCE: Duration = Duration::from_millis(1500);
/// The longest a batch waits for more calls before it is announced.
const PROGRESS_MAX_WAIT: Duration = Duration::from_secs(4);
/// The shortest gap between progress lines of one reply.
const PROGRESS_MIN_GAP: Duration = Duration::from_secs(8);

/// Groups the tool calls of one reply into occasional progress lines.
#[derive(Default)]
pub struct ProgressAnnouncer {
    batch: Vec<(ProgressKind, usize)>,
    batch_started: Option<Instant>,
    due_at: Option<Instant>,
    last_spoken: Option<(Instant, String)>,
}

impl ProgressAnnouncer {
    /// Adds a started tool call to the pending batch. Returns when the batch is due.
    pub fn action_started(&mut self, kind: ProgressKind, count: usize, now: Instant) -> Instant {
        let started = *self.batch_started.get_or_insert(now);
        self.batch.push((kind, count));
        let mut due = (now + PROGRESS_DEBOUNCE).min(started + PROGRESS_MAX_WAIT);
        if let Some((at, _)) = &self.last_spoken {
            due = due.max(*at + PROGRESS_MIN_GAP);
        }
        self.due_at = Some(due);
        due
    }

    /// When the pending batch is due, if there is one.
    pub fn due_at(&self) -> Option<Instant> {
        self.due_at
    }

    /// Takes the line to speak once the pending batch is due. A line identical to the previous
    /// one is dropped.
    pub fn take_due(&mut self, now: Instant) -> Option<String> {
        if self.due_at.is_none_or(|due| now < due) {
            return None;
        }
        let line = plain_speech(&progress_line(&self.batch)?);
        self.cancel();
        if line.is_empty()
            || self
                .last_spoken
                .as_ref()
                .is_some_and(|(_, last)| *last == line)
        {
            return None;
        }
        self.last_spoken = Some((now, line.clone()));
        Some(line)
    }

    /// Drops the pending batch: the turn finished, a summary covers it, or the agent asks for
    /// approval.
    pub fn cancel(&mut self) {
        self.batch.clear();
        self.batch_started = None;
        self.due_at = None;
    }
}

/// Builds the text to speak for `output`: each text message's `<spoken>` summary, or a short
/// filtered readout of its prose when it has none. Tool calls are announced separately.
pub fn spoken_reply_text(output: &AIAgentOutput, finished: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let has_spoken = reply_has_spoken_summary(output);
    for message in &output.messages {
        match &message.message {
            AIAgentOutputMessageType::Text(text) => {
                let part = match &text.spoken {
                    Some(spoken) => spoken.clone(),
                    // The server narrates turns that end without `<spoken>`, so prose is read
                    // only when the finished turn still has no summary; reading it earlier
                    // would speak the reply twice.
                    None if has_spoken || !finished => String::new(),
                    None => {
                        let prose = text
                            .sections
                            .iter()
                            .filter_map(|section| match section {
                                AIAgentTextSection::PlainText { text } => Some(text.text()),
                                AIAgentTextSection::Code { .. }
                                | AIAgentTextSection::Table { .. }
                                | AIAgentTextSection::Image { .. }
                                | AIAgentTextSection::MermaidDiagram { .. } => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        fallback_readout(&prose, true)
                    }
                };
                if !part.is_empty() {
                    parts.push(part);
                }
            }
            AIAgentOutputMessageType::Action(_)
            | AIAgentOutputMessageType::Reasoning { .. }
            | AIAgentOutputMessageType::Summarization { .. }
            | AIAgentOutputMessageType::Subagent(_)
            | AIAgentOutputMessageType::TodoOperation(_)
            | AIAgentOutputMessageType::WebSearch(_)
            | AIAgentOutputMessageType::WebFetch(_)
            | AIAgentOutputMessageType::CommentsAddressed { .. }
            | AIAgentOutputMessageType::DebugOutput { .. }
            | AIAgentOutputMessageType::ArtifactCreated(_)
            | AIAgentOutputMessageType::SkillInvoked(_)
            | AIAgentOutputMessageType::MessagesReceivedFromAgents { .. }
            | AIAgentOutputMessageType::EventsFromAgents { .. } => {}
        }
    }
    parts.join("\n\n")
}

/// Whether any text message of `output` carries a `<spoken>` summary.
pub fn reply_has_spoken_summary(output: &AIAgentOutput) -> bool {
    output.messages.iter().any(|message| {
        matches!(&message.message, AIAgentOutputMessageType::Text(text) if text.spoken.is_some())
    })
}

#[cfg(test)]
#[path = "conversation_text_tests.rs"]
mod tests;
