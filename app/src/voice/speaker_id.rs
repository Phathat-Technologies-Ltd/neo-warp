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
use local_transcription::{OnnxSpeakerEmbedder, campplus_en_voxceleb};
use voice_input::LiveCapture;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::local_model::{SpeakerIdModel, is_speaker_id_enabled, speaker_model_store};
use super::speaker_gate::Verdict;
use crate::settings::AISettings;

/// How long the loaded model stays in memory after its last use.
const IDLE_UNLOAD: Duration = Duration::from_secs(180);
/// Recording time per enrollment phrase.
const PHRASE_DURATION: Duration = Duration::from_millis(5500);
/// RMS below which a 10 ms frame counts as silence when trimming enrollment audio.
const SILENCE_RMS: f32 = 0.01;
/// Speech needed per enrollment phrase after trimming silence.
const MIN_PHRASE_SAMPLES: usize = 16_000;

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EnrollmentState {
    #[default]
    Idle,
    /// Recording the user reading `ENROLLMENT_PHRASES[phrase]`.
    Recording {
        phrase: usize,
    },
    Processing,
    Failed(String),
}

pub enum SpeakerProfileEvent {
    Changed,
}

#[derive(Default)]
pub struct SpeakerProfile {
    voiceprint: Option<Voiceprint>,
    worker: Option<EmbedWorker>,
    enrollment: EnrollmentState,
    /// Counts enrollments so timers and results from a cancelled one are dropped.
    generation: u64,
    capture: Option<LiveCapture>,
    recording: Arc<Mutex<Vec<f32>>>,
    phrases: Vec<Vec<f32>>,
}

impl SpeakerProfile {
    pub fn new() -> Self {
        Self {
            voiceprint: load_voiceprint(&voiceprint_path()),
            ..Self::default()
        }
    }

    pub fn enrollment(&self) -> &EnrollmentState {
        &self.enrollment
    }

    /// Whether a voiceprint made with the current speaker model is stored.
    pub fn is_enrolled(&self) -> bool {
        self.voiceprint
            .as_ref()
            .is_some_and(|print| print.model_id == campplus_en_voxceleb().id)
    }

    /// Whether finalized utterances must match the voiceprint.
    pub fn is_verifying(&self, app: &AppContext) -> bool {
        is_speaker_id_enabled()
            && *AISettings::as_ref(app).voice_speaker_id_enabled
            && self.is_enrolled()
            && SpeakerIdModel::as_ref(app).is_installed()
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

    /// Checks whether `samples` were spoken by the enrolled user.
    pub fn verify(
        &mut self,
        samples: Vec<f32>,
        sensitivity: Sensitivity,
    ) -> BoxFuture<'static, Verdict> {
        let Some(voiceprint) = self.voiceprint.clone() else {
            return Box::pin(async { Verdict::Unavailable });
        };
        let embedding = match self.worker() {
            Ok(worker) => worker.embed(samples),
            Err(error) => Box::pin(async move { Err(error) }),
        };
        Box::pin(async move {
            match embedding.await {
                Ok(embedding) => {
                    let similarity = voiceprint.similarity(&embedding);
                    log::debug!("Speaker similarity {similarity:.3}");
                    if similarity >= sensitivity.threshold() {
                        Verdict::Match
                    } else {
                        Verdict::NoMatch
                    }
                }
                Err(error) => {
                    log::warn!("Couldn't check the speaker: {error}");
                    Verdict::Unavailable
                }
            }
        })
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

    /// Starts guided enrollment: the user reads each phrase while the microphone records.
    pub fn start_enrollment(&mut self, ctx: &mut ModelContext<Self>) -> Result<(), String> {
        self.cancel_enrollment(ctx);
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
        )
        .map_err(|error| format!("Couldn't open the microphone: {error:#}"))?;
        self.capture = Some(capture);
        self.recording = recording;
        self.phrases.clear();
        self.set_enrollment(EnrollmentState::Recording { phrase: 0 }, ctx);
        self.schedule_phrase_end(ctx);
        Ok(())
    }

    pub fn cancel_enrollment(&mut self, ctx: &mut ModelContext<Self>) {
        self.generation += 1;
        self.capture = None;
        self.phrases.clear();
        if self.enrollment != EnrollmentState::Idle {
            self.set_enrollment(EnrollmentState::Idle, ctx);
        }
    }

    fn set_enrollment(&mut self, state: EnrollmentState, ctx: &mut ModelContext<Self>) {
        self.enrollment = state;
        ctx.emit(SpeakerProfileEvent::Changed);
        ctx.notify();
    }

    fn fail_enrollment(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        self.generation += 1;
        self.capture = None;
        self.phrases.clear();
        self.set_enrollment(EnrollmentState::Failed(message), ctx);
    }

    fn schedule_phrase_end(&mut self, ctx: &mut ModelContext<Self>) {
        let generation = self.generation;
        ctx.spawn(Timer::after(PHRASE_DURATION), move |me, _, ctx| {
            if me.generation == generation {
                me.finish_phrase(ctx);
            }
        });
    }

    fn finish_phrase(&mut self, ctx: &mut ModelContext<Self>) {
        let EnrollmentState::Recording { phrase } = self.enrollment else {
            return;
        };
        let samples = self
            .recording
            .lock()
            .map(|mut samples| std::mem::take(&mut *samples))
            .unwrap_or_default();
        let speech = trim_silence(&samples);
        if speech.len() < MIN_PHRASE_SAMPLES {
            self.fail_enrollment(
                "Didn't hear enough speech. Read each phrase aloud, then try again.".to_owned(),
                ctx,
            );
            return;
        }
        self.phrases.push(speech.to_vec());
        if phrase + 1 < ENROLLMENT_PHRASES.len() {
            self.set_enrollment(EnrollmentState::Recording { phrase: phrase + 1 }, ctx);
            self.schedule_phrase_end(ctx);
            return;
        }
        self.capture = None;
        self.set_enrollment(EnrollmentState::Processing, ctx);
        let phrases = std::mem::take(&mut self.phrases);
        let embeddings: Vec<_> = match self.worker() {
            Ok(worker) => phrases.into_iter().map(|p| worker.embed(p)).collect(),
            Err(error) => {
                self.fail_enrollment(error, ctx);
                return;
            }
        };
        let generation = self.generation;
        ctx.spawn(
            async move {
                let mut results = Vec::new();
                for embedding in embeddings {
                    results.push(embedding.await?);
                }
                Voiceprint::enroll(&campplus_en_voxceleb().id, &results)
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
                        me.set_enrollment(EnrollmentState::Idle, ctx);
                    }
                    Err(error) => me.fail_enrollment(error, ctx),
                }
            },
        );
    }
}

impl Entity for SpeakerProfile {
    type Event = SpeakerProfileEvent;
}

impl SingletonEntity for SpeakerProfile {}

#[cfg(test)]
#[path = "speaker_id_tests.rs"]
mod tests;
