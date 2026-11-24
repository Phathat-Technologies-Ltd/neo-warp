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
                PlaybackAction::Duck { gain } => self.log.push(format!("duck {gain}")),
                PlaybackAction::Restore => self.log.push("restore".to_owned()),
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

    /// Requests approval `id` and lets the debounce pass so its prompt is issued.
    fn request_approval(&mut self, id: u32, subject: &str, needs_screen: bool) {
        self.controller
            .approval_requested(id, subject, needs_screen, self.now);
        self.advance(APPROVAL_DEBOUNCE);
        let actions = self.controller.check_approval_due(self.now);
        self.apply(actions);
    }

    /// Speaks an approval prompt for `id` to the end and waits out the echo tail.
    fn play_approval_prompt(&mut self, id: u32, command: &str) {
        self.request_approval(id, command, false);
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

/// A long utterance often overlaps a reply sentence that starts playing while the user talks; once
/// its speaker check matched the user, it reaches the input instead of being dropped as echo.
#[test]
fn a_speaker_verified_long_utterance_over_playback_is_not_echo() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Here is the plan. ");
    h.apply(actions);
    h.controller.speech_started(h.now);
    h.run_synthesis();
    h.finish_playback();
    h.advance(Duration::from_secs(11));
    h.controller.speech_ended();
    let (outcome, _) = h.controller.on_final_from(
        "please refactor the parser so it handles nested quotes and then run the tests",
        true,
        h.now,
    );
    assert_eq!(outcome, FinalOutcome::NotConsumed);
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
    h.request_approval(9, "run cargo check", false);
    h.run_synthesis();
    h.finish_playback();
    h.run_synthesis();
    assert_eq!(
        h.log,
        vec![
            "synth Let me check",
            "play Let me check",
            "synth I want to run cargo check. Say yes, or no.",
            "play I want to run cargo check. Say yes, or no.",
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
    h.request_approval(4, "ls", false);
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
    h.request_approval(2, "ls", false);
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
    h.request_approval(1, "run a command that deletes files", true);
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
fn stopping_mid_reply_silences_it_and_drops_its_echo() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h
        .controller
        .reply_text("Here is the plan for the refactor. ");
    h.apply(actions);
    h.run_synthesis();
    h.controller.speech_started(h.now);
    let actions = h.controller.stop(h.now);
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("stop"));
    assert!(h.controller.is_echo_window(h.now));
    h.controller.speech_ended();
    let (outcome, _) = h
        .controller
        .on_final("here is the plan for the refactor", h.now);
    assert_eq!(outcome, FinalOutcome::Echo);
}

#[test]
fn restarting_after_a_stop_drops_the_previous_reply_echo() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h
        .controller
        .reply_text("Here is the plan for the refactor. Then we test it. ");
    h.apply(actions);
    h.run_synthesis();
    let stopped_reply = h.controller.reply();
    h.controller.speech_started(h.now);
    let actions = h.controller.stop(h.now);
    h.apply(actions);
    h.controller.start(ChunkerOptions::default());
    h.advance(ECHO_TAIL);
    h.controller.speech_ended();
    let (outcome, _) = h.controller.on_final("the plan for the refactor", h.now);
    assert_eq!(outcome, FinalOutcome::Echo);
    assert!(!h.controller.is_speaking());
    assert!(
        h.controller
            .synthesized(stopped_reply, 1, None, h.now)
            .is_empty()
    );
    assert_eq!(h.say("run the tests please"), FinalOutcome::NotConsumed);
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
    h.request_approval(1, "cargo test", false);
    h.run_synthesis();
    assert_eq!(h.log, vec!["stop"]);

    let actions = h.controller.set_muted(false, now);
    h.apply(actions);
    let actions = h.controller.reply_text("Four. ");
    h.apply(actions);
    h.run_synthesis();
    assert_eq!(h.log, vec!["stop", "synth Four.", "play Four."]);
}

#[test]
fn approval_resolved_during_the_debounce_is_never_spoken() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    h.controller.approval_requested(1, "run ls", false, h.now);
    h.advance(Duration::from_millis(500));
    assert!(h.controller.check_approval_due(h.now).is_empty());
    let actions = h.controller.approval_resolved(&1, h.now);
    h.apply(actions);
    assert_eq!(h.controller.approval_due_at(), None);
    h.advance(APPROVAL_DEBOUNCE);
    let actions = h.controller.check_approval_due(h.now);
    h.apply(actions);
    assert!(h.log.is_empty());
    assert!(h.controller.approvals().pending().is_none());
}

#[test]
fn approval_resolved_while_its_prompt_plays_stops_it() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    h.request_approval(1, "run ls", true);
    h.run_synthesis();
    assert_eq!(
        h.log.last().map(String::as_str),
        Some("play I want to run ls. Approve on screen.")
    );
    let actions = h.controller.approval_resolved(&1, h.now);
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("stop"));
    assert!(h.controller.approvals().pending().is_none());
    assert!(!h.controller.is_speaking());
    assert_eq!(h.controller.state(), ConversationState::Thinking);
}

#[test]
fn approval_resolved_while_its_prompt_is_queued_drops_it() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("Let me check. ");
    h.apply(actions);
    h.run_synthesis();
    h.request_approval(1, "run ls", false);
    let actions = h.controller.approval_resolved(&1, h.now);
    h.apply(actions);
    h.finish_playback();
    h.run_synthesis();
    assert!(!h.log.iter().any(|line| line.starts_with("play I want to")));
    assert!(h.playing.is_none());
}

#[test]
fn quick_approval_requests_share_one_prompt() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    for id in 1..=3 {
        h.controller.approval_requested(id, "run ls", false, h.now);
        h.advance(Duration::from_millis(200));
    }
    h.advance(APPROVAL_DEBOUNCE);
    let actions = h.controller.check_approval_due(h.now);
    h.apply(actions);
    assert_eq!(
        h.log,
        vec!["synth Three commands need your approval. Approve on screen."]
    );
    h.run_synthesis();
    h.finish_playback();
    h.advance(ECHO_TAIL);
    assert_eq!(h.say("yes"), FinalOutcome::Ignored);
    // Requests already announced are not announced again.
    h.controller.approval_requested(2, "run ls", false, h.now);
    assert_eq!(h.controller.approval_due_at(), None);
}

#[test]
fn resolved_approvals_are_not_announced_again() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    h.play_approval_prompt(1, "run ls");
    let actions = h.controller.approval_resolved(&1, h.now);
    h.apply(actions);
    h.controller.approval_requested(1, "run ls", false, h.now);
    assert_eq!(h.controller.approval_due_at(), None);
    // A new request after the resolved one is announced after the debounce, with no gap.
    h.controller.approval_requested(2, "run pwd", false, h.now);
    assert_eq!(
        h.controller.approval_due_at(),
        Some(h.now + APPROVAL_DEBOUNCE)
    );
}

#[test]
fn unresolved_approval_prompts_are_spaced_out() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let prompted_at = h.now + APPROVAL_DEBOUNCE;
    h.request_approval(1, "run ls", true);
    h.controller.approval_requested(2, "run pwd", true, h.now);
    assert_eq!(
        h.controller.approval_due_at(),
        Some(prompted_at + APPROVAL_PROMPT_GAP)
    );
    h.advance(APPROVAL_DEBOUNCE);
    assert!(h.controller.check_approval_due(h.now).is_empty());
}

/// A loudspeaker conversation with echo cancellation and verified barge-in, speaking a reply.
fn verified_barge_in_harness() -> Harness {
    let mut h = Harness::new();
    h.controller.set_echo_cancelled(true);
    h.controller.set_verify_barge_in(true);
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    let actions = h.controller.reply_text("A long answer. Another sentence. ");
    h.apply(actions);
    h.run_synthesis();
    h.log.clear();
    h
}

fn duck(h: &mut Harness) -> Instant {
    let now = h.now;
    h.controller.speech_started(now);
    let actions = h
        .controller
        .check_barge_in(now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION);
    h.apply(actions);
    assert_eq!(h.log, [format!("duck {BARGE_IN_DUCK_GAIN}")]);
    assert!(h.controller.is_barge_in_pending());
    now
}

#[test]
fn verified_barge_in_resumes_after_echo() {
    let mut h = verified_barge_in_harness();
    let now = duck(&mut h);
    assert_eq!(h.controller.state(), ConversationState::Speaking);
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::NoMatch, now + BARGE_IN_CHECK_AFTER);
    assert!(actions.is_empty());
    assert!(h.controller.is_barge_in_pending());
    let actions = h
        .controller
        .barge_in_transcript("a long answer", now + Duration::from_millis(900));
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("restore"));
    // The dismissed speech never ducks or interrupts again.
    let actions = h.controller.check_barge_in(now + Duration::from_secs(1));
    assert!(actions.is_empty());
    assert_eq!(h.controller.state(), ConversationState::Speaking);
    assert!(!h.log.iter().any(|entry| entry.starts_with("interrupted")));
    h.controller.speech_ended();
    let (outcome, _) = h
        .controller
        .on_final("a long answer", now + Duration::from_secs(1));
    assert_eq!(outcome, FinalOutcome::Echo);
}

#[test]
fn verified_barge_in_stops_for_user_text_despite_speaker_mismatch() {
    let mut h = verified_barge_in_harness();
    let reply = h.controller.reply();
    let now = duck(&mut h);
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::NoMatch, now + BARGE_IN_CHECK_AFTER);
    assert!(actions.is_empty());
    let actions = h
        .controller
        .barge_in_transcript("hold on please", now + Duration::from_millis(900));
    h.apply(actions);
    assert!(h.log.contains(&format!("interrupted {reply}")));
    assert_eq!(h.controller.state(), ConversationState::Listening);
    h.controller.speech_ended();
    let (outcome, _) = h
        .controller
        .on_final("hold on please", now + Duration::from_secs(1));
    assert_eq!(outcome, FinalOutcome::NotConsumed);
}

#[test]
fn verified_barge_in_stops_for_a_user_final() {
    let mut h = verified_barge_in_harness();
    let reply = h.controller.reply();
    let now = duck(&mut h);
    h.controller.speech_ended();
    let (outcome, actions) = h
        .controller
        .on_final("wait run the tests", now + Duration::from_millis(900));
    h.apply(actions);
    assert_eq!(outcome, FinalOutcome::NotConsumed);
    assert!(h.log.contains(&format!("interrupted {reply}")));
}

#[test]
fn verified_barge_in_stays_ducked_when_the_check_is_inconclusive() {
    let mut h = verified_barge_in_harness();
    let now = duck(&mut h);
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::Unavailable, now + BARGE_IN_CHECK_AFTER);
    assert!(actions.is_empty());
    let actions = h
        .controller
        .barge_in_transcript("hmm", now + Duration::from_millis(900));
    assert!(actions.is_empty());
    assert!(h.controller.is_barge_in_pending());
    assert_eq!(h.controller.state(), ConversationState::Speaking);
}

#[test]
fn verified_barge_in_stops_on_a_later_match() {
    let mut h = verified_barge_in_harness();
    let now = duck(&mut h);
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::NoMatch, now + BARGE_IN_CHECK_AFTER);
    assert!(actions.is_empty());
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::Match, now + BARGE_IN_RECHECKS[0]);
    h.apply(actions);
    assert_eq!(h.controller.state(), ConversationState::Listening);
}

#[test]
fn verified_barge_in_stops_for_the_user() {
    let mut h = verified_barge_in_harness();
    let reply = h.controller.reply();
    let now = h.now;
    h.controller.speech_started(now);
    let actions = h
        .controller
        .check_barge_in(now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION);
    h.apply(actions);
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::Match, now + BARGE_IN_CHECK_AFTER);
    h.apply(actions);
    assert_eq!(
        h.log,
        [
            format!("duck {BARGE_IN_DUCK_GAIN}"),
            "stop".to_owned(),
            "restore".to_owned(),
            format!("interrupted {reply}"),
        ]
    );
    assert_eq!(h.controller.state(), ConversationState::Listening);
    h.controller.speech_ended();
    let (outcome, _) = h
        .controller
        .on_final("wait, run the tests", now + Duration::from_secs(1));
    assert_eq!(outcome, FinalOutcome::NotConsumed);
}

#[test]
fn verified_barge_in_resumes_when_the_check_times_out() {
    let mut h = verified_barge_in_harness();
    let now = h.now;
    h.controller.speech_started(now);
    let ducked_at = now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION;
    let actions = h.controller.check_barge_in(ducked_at);
    h.apply(actions);
    h.controller.speech_ended();
    assert!(
        h.controller
            .check_barge_in(ducked_at + Duration::from_secs(1))
            .is_empty()
    );
    let actions = h
        .controller
        .check_barge_in(ducked_at + BARGE_IN_VERDICT_TIMEOUT);
    h.apply(actions);
    assert_eq!(h.log.last().map(String::as_str), Some("restore"));
    assert!(!h.controller.is_barge_in_pending());
    assert_eq!(h.controller.state(), ConversationState::Speaking);
}

#[test]
fn stop_phrase_interrupts_a_ducked_reply_at_once() {
    let mut h = verified_barge_in_harness();
    let now = h.now;
    h.controller.speech_started(now);
    let actions = h
        .controller
        .check_barge_in(now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION);
    h.apply(actions);
    h.controller.speech_ended();
    let (outcome, actions) = h
        .controller
        .on_final("stop", now + Duration::from_millis(300));
    h.apply(actions);
    assert_eq!(outcome, FinalOutcome::Stopped);
    assert_eq!(h.log[1..], ["stop".to_owned(), "restore".to_owned()]);
    assert_eq!(h.controller.state(), ConversationState::Listening);
}

#[test]
fn unverified_barge_in_interrupts_without_ducking() {
    let mut h = verified_barge_in_harness();
    h.controller.set_verify_barge_in(false);
    let now = h.now;
    h.controller.speech_started(now);
    let actions = h
        .controller
        .check_barge_in(now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION);
    h.apply(actions);
    assert!(!h.log.iter().any(|entry| entry.starts_with("duck")));
    assert_eq!(h.log[0], "stop");
    assert_eq!(h.controller.state(), ConversationState::Listening);
}

#[test]
fn an_approval_answer_that_lost_its_first_word_is_asked_again() {
    let mut h = Harness::new();
    h.play_approval_prompt(5, "look around the project");
    h.log.clear();
    // "Run it" reached the recognizer as just "It.": never a prompt, and the question is repeated.
    assert_eq!(h.say("It."), FinalOutcome::Ignored);
    assert_eq!(h.log, vec!["synth Sorry, say yes, or no."]);
    assert!(h.controller.approvals().pending().is_some());
    h.run_synthesis();
    h.finish_playback();
    h.advance(ECHO_TAIL);
    assert_eq!(
        h.say("Run it."),
        FinalOutcome::Answered(5, ApprovalReply::Approve)
    );
}

#[test]
fn an_approval_answer_split_in_two_utterances_answers_once() {
    let mut h = Harness::new();
    h.play_approval_prompt(5, "ls");
    assert_eq!(
        h.say("Run"),
        FinalOutcome::Answered(5, ApprovalReply::Approve)
    );
    h.advance(Duration::from_millis(500));
    assert_eq!(h.say("it."), FinalOutcome::Ignored);

    let mut h = Harness::new();
    h.play_approval_prompt(6, "ls");
    assert_eq!(h.say("Okay"), FinalOutcome::Ignored);
    h.advance(Duration::from_millis(500));
    assert_eq!(
        h.say("run it"),
        FinalOutcome::Answered(6, ApprovalReply::Approve)
    );
}

#[test]
fn stop_rejects_a_played_approval_prompt() {
    let mut h = Harness::new();
    let actions = h.controller.prompt_sent();
    h.apply(actions);
    // The agent's turn is still open while it waits on the approval.
    h.play_approval_prompt(5, "ls");
    assert_eq!(
        h.say("stop"),
        FinalOutcome::Answered(5, ApprovalReply::Reject)
    );
}

#[test]
fn barging_in_on_an_approval_prompt_keeps_the_approval_answerable() {
    let mut h = Harness::new();
    h.controller.set_echo_cancelled(true);
    h.controller.set_verify_barge_in(true);
    h.request_approval(5, "look around the project", false);
    h.run_synthesis();
    let now = h.now;
    h.controller.speech_started(now);
    let actions = h
        .controller
        .check_barge_in(now + BARGE_IN_AFTER_WITH_ECHO_CANCELLATION);
    h.apply(actions);
    // A one-word partial neither interrupts nor answers.
    let actions = h
        .controller
        .barge_in_transcript("run", now + Duration::from_millis(400));
    assert!(actions.is_empty());
    let actions = h
        .controller
        .barge_in_verdict(BargeInSpeaker::Match, now + BARGE_IN_CHECK_AFTER);
    h.apply(actions);
    assert_eq!(h.controller.state(), ConversationState::Listening);
    // The interruption alone answers nothing.
    assert!(h.controller.approvals().pending().is_some());
    h.controller.speech_ended();
    let (outcome, _) =
        h.controller
            .on_final_from("Run it.", true, now + Duration::from_millis(900));
    assert_eq!(outcome, FinalOutcome::Answered(5, ApprovalReply::Approve));
}

#[test]
fn barging_in_on_an_approval_prompt_with_other_words_never_answers_it() {
    let mut h = Harness::new();
    h.controller.set_headphones(true);
    h.request_approval(5, "ls", false);
    h.run_synthesis();
    let now = h.now;
    h.controller.speech_started(now);
    let actions = h.controller.check_barge_in(now + BARGE_IN_AFTER);
    h.apply(actions);
    h.controller.speech_ended();
    let (outcome, _) = h
        .controller
        .on_final("what does it do", now + Duration::from_secs(1));
    assert_eq!(outcome, FinalOutcome::NotConsumed);
    assert!(h.controller.approvals().pending().is_some());
    h.advance(Duration::from_secs(2));
    assert_eq!(
        h.say("yes"),
        FinalOutcome::Answered(5, ApprovalReply::Approve)
    );
}
