use std::cell::RefCell;

use super::*;
use crate::Audio;
use crate::speech::{SpeakError, Speaker, VoiceId, VoiceInfo};

/// A speaker that returns one sample per character and records what it was asked to say.
#[derive(Default)]
struct FakeSpeaker {
    fail_on: Option<String>,
}

impl Speaker for FakeSpeaker {
    fn synthesize(&self, text: &str, _: &VoiceId, _: f32) -> Result<Audio, SpeakError> {
        if self.fail_on.as_deref() == Some(text) {
            return Err(SpeakError::Engine("boom".to_owned()));
        }
        Ok(Audio {
            samples: vec![0.0; text.len()],
            sample_rate: 24_000,
        })
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        Vec::new()
    }
}

/// Runs the controller against a fake speaker and a fake output device whose playback the test
/// finishes by hand.
struct Harness {
    controller: ConversationController<(String, Audio), u32>,
    speaker: FakeSpeaker,
    /// Synthesis requests not yet run.
    synth_queue: RefCell<Vec<(u64, u64, String)>>,
    playing: Option<(u64, u64, String)>,
    log: Vec<String>,
}

impl Harness {
    fn new() -> Self {
        let mut controller = ConversationController::default();
        controller.start(ChunkerOptions::default());
        Self {
            controller,
            speaker: FakeSpeaker::default(),
            synth_queue: RefCell::new(Vec::new()),
            playing: None,
            log: Vec::new(),
        }
    }

    fn apply(&mut self, actions: Vec<PlaybackAction<(String, Audio)>>) {
        for action in actions {
            match action {
                PlaybackAction::Synthesize { reply, seq, text } => {
                    self.log.push(format!("synth {text}"));
                    self.synth_queue.borrow_mut().push((reply, seq, text));
                }
                PlaybackAction::Play {
                    reply,
                    seq,
                    audio: (text, _),
                } => {
                    self.log.push(format!("play {text}"));
                    self.playing = Some((reply, seq, text));
                }
                PlaybackAction::StopPlayback => {
                    self.log.push("stop".to_owned());
                    self.playing = None;
                }
                PlaybackAction::Interrupted { reply } => {
                    self.log.push(format!("interrupted {reply}"));
                }
            }
        }
    }

    /// Runs the pending synthesis requests, as `spawn_blocking` would.
    fn run_synthesis(&mut self) {
        loop {
            let pending = std::mem::take(&mut *self.synth_queue.borrow_mut());
            if pending.is_empty() {
                return;
            }
            for (reply, seq, text) in pending {
                let audio = self
                    .speaker
                    .synthesize(&text, &VoiceId::new("v"), 1.0)
                    .ok()
                    .map(|audio| (text, audio));
                let actions = self.controller.synthesized(reply, seq, audio);
                self.apply(actions);
            }
        }
    }

    fn finish_playback(&mut self) {
        let (reply, seq, _) = self.playing.take().expect("something is playing");
        let actions = self.controller.playback_finished(reply, seq);
        self.apply(actions);
    }
}

#[test]
fn speaks_sentences_in_order_one_synthesis_ahead() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    assert_eq!(h.controller.state(), ConversationState::Thinking);
    let actions = h.controller.reply_text("First one. Second one. Third");
    h.apply(actions);
    assert_eq!(h.log, vec!["synth First one."]);
    h.run_synthesis();
    assert_eq!(h.controller.state(), ConversationState::Speaking);
    assert_eq!(
        h.log,
        vec!["synth First one.", "play First one.", "synth Second one."]
    );
    let actions = h.controller.reply_finished();
    h.apply(actions);
    h.finish_playback();
    h.run_synthesis();
    h.finish_playback();
    h.run_synthesis();
    h.finish_playback();
    assert_eq!(
        h.log,
        vec![
            "synth First one.",
            "play First one.",
            "synth Second one.",
            "play Second one.",
            "synth Third",
            "play Third",
        ]
    );
    assert_eq!(h.controller.state(), ConversationState::Listening);
}

#[test]
fn barge_in_stops_playback_and_drops_pending_sentences() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("One. Two. Three. ");
    h.apply(actions);
    h.run_synthesis();
    let reply = h.controller.reply();
    let now = Instant::now();
    h.controller.speech_started(now);
    let actions = h.controller.check_barge_in(now + Duration::from_millis(60));
    assert!(actions.is_empty(), "a short noise does not interrupt");
    let actions = h.controller.check_barge_in(now + BARGE_IN_AFTER);
    h.apply(actions);
    assert_eq!(
        &h.log[h.log.len() - 2..],
        ["stop".to_owned(), format!("interrupted {reply}")]
    );
    assert_eq!(h.controller.state(), ConversationState::Listening);
    // The synthesis that was in flight finishes late and is dropped.
    h.log.clear();
    h.run_synthesis();
    let actions = h.controller.synthesized(
        reply,
        1,
        Some((
            "Two.".to_owned(),
            Audio {
                samples: Vec::new(),
                sample_rate: 24_000,
            },
        )),
    );
    h.apply(actions);
    assert!(h.log.is_empty());
    // Late text from the interrupted reply is not spoken either.
    let actions = h.controller.reply_text("Four. ");
    assert!(actions.is_empty());
}

#[test]
fn speech_that_ends_quickly_does_not_barge_in() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Hello there. ");
    h.apply(actions);
    h.run_synthesis();
    let now = Instant::now();
    h.controller.speech_started(now);
    h.controller.speech_ended();
    assert!(
        h.controller
            .check_barge_in(now + Duration::from_secs(1))
            .is_empty()
    );
    assert_eq!(h.controller.state(), ConversationState::Speaking);
}

#[test]
fn saying_stop_ends_the_speech_without_sending() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("A long answer. ");
    h.apply(actions);
    h.run_synthesis();
    let (outcome, actions) = h.controller.on_final("Stop.");
    assert_eq!(outcome, FinalOutcome::Stopped);
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("stop"));
    assert_eq!(h.controller.state(), ConversationState::Listening);
    let (outcome, _) = h.controller.on_final("now run the tests");
    assert_eq!(outcome, FinalOutcome::NotConsumed);
}

#[test]
fn approval_prompt_is_spoken_and_answered_by_voice() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Let me check");
    h.apply(actions);
    let actions = h.controller.approval_requested(9, "cargo check", false);
    h.apply(actions);
    h.run_synthesis();
    h.finish_playback();
    h.run_synthesis();
    assert_eq!(
        h.log,
        vec![
            "synth Let me check",
            "play Let me check",
            "synth I want to run cargo check. Say yes or no.",
            "play I want to run cargo check. Say yes or no.",
        ]
    );
    assert_eq!(
        h.controller.on_final("maybe later").0,
        FinalOutcome::NotConsumed
    );
    assert_eq!(
        h.controller.on_final("Run it.").0,
        FinalOutcome::Answered(9, ApprovalReply::Approve)
    );
    assert_eq!(h.controller.on_final("yes").0, FinalOutcome::NotConsumed);
}

#[test]
fn destructive_approvals_are_not_answered_by_voice() {
    let mut h = Harness::new();
    let actions = h.controller.approval_requested(1, "rm -rf build", true);
    h.apply(actions);
    assert_eq!(
        h.log,
        vec!["synth I want to run rm dash r f build. Please approve it on screen."]
    );
    assert_eq!(h.controller.on_final("yes").0, FinalOutcome::NotConsumed);
}

#[test]
fn failed_synthesis_skips_the_sentence() {
    let mut h = Harness::new();
    h.speaker.fail_on = Some("Bad.".to_owned());
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Bad. Good. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.log, vec!["synth Bad.", "synth Good.", "play Good."]);
}

#[test]
fn a_new_prompt_replaces_the_reply_being_spoken() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Old answer. ");
    h.apply(actions);
    h.run_synthesis();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("stop"));
    assert_eq!(h.controller.state(), ConversationState::Thinking);
}

#[test]
fn nothing_happens_while_off() {
    let mut controller: ConversationController<(), u32> = ConversationController::default();
    assert!(controller.prompt_sent().is_empty());
    assert!(controller.reply_text("Hello. ").is_empty());
    assert_eq!(controller.on_final("stop").0, FinalOutcome::NotConsumed);
}
