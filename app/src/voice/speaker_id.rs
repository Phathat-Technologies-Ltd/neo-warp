//! "Recognize my voice": the user's voiceprint, its guided enrollment, and the background worker
//! that embeds utterances. The voiceprint is stored only on this computer, readable only by the
//! user, and never sent anywhere.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::oneshot;
use futures::future::BoxFuture;
use local_transcription::speaker::{ENROLLMENT_PHRASES, Sensitivity, SpeakerEmbedder, Voiceprint};
use local_transcription::{OnnxSpeakerEmbedder, campplus_zh_en_advanced};
use voice_input::{LiveCapture, SpeechOutput};
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::local_model::{SpeakerIdModel, is_speaker_id_enabled, speaker_model_store};
use super::speaker_gate::Verdict;
use crate::settings::AISettings;

/// How long the loaded model stays in memory after its last use.
const IDLE_UNLOAD: Duration = Duration::from_secs(180);
/// Longest recording per enrollment phrase; the user normally stops it sooner.
const MAX_PHRASE_DURATION: Duration = Duration::from_secs(14);
/// RMS below which a 10 ms frame counts as silence when trimming enrollment audio.
const SILENCE_RMS: f32 = 0.01;
/// Speech needed per enrollment phrase after trimming silence.
const MIN_PHRASE_SPEECH_SECS: f32 = 2.5;
/// Speech quieter than this (RMS, dBFS) is too weak for a reliable voiceprint.
const MIN_SPEECH_DBFS: f32 = -38.0;
/// Share of samples at full scale above which the recording counts as clipped.
const MAX_CLIPPED_FRACTION: f32 = 0.002;
/// Speech must be at least this much louder than the background (dB).
const MIN_SPEECH_TO_NOISE_DB: f32 = 12.0;
/// How far above the threshold a match must score before it adapts the voiceprint.
const ADAPT_MARGIN: f32 = 0.10;
/// Speech a match needs before it adapts the voiceprint.
const MIN_ADAPT_SPEECH_SECS: f32 = 1.5;
/// Less speech than this gives an unreliable voiceprint score (the enrolled user scores below
/// the threshold on ~0.4 s replies like "yes"), so such utterances are let through unchecked.
const MIN_VERIFY_SPEECH_SECS: f32 = 0.7;

type EmbedResult = Result<Vec<f32>, String>;
type EmbedderFactory = Box<dyn Fn() -> Result<Box<dyn SpeakerEmbedder>, String> + Send>;

struct EmbedJob {
    samples: Vec<f32>,
    reply: oneshot::Sender<EmbedResult>,
}

/// Embeds audio on one low-priority background thread, loading the model on first use and
/// unloading it after `idle` without work.
pub struct EmbedWorker {
    jobs: mpsc::Sender<EmbedJob>,
}

impl EmbedWorker {
    pub fn spawn(factory: EmbedderFactory, idle: Duration) -> io::Result<Self> {
        let (jobs, rx) = mpsc::channel::<EmbedJob>();
        std::thread::Builder::new()
            .name("voice-speaker-id".to_owned())
            .spawn(move || {
                lower_thread_priority();
                let mut embedder: Option<Box<dyn SpeakerEmbedder>> = None;
                loop {
                    match rx.recv_timeout(idle) {
                        Ok(job) => {
                            if embedder.is_none() {
                                match factory() {
                                    Ok(loaded) => embedder = Some(loaded),
                                    Err(error) => {
                                        let _ = job.reply.send(Err(error));
                                        continue;
                                    }
                                }
                            }
                            let result = embedder
                                .as_mut()
                                .map(|embedder| {
                                    embedder.embed(&job.samples).map_err(|e| e.to_string())
                                })
                                .unwrap_or_else(|| Err("the speaker model isn't loaded".into()));
                            let _ = job.reply.send(result);
                        }
                        Err(RecvTimeoutError::Timeout) => embedder = None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self { jobs })
    }

    pub fn embed(&self, samples: Vec<f32>) -> BoxFuture<'static, EmbedResult> {
        let (reply, rx) = oneshot::channel();
        let sent = self.jobs.send(EmbedJob { samples, reply });
        Box::pin(async move {
            sent.map_err(|_| "the speaker worker stopped".to_owned())?;
            rx.await
                .map_err(|_| "the speaker worker stopped".to_owned())?
        })
    }
}

fn lower_thread_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: Sets the QoS class of the calling thread only.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

/// Where the voiceprint is stored.
pub fn voiceprint_path() -> PathBuf {
    warp_core::paths::data_dir()
        .join("voice")
        .join("voiceprint.json")
}

pub fn load_voiceprint(path: &Path) -> Option<Voiceprint> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes)
        .inspect_err(|error| log::warn!("Ignoring an unreadable voiceprint: {error}"))
        .ok()
}

/// Writes the voiceprint atomically, readable and writable only by the user.
pub fn save_voiceprint(path: &Path, voiceprint: &Voiceprint) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(&serde_json::to_vec(voiceprint).map_err(io::Error::other)?)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

pub fn delete_voiceprint(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// The speech in a recording: the span from the first to the last 10 ms frame above
/// [`SILENCE_RMS`].
pub fn trim_silence(samples: &[f32]) -> &[f32] {
    const FRAME: usize = 160;
    let loud = |frame: &[f32]| {
        (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt() >= SILENCE_RMS
    };
    let frames: Vec<&[f32]> = samples.chunks(FRAME).collect();
    let Some(first) = frames.iter().position(|frame| loud(frame)) else {
        return &[];
    };
    let last = frames
        .iter()
        .rposition(|frame| loud(frame))
        .unwrap_or(first);
    &samples[first * FRAME..((last + 1) * FRAME).min(samples.len())]
}

fn dbfs(rms: f32) -> f32 {
    20.0 * rms.max(1e-6).log10()
}

/// Why a recorded enrollment phrase can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityProblem {
    TooShort,
    TooQuiet,
    Clipped,
    TooNoisy,
}

impl QualityProblem {
    pub fn message(self) -> &'static str {
        match self {
            QualityProblem::TooShort => {
                "Didn't hear enough speech. Read the whole phrase at a normal pace."
            }
            QualityProblem::TooQuiet => {
                "Your voice was too quiet. Move closer to the microphone or speak up."
            }
            QualityProblem::Clipped => {
                "The recording is distorted. Move back from the microphone or speak more softly."
            }
            QualityProblem::TooNoisy => {
                "There's too much background noise. Try a quieter spot or move closer to the \
                 microphone."
            }
        }
    }
}

/// Measurements of a recorded enrollment phrase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhraseQuality {
    /// Length of the recording after trimming leading and trailing silence.
    pub speech_secs: f32,
    /// RMS level of the speech, dBFS.
    pub speech_dbfs: f32,
    /// RMS level of the quietest tenth of the recording's 10 ms frames, dBFS.
    pub noise_dbfs: f32,
    /// Share of samples at full scale.
    pub clipped_fraction: f32,
}

impl PhraseQuality {
    pub fn measure(recording: &[f32]) -> Self {
        let speech = trim_silence(recording);
        let rms = |samples: &[f32]| {
            (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
        };
        let mut frames: Vec<f32> = recording.chunks(160).map(rms).collect();
        frames.sort_by(f32::total_cmp);
        let quiet = &frames[..frames.len().div_ceil(10)];
        let noise = quiet.iter().sum::<f32>() / quiet.len().max(1) as f32;
        let clipped = recording.iter().filter(|s| s.abs() >= 0.99).count();
        Self {
            speech_secs: speech.len() as f32 / 16_000.0,
            speech_dbfs: dbfs(rms(speech)),
            noise_dbfs: dbfs(noise),
            clipped_fraction: clipped as f32 / recording.len().max(1) as f32,
        }
    }

    /// The first reason the phrase can't be used, if any.
    pub fn problem(&self) -> Option<QualityProblem> {
        if self.speech_secs < MIN_PHRASE_SPEECH_SECS {
            Some(QualityProblem::TooShort)
        } else if self.clipped_fraction > MAX_CLIPPED_FRACTION {
            Some(QualityProblem::Clipped)
        } else if self.speech_dbfs < MIN_SPEECH_DBFS {
            Some(QualityProblem::TooQuiet)
        } else if self.speech_dbfs - self.noise_dbfs < MIN_SPEECH_TO_NOISE_DB {
            Some(QualityProblem::TooNoisy)
        } else {
            None
        }
    }

    /// A one-line summary such as "6.2 s of speech, level -21 dBFS, background -58 dBFS".
    pub fn summary(&self) -> String {
        format!(
            "{:.1} s of speech, level {:.0} dBFS, background {:.0} dBFS",
            self.speech_secs, self.speech_dbfs, self.noise_dbfs
        )
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum EnrollmentState {
    #[default]
    Idle,
    /// Waiting for the user to record `ENROLLMENT_PHRASES[phrase]`.
    Ready {
        phrase: usize,
    },
    /// Recording the user reading `ENROLLMENT_PHRASES[phrase]`.
    Recording {
        phrase: usize,
    },
    /// The phrase was recorded; the user can play it back, record it again, or accept it.
    Review {
        phrase: usize,
        quality: PhraseQuality,
    },
    Processing,
    Failed(String),
}

/// The steps of guided enrollment, free of I/O: each phrase is recorded, checked, and accepted
/// before the next.
#[derive(Debug, Default)]
pub struct EnrollmentFlow {
    state: EnrollmentState,
    accepted: Vec<Vec<f32>>,
    /// The speech of the phrase under review.
    take: Vec<f32>,
}

impl EnrollmentFlow {
    pub fn state(&self) -> &EnrollmentState {
        &self.state
    }

    pub fn start(&mut self) {
        *self = Self {
            state: EnrollmentState::Ready { phrase: 0 },
            ..Self::default()
        };
    }

    pub fn cancel(&mut self) {
        *self = Self::default();
    }

    pub fn fail(&mut self, message: String) {
        *self = Self {
            state: EnrollmentState::Failed(message),
            ..Self::default()
        };
    }

    /// Starts recording the current phrase, discarding any take under review. Returns whether
    /// recording started.
    pub fn record(&mut self) -> bool {
        let phrase = match self.state {
            EnrollmentState::Ready { phrase } | EnrollmentState::Review { phrase, .. } => phrase,
            EnrollmentState::Idle
            | EnrollmentState::Recording { .. }
            | EnrollmentState::Processing
            | EnrollmentState::Failed(_) => return false,
        };
        self.take.clear();
        self.state = EnrollmentState::Recording { phrase };
        true
    }

    /// Ends the recording and checks its quality.
    pub fn finish_recording(&mut self, recording: &[f32]) {
        let EnrollmentState::Recording { phrase } = self.state else {
            return;
        };
        let quality = PhraseQuality::measure(recording);
        self.take = trim_silence(recording).to_vec();
        self.state = EnrollmentState::Review { phrase, quality };
    }

    /// The recorded speech under review, for playback.
    pub fn take(&self) -> Option<&[f32]> {
        matches!(self.state, EnrollmentState::Review { .. }).then_some(self.take.as_slice())
    }

    /// Accepts the phrase under review if it passed the quality check and moves to the next.
    /// Returns every accepted phrase once the last one is accepted.
    pub fn accept(&mut self) -> Option<Vec<Vec<f32>>> {
        let EnrollmentState::Review { phrase, quality } = self.state else {
            return None;
        };
        if quality.problem().is_some() {
            return None;
        }
        self.accepted.push(std::mem::take(&mut self.take));
        if phrase + 1 < ENROLLMENT_PHRASES.len() {
            self.state = EnrollmentState::Ready { phrase: phrase + 1 };
            return None;
        }
        self.state = EnrollmentState::Processing;
        Some(std::mem::take(&mut self.accepted))
    }

    pub fn finish_processing(&mut self) {
        if self.state == EnrollmentState::Processing {
            self.state = EnrollmentState::Idle;
        }
    }
}

pub enum SpeakerProfileEvent {
    Changed,
}

/// The result of a speaker check.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerCheck {
    pub verdict: Verdict,
    /// The utterance's embedding when it matched confidently enough to adapt the voiceprint.
    adaptation: Option<Vec<f32>>,
}

impl SpeakerCheck {
    fn unavailable() -> Self {
        Self {
            verdict: Verdict::Unavailable,
            adaptation: None,
        }
    }
}

#[derive(Default)]
pub struct SpeakerProfile {
    voiceprint: Option<Voiceprint>,
    worker: Option<EmbedWorker>,
    flow: EnrollmentFlow,
    /// Counts recordings and enrollments so timers and results from a cancelled one are dropped.
    generation: u64,
    capture: Option<LiveCapture>,
    recording: Arc<Mutex<Vec<f32>>>,
    playback: Option<SpeechOutput>,
}

impl SpeakerProfile {
    pub fn new() -> Self {
        Self {
            voiceprint: load_voiceprint(&voiceprint_path()),
            ..Self::default()
        }
    }

    pub fn enrollment(&self) -> &EnrollmentState {
        self.flow.state()
    }

    /// Whether a voiceprint made with the current speaker model and audio pipeline is stored.
    pub fn is_enrolled(&self) -> bool {
        self.voiceprint
            .as_ref()
            .is_some_and(|print| print.is_current(&campplus_zh_en_advanced().id))
    }

    /// Whether finalized utterances must match the voiceprint.
    pub fn is_verifying(&self, app: &AppContext) -> bool {
        is_speaker_id_enabled()
            && *AISettings::as_ref(app).voice_speaker_id_enabled
            && self.is_enrolled()
            && SpeakerIdModel::as_ref(app).is_installed()
    }

    /// Logs whether speaker checks are active and, if not, which condition is missing.
    pub fn log_verification_status(&self, sensitivity: Sensitivity, app: &AppContext) {
        log::info!(
            "Speaker check: {} (feature available: {}, setting on: {}, voiceprint stored: {}, \
             voiceprint current: {}, model installed: {}, sensitivity {sensitivity:?}, \
             threshold {:.2})",
            if self.is_verifying(app) {
                "active"
            } else {
                "inactive"
            },
            is_speaker_id_enabled(),
            *AISettings::as_ref(app).voice_speaker_id_enabled,
            self.voiceprint.is_some(),
            self.is_enrolled(),
            SpeakerIdModel::as_ref(app).is_installed(),
            sensitivity.threshold(),
        );
    }

    fn worker(&mut self) -> Result<&EmbedWorker, String> {
        if self.worker.is_none() {
            let factory: EmbedderFactory = Box::new(|| {
                OnnxSpeakerEmbedder::load(&speaker_model_store())
                    .map(|embedder| Box::new(embedder) as Box<dyn SpeakerEmbedder>)
                    .map_err(|error| error.to_string())
            });
            self.worker =
                Some(EmbedWorker::spawn(factory, IDLE_UNLOAD).map_err(|e| e.to_string())?);
        }
        Ok(self.worker.as_ref().expect("set above"))
    }

    /// Checks whether the speech in `samples` was spoken by the enrolled user. Silence is trimmed
    /// first, as it is for enrollment: the utterance range carries pre-roll and end-of-speech
    /// hangover that otherwise dominate the embedding and pull the score down. A confident match
    /// may adapt the voiceprint through [`Self::adapt`] when `may_adapt` (never while the
    /// assistant's speech could be in the audio).
    pub fn verify(
        &mut self,
        samples: Vec<f32>,
        sensitivity: Sensitivity,
        may_adapt: bool,
        utterance: u64,
        stage: &'static str,
    ) -> BoxFuture<'static, SpeakerCheck> {
        let audio_secs = samples.len() as f32 / 16_000.0;
        let speech = trim_silence(&samples).to_vec();
        let speech_secs = speech.len() as f32 / 16_000.0;
        let threshold = sensitivity.threshold();
        let Some(voiceprint) = self.voiceprint.clone() else {
            log::info!(
                "Speaker check (utterance {utterance}, {stage}): no voiceprint, let through"
            );
            return Box::pin(async { SpeakerCheck::unavailable() });
        };
        if speech_secs < MIN_VERIFY_SPEECH_SECS {
            log::info!(
                "Speaker check (utterance {utterance}, {stage}): {speech_secs:.2}s of speech in \
                 {audio_secs:.2}s of audio is too short to judge, let through"
            );
            return Box::pin(async { SpeakerCheck::unavailable() });
        }
        let embedding = match self.worker() {
            Ok(worker) => worker.embed(speech),
            Err(error) => Box::pin(async move { Err(error) }),
        };
        Box::pin(async move {
            match embedding.await {
                Ok(embedding) => {
                    let similarity = voiceprint.similarity(&embedding);
                    let verdict = if similarity >= threshold {
                        Verdict::Match
                    } else {
                        Verdict::NoMatch
                    };
                    let adapts = may_adapt && should_adapt(similarity, threshold, speech_secs);
                    log::info!(
                        "Speaker check (utterance {utterance}, {stage}): score {similarity:.3}, \
                         threshold {threshold:.2}, {verdict:?}, {audio_secs:.2}s audio, \
                         {speech_secs:.2}s speech{}",
                        if adapts { ", adapting voiceprint" } else { "" }
                    );
                    SpeakerCheck {
                        verdict,
                        adaptation: adapts.then_some(embedding),
                    }
                }
                Err(error) => {
                    log::warn!(
                        "Speaker check (utterance {utterance}, {stage}) couldn't run: {error}"
                    );
                    SpeakerCheck::unavailable()
                }
            }
        })
    }

    /// Folds a confidently matched utterance from [`Self::verify`] into the voiceprint and saves
    /// it.
    pub fn adapt(&mut self, check: &SpeakerCheck) {
        let (Some(embedding), Some(voiceprint)) = (&check.adaptation, &mut self.voiceprint) else {
            return;
        };
        voiceprint.adapt(embedding);
        if let Err(error) = save_voiceprint(&voiceprint_path(), voiceprint) {
            log::warn!("Couldn't save the adapted voiceprint: {error}");
        }
    }

    pub fn delete_voiceprint(&mut self, ctx: &mut ModelContext<Self>) {
        self.cancel_enrollment(ctx);
        self.voiceprint = None;
        if let Err(error) = delete_voiceprint(&voiceprint_path()) {
            log::error!("Couldn't delete the voiceprint: {error}");
        }
        ctx.emit(SpeakerProfileEvent::Changed);
        ctx.notify();
    }

    /// Starts guided enrollment at the first phrase.
    pub fn start_enrollment(&mut self, ctx: &mut ModelContext<Self>) {
        self.cancel_enrollment(ctx);
        self.flow.start();
        self.changed(ctx);
    }

    /// Records the current phrase through the same capture path hands-free verification taps
    /// (before denoising), so the voiceprint matches the audio it is compared with.
    pub fn record_phrase(&mut self, ctx: &mut ModelContext<Self>) -> Result<(), String> {
        self.stop_playback();
        if !self.flow.record() {
            return Ok(());
        }
        super::apply_system_echo_cancellation_setting(ctx);
        let recording = Arc::new(Mutex::new(Vec::new()));
        let sink = recording.clone();
        let capture = LiveCapture::start(
            move |chunk| {
                if let Ok(mut samples) = sink.lock() {
                    samples.extend(
                        chunk
                            .pcm
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0),
                    );
                }
            },
            |error| log::warn!("Voice enrollment microphone failed: {error}"),
        );
        let capture = match capture {
            Ok(capture) => capture,
            Err(error) => {
                let message = format!("Couldn't open the microphone: {error:#}");
                self.flow.fail(message.clone());
                self.changed(ctx);
                return Err(message);
            }
        };
        self.generation += 1;
        self.capture = Some(capture);
        self.recording = recording;
        let generation = self.generation;
        ctx.spawn(Timer::after(MAX_PHRASE_DURATION), move |me, _, ctx| {
            if me.generation == generation {
                me.stop_recording(ctx);
            }
        });
        self.changed(ctx);
        Ok(())
    }

    /// Ends the recording of the current phrase and checks its quality.
    pub fn stop_recording(&mut self, ctx: &mut ModelContext<Self>) {
        if !matches!(self.flow.state(), EnrollmentState::Recording { .. }) {
            return;
        }
        self.generation += 1;
        self.capture = None;
        let samples = self
            .recording
            .lock()
            .map(|mut samples| std::mem::take(&mut *samples))
            .unwrap_or_default();
        self.flow.finish_recording(&samples);
        if let EnrollmentState::Review { phrase, quality } = self.flow.state() {
            log::info!(
                "Voice enrollment phrase {}: {}, clipped {:.3}%, problem {:?}",
                phrase + 1,
                quality.summary(),
                quality.clipped_fraction * 100.0,
                quality.problem()
            );
        }
        self.changed(ctx);
    }

    /// Plays the phrase under review through the speakers.
    pub fn play_take(&mut self) -> Result<(), String> {
        let Some(take) = self.flow.take() else {
            return Ok(());
        };
        if self.playback.is_none() {
            self.playback = Some(
                SpeechOutput::open()
                    .map_err(|error| format!("Couldn't open the speaker: {error:#}"))?,
            );
        }
        if let Some(output) = &self.playback {
            output.play(take, 16_000, || {});
        }
        Ok(())
    }

    fn stop_playback(&mut self) {
        if let Some(output) = self.playback.take() {
            output.stop();
        }
    }

    /// Accepts the phrase under review; after the last phrase, builds and saves the voiceprint.
    pub fn accept_phrase(&mut self, ctx: &mut ModelContext<Self>) {
        self.stop_playback();
        let phrases = self.flow.accept();
        self.changed(ctx);
        let Some(phrases) = phrases else {
            return;
        };
        let embeddings: Vec<_> = match self.worker() {
            Ok(worker) => phrases.into_iter().map(|p| worker.embed(p)).collect(),
            Err(error) => {
                self.flow.fail(error);
                self.changed(ctx);
                return;
            }
        };
        self.generation += 1;
        let generation = self.generation;
        ctx.spawn(
            async move {
                let mut results = Vec::new();
                for embedding in embeddings {
                    results.push(embedding.await?);
                }
                Voiceprint::enroll(&campplus_zh_en_advanced().id, &results)
                    .map_err(|error| error.to_string())
            },
            move |me, result, ctx| {
                if me.generation != generation {
                    return;
                }
                let saved = result.and_then(|voiceprint| {
                    save_voiceprint(&voiceprint_path(), &voiceprint)
                        .map(|()| voiceprint)
                        .map_err(|error| format!("Couldn't save the voiceprint: {error}"))
                });
                match saved {
                    Ok(voiceprint) => {
                        me.voiceprint = Some(voiceprint);
                        me.flow.finish_processing();
                    }
                    Err(error) => me.flow.fail(error),
                }
                me.changed(ctx);
            },
        );
    }

    /// Discards the enrollment in progress.
    pub fn cancel_enrollment(&mut self, ctx: &mut ModelContext<Self>) {
        self.generation += 1;
        self.capture = None;
        self.stop_playback();
        if *self.flow.state() != EnrollmentState::Idle {
            self.flow.cancel();
            self.changed(ctx);
        }
    }

    fn changed(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.emit(SpeakerProfileEvent::Changed);
        ctx.notify();
    }
}

/// Whether a match is confident and long enough to adapt the voiceprint.
fn should_adapt(similarity: f32, threshold: f32, speech_secs: f32) -> bool {
    similarity >= threshold + ADAPT_MARGIN && speech_secs >= MIN_ADAPT_SPEECH_SECS
}

impl Entity for SpeakerProfile {
    type Event = SpeakerProfileEvent;
}

impl SingletonEntity for SpeakerProfile {}

#[cfg(test)]
#[path = "speaker_id_tests.rs"]
mod tests;
