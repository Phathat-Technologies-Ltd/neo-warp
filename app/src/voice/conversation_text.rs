//! What conversation mode says for an agent reply: its prose as markdown for the sentence chunker,
//! with tool calls replaced by short summaries.

use local_transcription::speech::summary::{SpokenSummary, command_name, plural};

use crate::ai::agent::{
    AIAgentActionType, AIAgentOutput, AIAgentOutputMessageType, AIAgentTextSection,
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
/// buttons may approve it because it is marked risky.
pub fn spoken_action_prompt(action: &AIAgentActionType) -> (String, bool) {
    match action {
        AIAgentActionType::RequestCommandOutput {
            command, is_risky, ..
        } => (command.clone(), is_risky.unwrap_or(false)),
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

/// Builds the markdown to speak for `output`. While the reply streams, a trailing code block is
/// left open so its summary counts all of its lines.
pub fn spoken_reply_text(output: &AIAgentOutput, finished: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in &output.messages {
        match &message.message {
            AIAgentOutputMessageType::Text(text) => {
                for section in &text.sections {
                    match section {
                        AIAgentTextSection::PlainText { text } => {
                            parts.push(text.text().to_owned())
                        }
                        AIAgentTextSection::Code { code, language, .. } => {
                            let language = language
                                .as_ref()
                                .map(|language| language.display_name())
                                .unwrap_or_default();
                            parts.push(format!("```{language}\n{}\n```", code.trim_end()));
                        }
                        AIAgentTextSection::Table { .. }
                        | AIAgentTextSection::Image { .. }
                        | AIAgentTextSection::MermaidDiagram { .. } => {}
                    }
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
    let mut text = parts.join("\n\n");
    if !finished && text.ends_with("\n```") {
        text.truncate(text.len() - "\n```".len());
    }
    text
}

#[cfg(test)]
#[path = "conversation_text_tests.rs"]
mod tests;
