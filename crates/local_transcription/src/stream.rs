use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::turn::{EndOfTurn, EndOfTurnConfig, TurnDecision, TurnDetector, ends_mid_thought};
use crate::vad::{
    SegmentEvent, Segmenter, SegmenterConfig, VAD_FRAME_SAMPLES, VAD_SAMPLE_RATE,
    VoiceActivityDetector,
};
use crate::{Audio, TranscribeError};

/// A text frame from the client of `/ai/transcribe/stream`. Audio travels in binary frames of
/// 16 kHz mono little-endian `i16` PCM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Config {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vad: Option<VadOptions>,
    },
    /// Ends the open utterance now, as if the speaker paused.
    Flush,
    /// Ends the stream once pending transcripts are sent.
    End,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VadOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silence_ms: Option<u32>,
    /// Whether a turn detector may end utterances early on finished thoughts and hold them open
    /// through mid-thought pauses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart_end_of_turn: Option<bool>,
}

/// A text frame from the server of `/ai/transcribe/stream`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    SpeechStart,
    /// The latest transcript of the open utterance `utterance`; replaces earlier partials.
    Partial {
        utterance: u64,
        text: String,
    },
    /// The transcript of the finished utterance `utterance`; replaces its partials.
    Final {
        utterance: u64,
        text: String,
    },
    SpeechEnd,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeKind {
    Partial,
    Final,
}

/// Audio of one utterance to transcribe.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeRequest {
    pub utterance: u64,
    pub kind: DecodeKind,
    pub audio: Audio,
}

impl DecodeRequest {
    pub fn into_message(self, text: String) -> ServerMessage {
        match self.kind {
            DecodeKind::Partial => ServerMessage::Partial {
                utterance: self.utterance,
                text,
            },
            DecodeKind::Final => ServerMessage::Final {
                utterance: self.utterance,
                text,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamOutput {
    Message(ServerMessage),
    Decode(DecodeRequest),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamConfig {
    pub segmenter: SegmenterConfig,
    /// Audio between partial transcripts of an open utterance.
    pub partial_interval_ms: u32,
    /// Partials decode at most this much of the end of the utterance.
    pub max_partial_ms: u32,
    /// An utterance this long ends even without a pause, bounding memory and decode time.
    pub max_utterance_ms: u32,
    /// Audio kept from before speech starts, so the first syllable isn't cut.
    pub pre_roll_ms: u32,
    /// When a turn detector ends utterances. Without one, `segmenter.min_silence_ms` does.
    pub end_of_turn: EndOfTurnConfig,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            segmenter: SegmenterConfig::default(),
            partial_interval_ms: 600,
            max_partial_ms: 15_000,
            max_utterance_ms: 30_000,
            pre_roll_ms: 192,
            end_of_turn: EndOfTurnConfig::default(),
        }
    }
}

fn ms_to_samples(ms: u32) -> usize {
    (ms as usize * VAD_SAMPLE_RATE as usize) / 1000
}

/// Splits a live audio stream into utterances with a [`VoiceActivityDetector`] and says when to
/// transcribe them. It does no I/O and never transcribes itself.
pub struct UtteranceStream {
    detector: Box<dyn VoiceActivityDetector>,
    segmenter: Segmenter,
    config: StreamConfig,
    /// A trailing odd byte of PCM from the last binary frame.
    pending_byte: Option<u8>,
    /// Samples not yet making up a whole frame.
    pending: Vec<f32>,
    pre_roll: VecDeque<f32>,
    utterance: Vec<f32>,
    utterance_id: u64,
    since_partial: usize,
    turn_detector: Option<Box<dyn TurnDetector>>,
    end_of_turn: EndOfTurn,
    smart_end_of_turn: bool,
    /// The latest partial transcript of the open utterance.
    partial_text: String,
}

impl UtteranceStream {
    pub fn new(detector: Box<dyn VoiceActivityDetector>, config: StreamConfig) -> Self {
        Self {
            detector,
            segmenter: Segmenter::new(config.segmenter),
            config,
            pending_byte: None,
            pending: Vec::with_capacity(VAD_FRAME_SAMPLES),
            pre_roll: VecDeque::new(),
            utterance: Vec::new(),
            utterance_id: 0,
            since_partial: 0,
            turn_detector: None,
            end_of_turn: EndOfTurn::new(config.end_of_turn),
            smart_end_of_turn: true,
            partial_text: String::new(),
        }
    }

    /// Lets `detector` decide when pauses end utterances.
    pub fn with_turn_detector(mut self, detector: Box<dyn TurnDetector>) -> Self {
        self.turn_detector = Some(detector);
        self.apply_silence_timeout();
        self
    }

    /// Changes how long a pause ends an utterance without a turn detector.
    pub fn set_min_silence_ms(&mut self, min_silence_ms: u32) {
        self.config.segmenter.min_silence_ms = min_silence_ms;
        self.apply_silence_timeout();
    }

    /// Turns the turn detector on or off; off, pauses end utterances after the fixed timeout.
    pub fn set_smart_end_of_turn(&mut self, enabled: bool) {
        self.smart_end_of_turn = enabled;
        self.apply_silence_timeout();
    }

    /// Records a partial transcript, so a pause after words like "the" or "in" doesn't end the
    /// turn even when it sounds finished.
    pub fn note_partial(&mut self, utterance: u64, text: &str) {
        if utterance == self.utterance_id && self.segmenter.in_speech() {
            text.clone_into(&mut self.partial_text);
        }
    }

    /// Whether a turn detector decides when utterances end.
    pub fn uses_turn_detector(&self) -> bool {
        self.smart_end_of_turn && self.turn_detector.is_some()
    }

    fn apply_silence_timeout(&mut self) {
        let min_silence_ms = if self.uses_turn_detector() {
            self.config.end_of_turn.incomplete_silence_ms
        } else {
            self.config.segmenter.min_silence_ms
        };
        self.segmenter.set_min_silence_ms(min_silence_ms);
    }

    pub fn in_speech(&self) -> bool {
        self.segmenter.in_speech()
    }

    /// Feeds little-endian `i16` PCM bytes.
    pub fn push_pcm(&mut self, bytes: &[u8]) -> Result<Vec<StreamOutput>, TranscribeError> {
        let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
        let mut rest = bytes;
        if let Some(low) = self.pending_byte.take() {
            match rest.split_first() {
                Some((&high, tail)) => {
                    samples.push(i16::from_le_bytes([low, high]) as f32 / 32768.0);
                    rest = tail;
                }
                None => self.pending_byte = Some(low),
            }
        }
        let (pairs, remainder) = rest.as_chunks::<2>();
        samples.extend(
            pairs
                .iter()
                .map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.0),
        );
        if let [last] = remainder {
            self.pending_byte = Some(*last);
        }
        self.push_samples(&samples)
    }

    /// Feeds 16 kHz mono samples.
    pub fn push_samples(&mut self, samples: &[f32]) -> Result<Vec<StreamOutput>, TranscribeError> {
        let mut outputs = Vec::new();
        for &sample in samples {
            self.pending.push(sample);
            if self.pending.len() == VAD_FRAME_SAMPLES {
                let frame = std::mem::take(&mut self.pending);
                let result = self.push_frame(&frame, &mut outputs);
                self.pending = frame;
                self.pending.clear();
                result?;
            }
        }
        Ok(outputs)
    }

    /// Ends the open utterance, if any.
    pub fn flush(&mut self) -> Vec<StreamOutput> {
        let mut outputs = Vec::new();
        if self.segmenter.in_speech() {
            self.utterance.append(&mut self.pending);
            self.segmenter.reset();
            self.end_utterance(&mut outputs);
        }
        outputs
    }

    fn push_frame(
        &mut self,
        frame: &[f32],
        outputs: &mut Vec<StreamOutput>,
    ) -> Result<(), TranscribeError> {
        let probability = self.detector.speech_probability(frame)?;
        let was_in_speech = self.segmenter.in_speech();
        let event = self.segmenter.push(probability);
        if was_in_speech {
            self.utterance.extend_from_slice(frame);
            self.since_partial += frame.len();
        } else {
            self.pre_roll.extend(frame.iter().copied());
            let keep = ms_to_samples(self.config.pre_roll_ms)
                + VAD_FRAME_SAMPLES * self.segmenter.min_speech_frames() as usize;
            let excess = self.pre_roll.len().saturating_sub(keep);
            self.pre_roll.drain(..excess);
        }
        match event {
            Some(SegmentEvent::SpeechStart) => {
                self.utterance_id += 1;
                self.utterance = self.pre_roll.drain(..).collect();
                self.partial_text.clear();
                self.since_partial = 0;
                outputs.push(StreamOutput::Message(ServerMessage::SpeechStart));
            }
            Some(SegmentEvent::SpeechEnd) => self.end_utterance(outputs),
            None if was_in_speech && self.probe_end_of_turn() => {
                self.segmenter.reset();
                self.end_utterance(outputs);
            }
            None if was_in_speech => {
                if self.utterance.len() >= ms_to_samples(self.config.max_utterance_ms) {
                    self.segmenter.reset();
                    self.end_utterance(outputs);
                } else if self.since_partial >= ms_to_samples(self.config.partial_interval_ms) {
                    self.since_partial = 0;
                    let start = self
                        .utterance
                        .len()
                        .saturating_sub(ms_to_samples(self.config.max_partial_ms));
                    outputs.push(StreamOutput::Decode(DecodeRequest {
                        utterance: self.utterance_id,
                        kind: DecodeKind::Partial,
                        audio: Audio {
                            samples: self.utterance[start..].to_vec(),
                            sample_rate: VAD_SAMPLE_RATE,
                        },
                    }));
                }
            }
            None => {}
        }
        Ok(())
    }

    /// Whether the turn detector ends the utterance at this point of the pause.
    fn probe_end_of_turn(&mut self) -> bool {
        if !self.uses_turn_detector() {
            return false;
        }
        match self.end_of_turn.on_silence(self.segmenter.silence_ms()) {
            TurnDecision::Wait => false,
            TurnDecision::End => true,
            TurnDecision::Probe => {
                let Some(detector) = self.turn_detector.as_mut() else {
                    return false;
                };
                match detector.completion_probability(&self.utterance) {
                    Ok(probability) => {
                        self.end_of_turn.is_complete(probability)
                            && !ends_mid_thought(&self.partial_text)
                    }
                    Err(error) => {
                        log::warn!(
                            "Turn detection failed, falling back to the silence timeout: {error}"
                        );
                        self.turn_detector = None;
                        self.apply_silence_timeout();
                        self.segmenter.silence_ms().unwrap_or(0)
                            >= self.config.segmenter.min_silence_ms
                    }
                }
            }
        }
    }

    fn end_utterance(&mut self, outputs: &mut Vec<StreamOutput>) {
        outputs.push(StreamOutput::Decode(DecodeRequest {
            utterance: self.utterance_id,
            kind: DecodeKind::Final,
            audio: Audio {
                samples: std::mem::take(&mut self.utterance),
                sample_rate: VAD_SAMPLE_RATE,
            },
        }));
        outputs.push(StreamOutput::Message(ServerMessage::SpeechEnd));
        self.since_partial = 0;
        self.end_of_turn.reset();
    }
}

/// Decodes waiting for the transcriber, which runs one at a time. A newer partial replaces a
/// waiting one, and a final makes its utterance's waiting partial pointless, so the transcriber
/// never falls behind the speaker by more than one partial.
#[derive(Debug, Default)]
pub struct DecodeQueue {
    queue: VecDeque<DecodeRequest>,
}

impl DecodeQueue {
    pub fn push(&mut self, request: DecodeRequest) {
        match request.kind {
            DecodeKind::Partial => {
                if self.queue.iter().any(|queued| {
                    queued.kind == DecodeKind::Final && queued.utterance >= request.utterance
                }) {
                    return;
                }
                self.queue.retain(|queued| queued.kind == DecodeKind::Final);
            }
            DecodeKind::Final => {
                self.queue.retain(|queued| {
                    queued.kind == DecodeKind::Final || queued.utterance != request.utterance
                });
            }
        }
        self.queue.push_back(request);
    }

    pub fn pop(&mut self) -> Option<DecodeRequest> {
        self.queue.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
