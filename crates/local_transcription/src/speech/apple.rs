use std::cmp::Reverse;

/// How natural a macOS system voice sounds, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AppleVoiceQuality {
    Default,
    Enhanced,
    Premium,
}

impl AppleVoiceQuality {
    pub fn label(self) -> &'static str {
        match self {
            AppleVoiceQuality::Default => "Default",
            AppleVoiceQuality::Enhanced => "Enhanced",
            AppleVoiceQuality::Premium => "Premium",
        }
    }
}

/// A macOS system voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleVoice {
    pub identifier: String,
    pub name: String,
    /// BCP 47 code such as `en-US`.
    pub language: String,
    pub quality: AppleVoiceQuality,
    /// A Personal Voice the user recorded on this device.
    pub personal: bool,
}

/// Identifier markers of the legacy character and Eloquence voices (Albert, Bubbles, Eddy, ...),
/// which sound robotic next to the stock voices of the same quality.
const NOVELTY_MARKERS: &[&str] = &["com.apple.speech.synthesis.voice.", ".eloquence."];

impl AppleVoice {
    fn is_novelty(&self) -> bool {
        NOVELTY_MARKERS
            .iter()
            .any(|marker| self.identifier.contains(marker))
    }
}

impl AppleVoice {
    pub fn display_name(&self) -> String {
        let kind = if self.personal {
            "Personal Voice"
        } else {
            self.quality.label()
        };
        format!("{} ({kind}, {})", self.name, self.language)
    }
}

/// The voices that speak `language` (an ISO 639-1 code such as `en`), best first: by quality,
/// then the regular voices before the novelty ones and the user's own Personal Voices, then by
/// name.
pub fn rank_apple_voices(voices: Vec<AppleVoice>, language: &str) -> Vec<AppleVoice> {
    let mut voices: Vec<AppleVoice> = voices
        .into_iter()
        .filter(|voice| {
            voice
                .language
                .split(['-', '_'])
                .next()
                .is_some_and(|code| code.eq_ignore_ascii_case(language))
        })
        .collect();
    voices.sort_by(|a, b| {
        (
            Reverse(a.quality),
            a.is_novelty(),
            a.personal,
            &a.name,
            &a.language,
        )
            .cmp(&(
                Reverse(b.quality),
                b.is_novelty(),
                b.personal,
                &b.name,
                &b.language,
            ))
    });
    voices
}

#[cfg(target_os = "macos")]
pub use macos::{
    APPLE_PERSONAL_VOICE_REQUEST, AppleSpeaker, apple_voices, request_personal_voice_authorization,
};

#[cfg(target_os = "macos")]
mod macos {
    use std::ptr::NonNull;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_avf_audio::{
        AVAudioBuffer, AVAudioPCMBuffer, AVSpeechSynthesisPersonalVoiceAuthorizationStatus,
        AVSpeechSynthesisVoice, AVSpeechSynthesisVoiceQuality, AVSpeechSynthesisVoiceTraits,
        AVSpeechSynthesizer, AVSpeechUtterance, AVSpeechUtteranceDefaultSpeechRate,
        AVSpeechUtteranceMaximumSpeechRate,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop, NSString};

    use super::{AppleVoice, AppleVoiceQuality, rank_apple_voices};
    use crate::Audio;
    use crate::speech::{SpeakError, Speaker, VoiceId, VoiceInfo};

    /// The voice id that stands for "use my Personal Voice" before the user has allowed it; picking
    /// it asks for authorization.
    pub const APPLE_PERSONAL_VOICE_REQUEST: &str = "personal-voice-request";

    /// How long to wait for the next buffer before giving up on a sentence.
    const BUFFER_TIMEOUT: Duration = Duration::from_secs(20);

    fn personal_voice_status() -> AVSpeechSynthesisPersonalVoiceAuthorizationStatus {
        unsafe { AVSpeechSynthesizer::personalVoiceAuthorizationStatus() }
    }

    fn to_voice(voice: &AVSpeechSynthesisVoice) -> AppleVoice {
        unsafe {
            let quality = voice.quality();
            AppleVoice {
                identifier: voice.identifier().to_string(),
                name: voice.name().to_string(),
                language: voice.language().to_string(),
                quality: if quality == AVSpeechSynthesisVoiceQuality::Premium {
                    AppleVoiceQuality::Premium
                } else if quality == AVSpeechSynthesisVoiceQuality::Enhanced {
                    AppleVoiceQuality::Enhanced
                } else {
                    AppleVoiceQuality::Default
                },
                personal: voice
                    .voiceTraits()
                    .contains(AVSpeechSynthesisVoiceTraits::IsPersonalVoice),
            }
        }
    }

    /// The installed voices for `language`, best first.
    pub fn apple_voices(language: &str) -> Vec<AppleVoice> {
        let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
        rank_apple_voices(
            (0..voices.count())
                .map(|i| to_voice(&voices.objectAtIndex(i)))
                .collect(),
            language,
        )
    }

    /// Asks the user to allow Personal Voices, calling `done` with whether they're allowed.
    pub fn request_personal_voice_authorization(done: impl Fn(bool) + 'static) {
        let handler = RcBlock::new(
            move |status: AVSpeechSynthesisPersonalVoiceAuthorizationStatus| {
                done(status == AVSpeechSynthesisPersonalVoiceAuthorizationStatus::Authorized);
            },
        );
        unsafe {
            AVSpeechSynthesizer::requestPersonalVoiceAuthorizationWithCompletionHandler(&handler)
        };
    }

    /// The macOS system voices through `AVSpeechSynthesizer`, rendered to PCM buffers rather than
    /// played, so speech goes through the app's own output path.
    pub struct AppleSpeaker {
        language: String,
    }

    impl AppleSpeaker {
        /// A speaker for the voices of `language`, an ISO 639-1 code such as `en`.
        pub fn new(language: impl Into<String>) -> Self {
            Self {
                language: language.into(),
            }
        }
    }

    /// The first channel of `buffer` as f32 samples, with its sample rate.
    fn read_buffer(buffer: &AVAudioBuffer) -> Option<(Vec<f32>, u32)> {
        let object: &AnyObject = buffer.as_ref();
        let pcm = object.downcast_ref::<AVAudioPCMBuffer>()?;
        unsafe {
            let format = pcm.format();
            let frames = pcm.frameLength() as usize;
            let rate = format.sampleRate() as u32;
            let channels = format.channelCount().max(1) as usize;
            let stride = if format.isInterleaved() { channels } else { 1 };
            let floats = pcm.floatChannelData();
            if !floats.is_null() {
                let data = (*floats).as_ptr();
                return Some(((0..frames).map(|i| *data.add(i * stride)).collect(), rate));
            }
            let ints = pcm.int16ChannelData();
            if !ints.is_null() {
                let data = (*ints).as_ptr();
                return Some((
                    (0..frames)
                        .map(|i| f32::from(*data.add(i * stride)) / 32768.0)
                        .collect(),
                    rate,
                ));
            }
            let ints = pcm.int32ChannelData();
            if !ints.is_null() {
                let data = (*ints).as_ptr();
                return Some((
                    (0..frames)
                        .map(|i| *data.add(i * stride) as f32 / 2_147_483_648.0)
                        .collect(),
                    rate,
                ));
            }
            None
        }
    }

    /// Waits for the next buffer, running this thread's run loop meanwhile. The synthesizer
    /// delivers buffers through the main run loop, so off the main thread this only works while
    /// the main thread runs its loop, as it does in the app.
    fn next_buffer<T>(rx: &mpsc::Receiver<T>) -> Result<T, mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + BUFFER_TIMEOUT;
        let run_loop = NSRunLoop::currentRunLoop();
        loop {
            match rx.try_recv() {
                Ok(value) => return Ok(value),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(mpsc::RecvTimeoutError::Disconnected);
                }
                Err(mpsc::TryRecvError::Empty) if Instant::now() >= deadline => {
                    return Err(mpsc::RecvTimeoutError::Timeout);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
                    run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &until);
                }
            }
        }
    }

    impl Speaker for AppleSpeaker {
        fn synthesize(&self, text: &str, voice: &VoiceId, rate: f32) -> Result<Audio, SpeakError> {
            if text.trim().is_empty() {
                return Ok(Audio {
                    samples: Vec::new(),
                    sample_rate: 22_050,
                });
            }
            let (tx, rx) = mpsc::channel::<Option<(Vec<f32>, u32)>>();
            let callback = RcBlock::new(move |buffer: NonNull<AVAudioBuffer>| {
                let read = read_buffer(unsafe { buffer.as_ref() });
                let _ = tx.send(read);
            });
            let synthesizer = unsafe { AVSpeechSynthesizer::new() };
            unsafe {
                let utterance =
                    AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(text));
                let chosen: Option<Retained<AVSpeechSynthesisVoice>> =
                    AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(
                        voice.as_str(),
                    ))
                    .or_else(|| {
                        let best = apple_voices(&self.language).into_iter().next()?;
                        AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(
                            &best.identifier,
                        ))
                    });
                utterance.setVoice(chosen.as_deref());
                // The synthesizer's rate scale is steeper than linear around its default, so
                // only half of the requested change is applied.
                utterance.setRate(
                    (AVSpeechUtteranceDefaultSpeechRate * (1.0 + (rate - 1.0) / 2.0))
                        .min(AVSpeechUtteranceMaximumSpeechRate),
                );
                synthesizer.writeUtterance_toBufferCallback(
                    &utterance,
                    RcBlock::as_ptr(&callback) as *mut _,
                );
            }
            let mut samples = Vec::new();
            let mut sample_rate = 0;
            loop {
                match next_buffer(&rx) {
                    Ok(Some((chunk, rate))) => {
                        // The synthesizer ends an utterance with an empty buffer.
                        if chunk.is_empty() {
                            break;
                        }
                        sample_rate = rate;
                        samples.extend(chunk);
                    }
                    Ok(None) => {
                        return Err(SpeakError::Engine(
                            "unsupported audio buffer format".to_owned(),
                        ));
                    }
                    Err(_) if !samples.is_empty() => break,
                    Err(error) => {
                        return Err(SpeakError::Engine(format!(
                            "the system synthesizer produced no audio: {error}"
                        )));
                    }
                }
            }
            drop(synthesizer);
            if samples.is_empty() || sample_rate == 0 {
                return Err(SpeakError::Engine(
                    "the system synthesizer produced no audio".to_owned(),
                ));
            }
            Ok(Audio {
                samples,
                sample_rate,
            })
        }

        fn is_ready(&self) -> bool {
            true
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            let mut voices: Vec<VoiceInfo> = apple_voices(&self.language)
                .into_iter()
                .map(|voice| VoiceInfo {
                    display_name: voice.display_name(),
                    id: VoiceId::new(voice.identifier),
                })
                .collect();
            if personal_voice_status()
                == AVSpeechSynthesisPersonalVoiceAuthorizationStatus::NotDetermined
            {
                voices.push(VoiceInfo {
                    id: VoiceId::new(APPLE_PERSONAL_VOICE_REQUEST),
                    display_name: "Personal Voice (asks for permission)".to_owned(),
                });
            }
            voices
        }
    }
}

#[cfg(test)]
#[path = "apple_tests.rs"]
mod tests;
