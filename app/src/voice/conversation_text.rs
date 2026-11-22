//! What conversation mode says for an agent reply: its prose as markdown for the sentence chunker,
//! with tool calls replaced by short summaries.

use local_transcription::speech::spoken::fallback_readout;
use local_transcription::speech::summary::{
    SpokenSummary, command_name, command_result_readout, plural,
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
                format!("Running {}.", command_name(command))
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

/// The command (or summary) to name in a spoken approval prompt, and whether only the on-screen
/// buttons may approve it. Only commands the risk classification explicitly marks as safe may be
/// approved by voice; risky and unclassified ones need the screen.
pub fn spoken_action_prompt(action: &AIAgentActionType) -> (String, bool) {
    match action {
        AIAgentActionType::RequestCommandOutput {
            command, is_risky, ..
        } => (command.clone(), *is_risky != Some(false)),
        action => {
            let summary = SpokenAction(action)
                .spoken_summary()
                .unwrap_or_else(|| "this action.".to_owned());
            (
                summary.trim_end_matches('.').to_lowercase(),
                // Only commands have a risk classification; anything else is approved on screen.
                true,
            )
        }
    }
}

/// What to say when an action finishes, if anything: a command's short output is read out, and
/// longer output is only summarized.
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
        RequestCommandOutputResult::Denylisted { command } => Some(format!(
            "{} is on the deny list, so it didn't run.",
            command_name(command)
        )),
        RequestCommandOutputResult::LongRunningCommandSnapshot { .. }
        | RequestCommandOutputResult::CancelledBeforeExecution
        | RequestCommandOutputResult::TerminalBusy { .. } => None,
    }
}

/// Builds the text to speak for `output`: each text message's `<spoken>` summary, or a short
/// filtered readout of its prose when it has none, and short summaries of tool calls.
pub fn spoken_reply_text(output: &AIAgentOutput, finished: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let last_index = output.messages.len().saturating_sub(1);
    for (index, message) in output.messages.iter().enumerate() {
        match &message.message {
            AIAgentOutputMessageType::Text(text) => {
                let part = match &text.spoken {
                    Some(spoken) => spoken.clone(),
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
                        fallback_readout(&prose, finished || index < last_index)
                    }
                };
                if !part.is_empty() {
                    parts.push(part);
                }
            }
            AIAgentOutputMessageType::Action(action) => {
                if let Some(summary) = SpokenAction(&action.action).spoken_summary() {
                    parts.push(summary);
                }
            }
            AIAgentOutputMessageType::Reasoning { .. }
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

#[cfg(test)]
#[path = "conversation_text_tests.rs"]
mod tests;
