//! What conversation mode says for an agent reply: its prose as markdown for the sentence chunker,
//! with tool calls replaced by short summaries.

use local_transcription::speech::spoken::fallback_readout;
use local_transcription::speech::summary::{
    SpokenSummary, command_approval_subject, command_description, plural,
};

use crate::ai::agent::{
    AIAgentActionType, AIAgentOutput, AIAgentOutputMessageType, AIAgentTextSection,
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

/// The line said as soon as a tool call starts, only when the user must act on it. Other tool
/// calls are not spoken: the reply's summary covers them.
pub fn action_announcement(action: &AIAgentActionType) -> Option<String> {
    match action {
        AIAgentActionType::AskUserQuestion { .. }
        | AIAgentActionType::TransferShellCommandControlToUser { .. } => {
            SpokenAction(action).spoken_summary()
        }
        AIAgentActionType::RequestCommandOutput { .. }
        | AIAgentActionType::RequestFileEdits { .. }
        | AIAgentActionType::ReadFiles(_)
        | AIAgentActionType::Grep { .. }
        | AIAgentActionType::FileGlob { .. }
        | AIAgentActionType::FileGlobV2 { .. }
        | AIAgentActionType::SearchCodebase(_)
        | AIAgentActionType::CallMCPTool { .. }
        | AIAgentActionType::RunAgents(_)
        | AIAgentActionType::UseComputer(_)
        | AIAgentActionType::RequestComputerUse(_)
        | AIAgentActionType::WriteToLongRunningShellCommand { .. }
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
        | AIAgentActionType::WaitForEvents { .. } => None,
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

/// Builds the text to speak for `output`: each text message's `<spoken>` summary, or a short
/// filtered readout of its prose when it has none. Tool calls and their results are not in it.
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
