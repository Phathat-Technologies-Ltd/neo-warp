//! Removes Warp's shell-integration escape sequences from recorded output so recordings play
//! cleanly in asciinema and other players.
//!
//! The shell integration reports state to Warp through private DCS sequences (`ESC P $ ...`),
//! private OSC codes (9277-9280), and prompt/cwd marks (OSC 133, OSC 7). None of those draw
//! anything, and some players print them literally. Standard sequences, including window titles
//! (OSC 0/1/2) and hyperlinks, are kept. Output arrives in chunks, so a sequence may be split;
//! the unfinished part is held until the next chunk.

/// OSC codes that are only meaningful to Warp.
const STRIPPED_OSC_CODES: &[&str] = &["7", "133", "9277", "9278", "9279", "9280"];
/// An OSC code longer than this can't be one of [`STRIPPED_OSC_CODES`].
const MAX_OSC_CODE_LEN: usize = 4;

const ESC: char = '\x1b';
const BEL: char = '\x07';

#[derive(Debug, Default)]
enum State {
    #[default]
    Text,
    /// Saw `ESC`.
    Escape,
    /// Saw `ESC P`.
    DcsStart,
    /// Saw `ESC ]` and these code characters.
    OscCode(String),
    /// Inside a sequence being dropped.
    Discard {
        /// The previous character was `ESC`.
        saw_escape: bool,
        /// BEL also terminates the sequence (OSC).
        ends_on_bel: bool,
    },
}

#[derive(Debug, Default)]
pub struct WarpSequenceFilter {
    state: State,
}

impl WarpSequenceFilter {
    /// Filters the next chunk, returning the text that can be written now.
    pub fn push(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            self.state = match std::mem::take(&mut self.state) {
                State::Text => match ch {
                    ESC => State::Escape,
                    _ => {
                        out.push(ch);
                        State::Text
                    }
                },
                State::Escape => match ch {
                    'P' => State::DcsStart,
                    ']' => State::OscCode(String::new()),
                    ESC => {
                        out.push(ESC);
                        State::Escape
                    }
                    _ => {
                        out.push(ESC);
                        out.push(ch);
                        State::Text
                    }
                },
                State::DcsStart => match ch {
                    '$' => State::Discard {
                        saw_escape: false,
                        ends_on_bel: false,
                    },
                    ESC => {
                        out.push_str("\x1bP");
                        State::Escape
                    }
                    _ => {
                        out.push_str("\x1bP");
                        out.push(ch);
                        State::Text
                    }
                },
                State::OscCode(mut code) => {
                    if ch.is_ascii_digit() && code.len() < MAX_OSC_CODE_LEN {
                        code.push(ch);
                        State::OscCode(code)
                    } else if STRIPPED_OSC_CODES.contains(&code.as_str())
                        && matches!(ch, ';' | BEL | ESC)
                    {
                        match ch {
                            BEL => State::Text,
                            _ => State::Discard {
                                saw_escape: ch == ESC,
                                ends_on_bel: true,
                            },
                        }
                    } else {
                        out.push_str("\x1b]");
                        out.push_str(&code);
                        if ch == ESC {
                            State::Escape
                        } else {
                            out.push(ch);
                            State::Text
                        }
                    }
                }
                State::Discard {
                    saw_escape,
                    ends_on_bel,
                } => {
                    if (saw_escape && ch == '\\') || (ends_on_bel && ch == BEL) {
                        State::Text
                    } else {
                        State::Discard {
                            saw_escape: ch == ESC,
                            ends_on_bel,
                        }
                    }
                }
            };
        }
        out
    }

    /// Ends the stream, returning any held text that turned out not to be a Warp sequence. An
    /// unterminated Warp sequence is dropped.
    pub fn flush(&mut self) -> String {
        match std::mem::take(&mut self.state) {
            State::Text | State::Discard { .. } => String::new(),
            State::Escape => ESC.to_string(),
            State::DcsStart => "\x1bP".to_owned(),
            State::OscCode(code) => format!("\x1b]{code}"),
        }
    }
}

/// Filters a complete text in one call.
pub fn strip_warp_sequences(text: &str) -> String {
    let mut filter = WarpSequenceFilter::default();
    let mut out = filter.push(text);
    out.push_str(&filter.flush());
    out
}

#[cfg(test)]
#[path = "sanitize_tests.rs"]
mod tests;
