use std::cell::RefCell;

use super::*;
use crate::Audio;
use crate::speech::approval::{APPROVAL_LISTEN_FOR, ECHO_TAIL};
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

/// Runs the controller against a fake speaker, a fake output device whose playback the test
/// finishes by hand, and a fake clock.
struct Harness {
    controller: ConversationController<(String, Audio), u32>,
    speaker: FakeSpeaker,
    /// Synthesis requests not yet run.
    synth_queue: RefCell<Vec<(u64, u64, String)>>,
    playing: Option<(u64, u64, String)>,
    log: Vec<String>,
    now: Instant,
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
            now: Instant::now(),
        }
    }

    fn advance(&mut self, by: Duration) {
        self.now += by;
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
                let actions = self.controller.synthesized(reply, seq, audio, self.now);
                self.apply(actions);
            }
        }
    }

    fn finish_playback(&mut self) {
        let (reply, seq, _) = self.playing.take().expect("something is playing");
        let actions = self.controller.playback_finished(reply, seq, self.now);
        self.apply(actions);
    }

    /// The user says `text` now, as one utterance from speech start to final.
    fn say(&mut self, text: &str) -> FinalOutcome<u32> {
        self.controller.speech_started(self.now);
        self.controller.speech_ended();
        let (outcome, actions) = self.controller.on_final(text, self.now);
        self.apply(actions);
        outcome
    }

    /// Speaks an approval prompt for `id` to the end and waits out the echo tail.
    fn play_approval_prompt(&mut self, id: u32, command: &str) {
        let actions = self.controller.approval_requested(id, command, false);
        self.apply(actions);
        self.run_synthesis();
        self.finish_playback();
        self.advance(ECHO_TAIL);
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
    h.controller.set_headphones(true);
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("One. Two. Three. ");
    h.apply(actions);
    h.run_synthesis();
    let reply = h.controller.reply();
    let now = h.now;
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
        now,
    );
    h.apply(actions);
    assert!(h.log.is_empty());
    // Late text from the interrupted reply is not spoken either.
    let actions = h.controller.reply_text("Four. ");
    assert!(actions.is_empty());
    // The barged-in utterance itself reaches the input.
    let (outcome, _) = h.controller.on_final("actually run the tests", now);
    assert_eq!(outcome, FinalOutcome::NotConsumed);
}

#[test]
fn barge_in_counts_speech_that_started_before_playback() {
    let mut h = Harness::new();
    h.controller.set_headphones(true);
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    // The user starts talking while the agent is still thinking.
    let started = h.now;
    h.controller.speech_started(started);
    h.advance(Duration::from_secs(1));
    let actions = h.controller.reply_text("Hello there. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.controller.state(), ConversationState::Speaking);
    // Overlap is measured from when the audio started, and re-checking while the user keeps
    // talking interrupts once it is long enough.
    assert!(h.controller.check_barge_in(h.now).is_empty());
    assert!(h.controller.is_user_talking());
    let actions = h.controller.check_barge_in(h.now + BARGE_IN_AFTER);
    h.apply(actions);
    assert_eq!(h.controller.state(), ConversationState::Listening);
}

#[test]
fn loudspeakers_without_echo_cancellation_never_barge_in() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("A long answer. ");
    h.apply(actions);
    h.run_synthesis();
    let now = h.now;
    h.controller.speech_started(now);
    assert!(
        h.controller
            .check_barge_in(now + Duration::from_secs(5))
            .is_empty()
    );
    assert_eq!(h.controller.state(), ConversationState::Speaking);
    assert!(h.controller.is_echo_window(now));
    // Sustained echo is still dropped rather than sent as a prompt.
    h.controller.speech_ended();
    let (outcome, _) = h.controller.on_final("a long answer", now);
    assert_eq!(outcome, FinalOutcome::Echo);
}

#[test]
fn echo_cancellation_allows_quick_barge_in_on_loudspeakers() {
    let mut h = Harness::new();
    h.controller.set_echo_cancelled(true);
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("A long answer. ");
    h.apply(actions);
    h.run_synthesis();
    let now = h.now;
    assert!(!h.controller.is_echo_window(now));
    h.controller.speech_started(now);
    assert!(h.controller.check_barge_in(now + BARGE_IN_AFTER).is_empty());
    let actions = h
        .controller
        .check_barge_in(now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION);
    h.apply(actions);
    assert_eq!(h.controller.state(), ConversationState::Listening);
    h.controller.speech_ended();
    let (outcome, _) = h.controller.on_final(
        "wait, run the tests instead",
        now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION,
    );
    assert_eq!(outcome, FinalOutcome::NotConsumed);
}

#[test]
fn speech_under_way_when_playback_starts_is_echo() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Here is the plan. ");
    h.apply(actions);
    h.controller.speech_started(h.now);
    h.run_synthesis();
    h.controller.speech_ended();
    h.finish_playback();
    h.advance(ECHO_TAIL);
    let (outcome, _) = h.controller.on_final("something else entirely", h.now);
    assert_eq!(outcome, FinalOutcome::Echo);
}

#[test]
fn a_final_without_speech_start_during_the_tail_is_echo() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Done. ");
    h.apply(actions);
    h.run_synthesis();
    h.finish_playback();
    h.advance(ECHO_TAIL / 2);
    assert!(h.controller.is_echo_window(h.now));
    let (outcome, _) = h.controller.on_final("unrelated words here", h.now);
    assert_eq!(outcome, FinalOutcome::Echo);
}

#[test]
fn transcripts_repeating_recent_speech_are_dropped_after_the_tail() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h
        .controller
        .reply_text("The build has two errors in main. ");
    h.apply(actions);
    h.run_synthesis();
    h.finish_playback();
    h.advance(ECHO_TAIL * 2);
    assert!(!h.controller.is_echo_window(h.now));
    assert_eq!(h.say("the build has two errors"), FinalOutcome::Echo);
    assert_eq!(h.say("Two errors in main."), FinalOutcome::Echo);
    assert_eq!(h.say("fix the two errors"), FinalOutcome::NotConsumed);
    h.advance(RECENT_SPEECH_WINDOW);
    assert_eq!(h.say("the build has two errors"), FinalOutcome::NotConsumed);
}

#[test]
fn enough_stops_speech_in_the_echo_tail() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("First. Second sentence here. ");
    h.apply(actions);
    h.run_synthesis();
    h.finish_playback();
    assert_eq!(h.say("enough"), FinalOutcome::Stopped);
    assert_eq!(h.controller.state(), ConversationState::Listening);
}

#[test]
fn speech_over_loudspeakers_that_never_barges_in_is_dropped_as_echo() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("The build has two errors. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.say("the build has two errors"), FinalOutcome::Echo);
    assert_eq!(h.controller.state(), ConversationState::Speaking);
    h.finish_playback();
    // Still within the echo tail.
    assert_eq!(h.say("two errors"), FinalOutcome::Echo);
    h.advance(ECHO_TAIL);
    assert_eq!(h.say("fix them"), FinalOutcome::NotConsumed);
}

#[test]
fn headphones_trust_speech_over_the_assistant() {
    let mut h = Harness::new();
    h.controller.set_headphones(true);
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("An answer. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.say("and the tests"), FinalOutcome::NotConsumed);
}

#[test]
fn speech_that_ends_quickly_does_not_barge_in() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Hello there. ");
    h.apply(actions);
    h.run_synthesis();
    let now = h.now;
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
    assert_eq!(h.say("Stop."), FinalOutcome::Stopped);
    assert_eq!(h.log.last().map(String::as_str), Some("stop"));
    assert_eq!(h.controller.state(), ConversationState::Listening);
    h.advance(ECHO_TAIL);
    assert_eq!(h.say("now run the tests"), FinalOutcome::NotConsumed);
    // A later reply of the interrupted turn stays silent.
    h.controller.reply_started();
    assert!(h.controller.reply_text("More. ").is_empty());
}

#[test]
fn stop_is_a_prompt_while_listening() {
    let mut h = Harness::new();
    assert_eq!(h.say("stop"), FinalOutcome::NotConsumed);
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    assert_eq!(h.say("quiet"), FinalOutcome::Stopped);
}

#[test]
fn follow_up_replies_of_a_turn_are_spoken() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Checking. ");
    h.apply(actions);
    let actions = h.controller.reply_finished();
    h.apply(actions);
    h.run_synthesis();
    h.finish_playback();
    assert_eq!(h.controller.state(), ConversationState::Listening);
    h.controller.reply_started();
    assert_eq!(h.controller.state(), ConversationState::Thinking);
    let actions = h.controller.reply_text("Done. ");
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("synth Done."));
}

#[test]
fn approval_prompt_is_spoken_and_answered_by_voice() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Let me check");
    h.apply(actions);
    let actions = h.controller.approval_requested(9, "run cargo check", false);
    h.apply(actions);
    h.run_synthesis();
    h.finish_playback();
    h.run_synthesis();
    assert_eq!(
        h.log,
        vec![
            "synth Let me check",
            "play Let me check",
            "synth I want to run cargo check. Say run it, or no.",
            "play I want to run cargo check. Say run it, or no.",
        ]
    );
    // Said while the prompt plays: dropped, the approval stays pending.
    assert_eq!(h.say("Run it."), FinalOutcome::Echo);
    assert!(h.controller.approvals().pending().is_some());
    h.finish_playback();
    // Within the echo tail after the prompt.
    assert_eq!(h.say("Run it."), FinalOutcome::Echo);
    h.advance(ECHO_TAIL);
    assert_eq!(h.say("maybe later"), FinalOutcome::NotConsumed);
    assert_eq!(
        h.say("Run it."),
        FinalOutcome::Answered(9, ApprovalReply::Approve)
    );
    assert_eq!(h.say("yes"), FinalOutcome::NotConsumed);
}

#[test]
fn approval_answers_need_speech_that_started_after_the_prompt() {
    let mut h = Harness::new();
    h.controller.set_headphones(true);
    let actions = h.controller.approval_requested(4, "ls", false);
    h.apply(actions);
    h.run_synthesis();
    // The user starts talking before the prompt ends; the final arrives afterwards.
    h.controller.speech_started(h.now);
    h.finish_playback();
    h.advance(Duration::from_secs(1));
    h.controller.speech_ended();
    let (outcome, _) = h.controller.on_final("yes", h.now);
    assert_eq!(outcome, FinalOutcome::Ignored);
    // A final with no recorded speech start is never an answer.
    let (outcome, _) = h.controller.on_final("yes", h.now);
    assert_eq!(outcome, FinalOutcome::Ignored);
    assert_eq!(
        h.say("no"),
        FinalOutcome::Answered(4, ApprovalReply::Reject)
    );
}

#[test]
fn an_unplayed_approval_prompt_is_never_answered_by_voice() {
    let mut h = Harness::new();
    let actions = h.controller.approval_requested(2, "ls", false);
    h.apply(actions);
    h.run_synthesis();
    // "Stop" silences the prompt before it finishes.
    assert_eq!(h.say("stop"), FinalOutcome::Stopped);
    h.advance(Duration::from_secs(1));
    assert_eq!(h.say("yes"), FinalOutcome::Ignored);
}

#[test]
fn spoken_approvals_time_out_to_the_screen() {
    let mut h = Harness::new();
    h.play_approval_prompt(3, "ls");
    assert!(h.controller.check_approval_timeout(h.now).is_empty());
    h.advance(APPROVAL_LISTEN_FOR);
    let actions = h.controller.check_approval_timeout(h.now);
    h.apply(actions);
    assert_eq!(
        h.log.last().map(String::as_str),
        Some("synth I'll wait for you to approve it on screen.")
    );
    h.run_synthesis();
    h.finish_playback();
    h.advance(ECHO_TAIL);
    assert_eq!(h.say("yes"), FinalOutcome::Ignored);
    assert!(h.controller.approvals().pending().is_some());
}

#[test]
fn destructive_approvals_are_not_answered_by_voice() {
    let mut h = Harness::new();
    let actions = h
        .controller
        .approval_requested(1, "run a command that deletes files", true);
    h.apply(actions);
    assert_eq!(
        h.log,
        vec!["synth I want to run a command that deletes files. Approve on screen."]
    );
    h.run_synthesis();
    h.finish_playback();
    h.advance(ECHO_TAIL);
    assert_eq!(h.say("yes"), FinalOutcome::Ignored);
    assert_eq!(h.say("run it"), FinalOutcome::Ignored);
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
    assert_eq!(
        controller.on_final("stop", Instant::now()).0,
        FinalOutcome::NotConsumed
    );
}

#[test]
fn recognizes_headphone_devices() {
    assert!(is_headphone_device_name("AirPods Pro"));
    assert!(is_headphone_device_name("External Headphones"));
    assert!(!is_headphone_device_name("MacBook Pro Speakers"));
}

#[test]
fn stop_speaking_drops_the_rest_of_the_reply_and_speaks_the_next() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("One. Two. Three. ");
    h.apply(actions);
    h.run_synthesis();
    assert!(h.controller.is_speaking());
    h.log.clear();
    let now = h.now;
    let actions = h.controller.stop_speaking(now);
    h.apply(actions);
    assert_eq!(h.log, vec!["stop"]);
    assert_eq!(h.controller.state(), ConversationState::Listening);
    assert!(!h.controller.is_speaking());

    // More text of the stopped reply is not read.
    let actions = h.controller.reply_text("Four. ");
    h.apply(actions);
    assert!(h.controller.stop_speaking(now).is_empty());
    h.run_synthesis();
    assert_eq!(h.log, vec!["stop"]);

    // The follow-up reply is spoken normally.
    h.controller.reply_started();
    let actions = h.controller.reply_text("Done. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.log, vec!["stop", "synth Done.", "play Done."]);
}

#[test]
fn mute_silences_readout_until_unmuted() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("One. Two. ");
    h.apply(actions);
    h.run_synthesis();
    h.log.clear();
    let now = h.now;
    let actions = h.controller.set_muted(true, now);
    h.apply(actions);
    assert_eq!(h.log, vec!["stop"]);
    assert!(h.controller.is_muted());
    assert_eq!(h.controller.state(), ConversationState::Thinking);

    let actions = h.controller.reply_text("Three. ");
    h.apply(actions);
    let actions = h.controller.speak("Ran cargo check: ok.".to_owned());
    h.apply(actions);
    let actions = h.controller.approval_requested(1, "cargo test", false);
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.log, vec!["stop"]);

    let actions = h.controller.set_muted(false, now);
    h.apply(actions);
    let actions = h.controller.reply_text("Four. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.log, vec!["stop", "synth Four.", "play Four."]);
}
