//! Speaker verification for hands-free input, free of I/O so it can be tested: an [`AudioTap`]
//! keeps the recent microphone audio, and a [`SpeakerGate`] holds each final transcript back until
//! its audio is checked against the voiceprint, keeping later messages in order behind it.

use std::collections::VecDeque;
use std::ops::Range;

use local_transcription::speech::approval::is_stop_phrase;
use local_transcription::stream::ServerMessage;

const SAMPLE_RATE: u64 = 16_000;
/// Audio kept for verification; longer than any utterance the server waits on.
const TAP_SAMPLES: usize = 12 * SAMPLE_RATE as usize;
/// How far before `SpeechStart` arrives the utterance may have started: the server's pre-roll,
/// VAD onset, and delivery.
const START_SLACK_SAMPLES: u64 = SAMPLE_RATE / 2;
/// Audio used when no `SpeechStart` was seen for a final.
const FALLBACK_SAMPLES: u64 = 3 * SAMPLE_RATE;

/// The most recent microphone audio as sent to the server, addressed by absolute sample index.
#[derive(Debug, Default)]
pub struct AudioTap {
    samples: VecDeque<f32>,
    /// Samples pushed since the session started.
    total: u64,
}

impl AudioTap {
    /// Appends little-endian `i16` PCM.
    pub fn push_pcm(&mut self, pcm: &[u8]) {
        for pair in pcm.as_chunks::<2>().0 {
            if self.samples.len() == TAP_SAMPLES {
                self.samples.pop_front();
            }
            self.samples
                .push_back(f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0);
        }
        self.total += (pcm.len() / 2) as u64;
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// The samples in `range` that are still kept.
    pub fn slice(&self, range: Range<u64>) -> Vec<f32> {
        let first_kept = self.total - self.samples.len() as u64;
        let start = range.start.clamp(first_kept, self.total);
        let end = range.end.clamp(start, self.total);
        self.samples
            .range((start - first_kept) as usize..(end - first_kept) as usize)
            .copied()
            .collect()
    }
}

/// A final transcript waiting for its speaker check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRequest {
    pub utterance: u64,
    pub text: String,
    /// The utterance's audio in [`AudioTap`] sample indices.
    pub range: Range<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateStep {
    /// Hand the message on to hands-free input.
    Deliver(ServerMessage),
    /// Check the speaker of this final, then report the result with [`SpeakerGate::verdict`].
    Verify(VerifyRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Match,
    NoMatch,
    /// The check couldn't run (e.g. the model failed to load); the final is let through so a
    /// broken model never silences the user.
    Unavailable,
}

#[derive(Debug, Default)]
pub struct SpeakerGate {
    speech_start: Option<u64>,
    speech_end: Option<u64>,
    pending: Option<VerifyRequest>,
    queue: VecDeque<ServerMessage>,
}

impl SpeakerGate {
    #[cfg(test)]
    fn is_verifying(&self) -> bool {
        self.pending.is_some()
    }

    /// Takes a server message that arrived when `now` samples had been sent. With `verify` off,
    /// finals pass unchecked. Stop phrases always pass, so the user can stop the assistant even
    /// when the check misses them.
    pub fn push(&mut self, message: ServerMessage, now: u64, verify: bool) -> Vec<GateStep> {
        if self.pending.is_some() {
            self.queue.push_back(message);
            return Vec::new();
        }
        match message {
            ServerMessage::SpeechStart => {
                self.speech_start = Some(now.saturating_sub(START_SLACK_SAMPLES));
                self.speech_end = None;
                vec![GateStep::Deliver(ServerMessage::SpeechStart)]
            }
            ServerMessage::SpeechEnd => {
                self.speech_end = Some(now);
                vec![GateStep::Deliver(ServerMessage::SpeechEnd)]
            }
            ServerMessage::Final { utterance, text } => {
                let end = self.speech_end.take().unwrap_or(now);
                let start = self
                    .speech_start
                    .take()
                    .unwrap_or_else(|| end.saturating_sub(FALLBACK_SAMPLES));
                if !verify || text.trim().is_empty() || is_stop_phrase(&text) {
                    return vec![GateStep::Deliver(ServerMessage::Final { utterance, text })];
                }
                let request = VerifyRequest {
                    utterance,
                    text,
                    range: start..end,
                };
                self.pending = Some(request.clone());
                vec![GateStep::Verify(request)]
            }
            message @ (ServerMessage::Partial { .. } | ServerMessage::Error { .. }) => {
                vec![GateStep::Deliver(message)]
            }
        }
    }

    /// Resolves the pending final and replays messages queued behind it, which may start the next
    /// check. A rejected final is delivered with empty text so its partial is cleared. Returns the
    /// steps and whether the final was rejected.
    pub fn verdict(&mut self, verdict: Verdict, now: u64, verify: bool) -> (Vec<GateStep>, bool) {
        let Some(request) = self.pending.take() else {
            return (Vec::new(), false);
        };
        let rejected = verdict == Verdict::NoMatch;
        let text = if rejected {
            String::new()
        } else {
            request.text
        };
        let mut steps = vec![GateStep::Deliver(ServerMessage::Final {
            utterance: request.utterance,
            text,
        })];
        while self.pending.is_none()
            && let Some(message) = self.queue.pop_front()
        {
            steps.extend(self.push(message, now, verify));
        }
        (steps, rejected)
    }
}

#[cfg(test)]
#[path = "speaker_gate_tests.rs"]
mod tests;
