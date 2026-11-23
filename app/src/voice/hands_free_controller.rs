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
    /// The session is on but the microphone is closed because Warp isn't the active app.
    Paused,
}

impl HandsFreeState {
    /// Whether the microphone is open.
    pub fn is_mic_on(self) -> bool {
        match self {
            HandsFreeState::Off | HandsFreeState::Paused => false,
            HandsFreeState::Listening | HandsFreeState::Hearing | HandsFreeState::Transcribing => {
                true
            }
        }
    }

    /// Whether the session is on, including while paused.
    pub fn is_on(self) -> bool {
        match self {
            HandsFreeState::Off => false,
            HandsFreeState::Listening
            | HandsFreeState::Hearing
            | HandsFreeState::Transcribing
            | HandsFreeState::Paused => true,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HandsFreeState::Off => "Hands-free off",
            HandsFreeState::Listening => "Listening",
            HandsFreeState::Hearing => "Hearing",
            HandsFreeState::Transcribing => "Transcribing",
            HandsFreeState::Paused => "Mic paused",
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
    /// Remove the pending voice text of an utterance that will never finish.
    DiscardPartial,
    /// Send the input because the user said a send command.
    Send,
    /// Send the input because the auto-send pause passed. Inputs that run shell commands ignore
    /// it, so a misheard utterance never runs without an explicit command.
    AutoSend,
}

impl HandsFreeEffect {
    /// The effect's name without its text, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            HandsFreeEffect::ShowPartial(_) => "partial",
            HandsFreeEffect::Commit(_) => "commit",
            HandsFreeEffect::InsertNewline => "newline",
            HandsFreeEffect::ClearInput => "clear",
            HandsFreeEffect::DiscardPartial => "discard",
            HandsFreeEffect::Send => "send",
            HandsFreeEffect::AutoSend => "auto-send",
        }
    }
}

#[derive(Debug, Default)]
pub struct HandsFreeController {
    state: HandsFreeState,
    options: HandsFreeOptions,
    /// Set while the user types; transcripts are ignored until the input is cleared or sent.
    paused: bool,
    /// The utterance whose partials are on screen, with the latest partial's text.
    showing: Option<(u64, String)>,
    auto_send_at: Option<Instant>,
}

impl HandsFreeController {
    pub fn state(&self) -> HandsFreeState {
        self.state
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

    /// Closes the microphone without ending the session. The open utterance is discarded, and
    /// transcripts are ignored until [`Self::resume`].
    pub fn pause(&mut self) -> Vec<HandsFreeEffect> {
        if !self.state.is_mic_on() {
            return Vec::new();
        }
        self.state = HandsFreeState::Paused;
        self.auto_send_at = None;
        match self.showing.take() {
            Some(_) => vec![HandsFreeEffect::DiscardPartial],
            None => Vec::new(),
        }
    }

    /// Reopens the microphone after [`Self::pause`].
    pub fn resume(&mut self) {
        if self.state == HandsFreeState::Paused {
            self.state = HandsFreeState::Listening;
        }
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

    /// Whether transcripts are dropped because the user is typing.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Handles a server message. Final text is left as spoken; the input that applies it decides
    /// whether to rewrite it as terminal syntax.
    pub fn on_message(&mut self, message: ServerMessage, now: Instant) -> Vec<HandsFreeEffect> {
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
                self.showing = Some((utterance, text.clone()));
                vec![HandsFreeEffect::ShowPartial(text)]
            }
            ServerMessage::Final { utterance, text } => {
                if self.state == HandsFreeState::Transcribing {
                    self.state = HandsFreeState::Listening;
                }
                if self.paused {
                    return Vec::new();
                }
                let shown = self
                    .showing
                    .take()
                    .filter(|(id, _)| *id == utterance)
                    .map(|(_, partial)| partial);
                self.final_effects(&text, shown, now)
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

    /// Drops `utterance` without applying it (its speaker isn't the user, or conversation mode
    /// took it), removing its partial if shown.
    pub fn reject(&mut self, utterance: u64) -> Vec<HandsFreeEffect> {
        if !self.state.is_mic_on() {
            return Vec::new();
        }
        if self.state == HandsFreeState::Transcribing {
            self.state = HandsFreeState::Listening;
        }
        if self.showing.as_ref().is_some_and(|(id, _)| *id == utterance) {
            self.showing = None;
            return vec![HandsFreeEffect::DiscardPartial];
        }
        Vec::new()
    }

    /// `shown` is the latest partial of the final's utterance, if it is on screen.
    fn final_effects(
        &mut self,
        text: &str,
        shown: Option<String>,
        now: Instant,
    ) -> Vec<HandsFreeEffect> {
        // Rejected utterances arrive through [`Self::reject`], so an empty final for speech whose
        // partial is on screen is a transcription that lost its text: keep what the user saw.
        let text = match &shown {
            Some(partial) if text.trim().is_empty() => partial.as_str(),
            _ => text,
        };
        let (text, command) = if self.options.voice_commands {
            split_voice_command(text)
        } else {
            (text.trim().to_owned(), None)
        };
        let shown = shown.is_some();
        let mut effects = Vec::new();
        // An empty final must not route, correct, or schedule a send of what is already typed.
        if text.is_empty() && command.is_none() {
            if shown {
                effects.push(HandsFreeEffect::DiscardPartial);
            }
            return effects;
        }
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

    /// Moves a pending auto-send to no earlier than `at`.
    pub fn defer_auto_send(&mut self, at: Instant) {
        if let Some(due) = &mut self.auto_send_at {
            *due = (*due).max(at);
        }
    }

    /// Sends the prompt once the auto-send pause has passed.
    pub fn tick(&mut self, now: Instant) -> Option<HandsFreeEffect> {
        let due = self.auto_send_at.is_some_and(|at| now >= at)
            && !self.paused
            && self.state == HandsFreeState::Listening;
        if due {
            self.input_reset();
            return Some(HandsFreeEffect::AutoSend);
        }
        None
    }
}

/// The final text to write into an input, rewritten as terminal syntax for shell inputs.
pub fn final_text_for_input(text: &str, is_shell: bool) -> String {
    if is_shell {
        terminal_text::normalize(text)
    } else {
        text.to_owned()
    }
}

/// Whether `effect` submits the input holding `buffer`. Auto-send submits agent prompts, and shell
/// commands only when they are low risk; any other command waits for a spoken send or Enter.
pub fn sends_input(effect: &HandsFreeEffect, is_shell: bool, buffer: &str) -> bool {
    match effect {
        HandsFreeEffect::Send => true,
        HandsFreeEffect::AutoSend => !is_shell || is_low_risk_command(buffer),
        HandsFreeEffect::ShowPartial(_)
        | HandsFreeEffect::Commit(_)
        | HandsFreeEffect::InsertNewline
        | HandsFreeEffect::ClearInput
        | HandsFreeEffect::DiscardPartial => false,
    }
}

/// Where a dictated utterance should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRoute {
    /// The agent, as a natural-language prompt.
    Agent,
    /// The shell, as a command.
    Shell,
}

/// Spoken prefixes that may introduce a shell command.
const COMMAND_PREFIXES: &[&str] = &["run", "command", "execute"];

/// Executables whose name at the start of an utterance marks it as a command.
const KNOWN_EXECUTABLES: &[&str] = &[
    "ls",
    "cd",
    "pwd",
    "git",
    "cargo",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "make",
    "docker",
    "kubectl",
    "cat",
    "grep",
    "rg",
    "find",
    "echo",
    "mkdir",
    "rm",
    "mv",
    "cp",
    "touch",
    "chmod",
    "ssh",
    "curl",
    "wget",
    "python",
    "python3",
    "pip",
    "node",
    "brew",
    "gh",
    "vim",
    "nvim",
    "code",
    "top",
    "htop",
    "ps",
    "kill",
    "tail",
    "head",
    "less",
    "man",
    "which",
    "export",
    "source",
    "clear",
    "exit",
    "sudo",
    "go",
    "rustup",
    "terraform",
    "aws",
    "uv",
    "tar",
    "zip",
    "unzip",
    "wc",
    "whoami",
    "date",
    "tree",
    "du",
    "df",
];

/// Words that show an utterance is English rather than (or as well as) a command, as in
/// "make sure" or "git status and tell me what changed".
const ENGLISH_WORDS: &[&str] = &[
    "sure", "me", "us", "the", "it", "this", "that", "all", "my", "our", "some", "why", "what",
    "how", "if", "whether", "and", "then", "tell", "explain", "please", "can", "could", "would",
    "you", "is", "are",
];

/// Commands that only read state and run on auto-send.
const LOW_RISK_COMMANDS: &[&str] = &[
    "ls", "pwd", "cat", "head", "tail", "wc", "which", "whoami", "date", "tree", "du", "df",
    "grep", "rg",
];

/// Read-only git subcommands that run on auto-send.
const LOW_RISK_GIT_SUBCOMMANDS: &[&str] = &["status", "log", "diff", "show"];

/// Decides where a dictated utterance goes and returns the text to insert. Dictation is mostly
/// natural language, so only an utterance that is clearly a command, optionally introduced by a
/// spoken "run"/"command" (which is stripped), routes to the shell. Natural language, including a
/// command mixed with a request, goes to the agent unchanged.
pub fn route_utterance(text: &str) -> (InputRoute, String) {
    let trimmed = text.trim();
    let first = trimmed.split_whitespace().next().unwrap_or_default();
    let command = if COMMAND_PREFIXES.contains(&word_key(first).as_str()) {
        trimmed[first.len()..]
            .trim_start_matches([',', ':', '.', ' '])
            .trim()
    } else {
        trimmed
    };
    if looks_like_command(command) {
        (InputRoute::Shell, command.to_owned())
    } else {
        (InputRoute::Agent, trimmed.to_owned())
    }
}

/// Whether `command` only reads state, so it may run without an explicit send.
pub fn is_low_risk_command(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() || command.contains([';', '|', '&', '>', '<', '`', '$', '\n']) {
        return false;
    }
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    if program == "git" {
        return words
            .next()
            .is_some_and(|sub| LOW_RISK_GIT_SUBCOMMANDS.contains(&sub))
            && !words.any(|arg| arg.starts_with("--output") || arg == "-o");
    }
    LOW_RISK_COMMANDS.contains(&program)
}

fn word_key(word: &str) -> String {
    word.trim_end_matches([',', ':', '.']).to_ascii_lowercase()
}

fn looks_like_command(text: &str) -> bool {
    if text.is_empty() || text.ends_with('?') {
        return false;
    }
    let mut words = text.split_whitespace().map(word_key);
    let Some(first) = words.next() else {
        return false;
    };
    KNOWN_EXECUTABLES.contains(&first.as_str())
        && !words.any(|word| ENGLISH_WORDS.contains(&word.as_str()))
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

    /// Whether the open utterance (or the next one, when none is open) starts an input that holds
    /// nothing else.
    pub fn starts_input(&self, buffer: &str) -> bool {
        match &self.range {
            Some(range) => buffer
                .get(..range.start)
                .is_none_or(|p| p.trim().is_empty()),
            None => buffer.trim().is_empty(),
        }
    }

    pub fn clear(&mut self) {
        self.range = None;
    }
}

/// The single input hands-free belongs to: the focused input of the active tab or pane in the
/// active window, identified by its window and terminal surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceTarget {
    target: Option<(warpui::WindowId, warpui::EntityId)>,
}

impl VoiceTarget {
    pub fn is(&self, window_id: warpui::WindowId, surface: warpui::EntityId) -> bool {
        self.target == Some((window_id, surface))
    }

    pub fn surface(&self) -> Option<warpui::EntityId> {
        self.target.map(|(_, surface)| surface)
    }

    /// Moves the target to a newly focused input. Returns whether it changed.
    pub fn focus(&mut self, window_id: warpui::WindowId, surface: warpui::EntityId) -> bool {
        let changed = !self.is(window_id, surface);
        self.target = Some((window_id, surface));
        changed
    }
}

#[cfg(test)]
#[path = "hands_free_controller_tests.rs"]
mod tests;
