//! The hands-free state machine, free of I/O so it can be tested: it turns server transcripts,
//! typing, and time into edits of the input.

use std::ops::Range;
use std::time::Duration;

use instant::Instant;
use local_transcription::stream::ServerMessage;
use local_transcription::{VoiceCommand, split_voice_command, terminal_text};

/// What the hands-free indicator shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HandsFreeState {
    #[default]
    Off,
    /// The microphone is open and waiting for speech.
    Listening,
    /// Speech is being heard.
    Hearing,
    /// An utterance ended and its final transcript is pending.
    Transcribing,
}

impl HandsFreeState {
    /// Whether the microphone is open.
    pub fn is_mic_on(self) -> bool {
        match self {
            HandsFreeState::Off => false,
            HandsFreeState::Listening | HandsFreeState::Hearing | HandsFreeState::Transcribing => {
                true
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HandsFreeState::Off => "Hands-free off",
            HandsFreeState::Listening => "Listening",
            HandsFreeState::Hearing => "Hearing",
            HandsFreeState::Transcribing => "Transcribing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandsFreeOptions {
    /// Silence after the last final transcript before the prompt sends; `None` turns it off.
    pub auto_send_after: Option<Duration>,
    pub voice_commands: bool,
}

impl Default for HandsFreeOptions {
    fn default() -> Self {
        Self {
            auto_send_after: Some(DEFAULT_AUTO_SEND),
            voice_commands: true,
        }
    }
}

pub const DEFAULT_AUTO_SEND: Duration = Duration::from_millis(1200);
pub const MIN_AUTO_SEND: Duration = Duration::from_millis(600);
pub const MAX_AUTO_SEND: Duration = Duration::from_millis(3000);

/// An edit of the input the owner must carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandsFreeEffect {
    /// Replace the pending voice text with this partial transcript.
    ShowPartial(String),
    /// Replace the pending voice text with this final transcript and close the pending range.
    Commit(String),
    InsertNewline,
    ClearInput,
    Send,
}

#[derive(Debug, Default)]
pub struct HandsFreeController {
    state: HandsFreeState,
    options: HandsFreeOptions,
    /// Set while the user types; transcripts are ignored until the input is cleared or sent.
    paused: bool,
    /// The utterance whose partials are on screen.
    showing: Option<u64>,
    auto_send_at: Option<Instant>,
}

impl HandsFreeController {
    pub fn state(&self) -> HandsFreeState {
        self.state
    }

    #[cfg(test)]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn auto_send_at(&self) -> Option<Instant> {
        self.auto_send_at
    }

    pub fn start(&mut self, options: HandsFreeOptions) {
        *self = Self {
            state: HandsFreeState::Listening,
            options,
            ..Self::default()
        };
    }

    pub fn stop(&mut self) {
        *self = Self::default();
    }

    /// The user typed into the input (or edited it outside hands-free).
    pub fn user_typed(&mut self) {
        if self.state.is_mic_on() {
            self.paused = true;
            self.showing = None;
            self.auto_send_at = None;
        }
    }

    /// The input was cleared or sent, by voice or by hand.
    pub fn input_reset(&mut self) {
        self.paused = false;
        self.showing = None;
        self.auto_send_at = None;
    }

    /// Handles a server message. `input_is_empty` describes the input after earlier effects, and
    /// `normalize` says whether final text should be rewritten as terminal syntax.
    pub fn on_message(
        &mut self,
        message: ServerMessage,
        normalize: bool,
        now: Instant,
    ) -> Vec<HandsFreeEffect> {
        if !self.state.is_mic_on() {
            return Vec::new();
        }
        match message {
            ServerMessage::SpeechStart => {
                self.state = HandsFreeState::Hearing;
                self.auto_send_at = None;
                Vec::new()
            }
            ServerMessage::SpeechEnd => {
                self.state = HandsFreeState::Transcribing;
                Vec::new()
            }
            ServerMessage::Partial { utterance, text } => {
                if self.paused {
                    return Vec::new();
                }
                self.showing = Some(utterance);
                vec![HandsFreeEffect::ShowPartial(text)]
            }
            ServerMessage::Final { utterance, text } => {
                if self.state == HandsFreeState::Transcribing {
                    self.state = HandsFreeState::Listening;
                }
                if self.paused {
                    return Vec::new();
                }
                let shown = self.showing.take() == Some(utterance);
                self.final_effects(&text, shown, normalize, now)
            }
            ServerMessage::Error { message } => {
                log::warn!("Hands-free transcription error: {message}");
                if self.state == HandsFreeState::Transcribing {
                    self.state = HandsFreeState::Listening;
                }
                Vec::new()
            }
        }
    }

    fn final_effects(
        &mut self,
        text: &str,
        shown: bool,
        normalize: bool,
        now: Instant,
    ) -> Vec<HandsFreeEffect> {
        let (text, command) = if self.options.voice_commands {
            split_voice_command(text)
        } else {
            (text.trim().to_owned(), None)
        };
        let text = if normalize {
            terminal_text::normalize(&text)
        } else {
            text
        };
        let mut effects = Vec::new();
        if !text.is_empty() || shown {
            effects.push(HandsFreeEffect::Commit(text));
        }
        match command {
            Some(VoiceCommand::Send) => {
                effects.push(HandsFreeEffect::Send);
                self.input_reset();
            }
            Some(VoiceCommand::Cancel) => {
                effects.push(HandsFreeEffect::ClearInput);
                self.input_reset();
            }
            Some(VoiceCommand::NewLine) => {
                effects.push(HandsFreeEffect::InsertNewline);
                self.auto_send_at = None;
            }
            None => {
                self.auto_send_at = self.options.auto_send_after.map(|after| now + after);
            }
        }
        effects
    }

    /// Sends the prompt once the auto-send pause has passed.
    pub fn tick(&mut self, now: Instant) -> Option<HandsFreeEffect> {
        let due = self.auto_send_at.is_some_and(|at| now >= at)
            && !self.paused
            && self.state == HandsFreeState::Listening;
        if due {
            self.input_reset();
            return Some(HandsFreeEffect::Send);
        }
        None
    }
}

/// The span of the input that holds the open utterance's transcript, which each partial and the
/// final replace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingVoiceRange {
    range: Option<Range<usize>>,
}

impl PendingVoiceRange {
    /// Replaces the pending text in `buffer` with `text`, opening a range at the end of the buffer
    /// when none is open, separated from existing text by a space.
    pub fn replace(&mut self, buffer: &str, text: &str) -> String {
        let range = match self.range.take() {
            Some(range) if range.end <= buffer.len() && buffer.is_char_boundary(range.start) => {
                range
            }
            _ => {
                let start = if buffer.is_empty() || buffer.ends_with(char::is_whitespace) {
                    buffer.len()
                } else {
                    buffer.len() + 1
                };
                start..start
            }
        };
        let mut out = String::with_capacity(buffer.len() + text.len() + 1);
        let prefix_end = range.start.min(buffer.len());
        out.push_str(&buffer[..prefix_end]);
        if range.start > buffer.len() {
            out.push(' ');
        }
        out.push_str(text);
        let suffix_start = range.end.min(buffer.len());
        out.push_str(&buffer[suffix_start..]);
        self.range = Some(range.start..range.start + text.len());
        out
    }

    /// Replaces the pending text with `text` and closes the range.
    pub fn commit(&mut self, buffer: &str, text: &str) -> String {
        let out = match (text.is_empty(), self.range.is_some()) {
            (true, false) => buffer.to_owned(),
            // The utterance turned out empty; drop its partial and the space before it.
            (true, true) => self.replace(buffer, text).trim_end().to_owned(),
            (false, _) => self.replace(buffer, text),
        };
        self.range = None;
        out
    }

    pub fn clear(&mut self) {
        self.range = None;
    }
}

#[cfg(test)]
#[path = "hands_free_controller_tests.rs"]
mod tests;
