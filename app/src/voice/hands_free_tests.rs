use std::time::Duration;

use local_transcription::{
    Audio, TranscribeError, Transcriber, VAD_FRAME_SAMPLES, VadFactory, VoiceActivityDetector,
};
use selfhost_server::EmbeddedConfig;
use selfhost_server::sessions::SessionStore;

use super::*;
use crate::server::server_api::connect_transcribe_stream;

const ISSUER: &str = "https://issuer.example";
const SECRET: &[u8] = b"hands-free-test-signing-secret-0123456789";

/// Treats a frame as speech when it is loud.
struct EnergyVad;

impl VoiceActivityDetector for EnergyVad {
    fn speech_probability(&mut self, frame: &[f32]) -> Result<f32, TranscribeError> {
        let energy = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        Ok(if energy.sqrt() > 0.05 { 0.95 } else { 0.0 })
    }

    fn reset(&mut self) {}
}

/// Answers with the utterance length.
struct FakeTranscriber;

impl Transcriber for FakeTranscriber {
    fn transcribe(&self, audio: &Audio, _: Option<&str>) -> Result<String, TranscribeError> {
        Ok(format!(
            "{} frames",
            audio.samples.len() / VAD_FRAME_SAMPLES
        ))
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// 10 ms chunks of 16 kHz PCM like [`LiveCapture`] delivers: a tone while `speaking`, else silence.
fn chunks(ms: usize, speaking: bool) -> impl Iterator<Item = LiveChunk> {
    (0..ms / 10).map(move |chunk| {
        let pcm = (0..160)
            .flat_map(|i| {
                let t = (chunk * 160 + i) as f32 / 16_000.0;
                let sample = if speaking {
                    (t * 220.0 * std::f32::consts::TAU).sin() * 0.4
                } else {
                    0.0
                };
                ((sample * 32767.0) as i16).to_le_bytes()
            })
            .collect();
        LiveChunk {
            pcm,
            level: if speaking { 0.28 } else { 0.0 },
        }
    })
}

async fn signed_in_token() -> String {
    let sessions = SessionStore::new(Some(SECRET), Duration::from_secs(3600));
    let custom = sessions.sign_in(ISSUER, "subject", None, None);
    sessions
        .redeem_custom_token(&custom)
        .await
        .unwrap()
        .id_token
}

/// Speech from a fake microphone goes through the embedded server's `/ai/transcribe/stream` and
/// comes back as partial and final edits of the input.
#[tokio::test(flavor = "multi_thread")]
async fn speech_reaches_the_input_through_the_embedded_server() {
    let vad: VadFactory = Arc::new(|| Ok(Box::new(EnergyVad) as Box<dyn VoiceActivityDetector>));
    let address = selfhost_server::spawn_embedded(EmbeddedConfig {
        issuer: ISSUER.to_owned(),
        client_id: "client".to_owned(),
        signing_secret: SECRET.to_vec(),
        session_max_age: Duration::from_secs(3600),
        remote_server_artifacts: None,
        transcriber: Some(Arc::new(FakeTranscriber)),
        vad: Some(vad),
        turn_detector: None,
        codebase_index_dir: None,
        recording_store: None,
    })
    .unwrap();
    let socket =
        connect_transcribe_stream(&format!("http://{address}"), Some(&signed_in_token().await))
            .await
            .unwrap();

    let (audio_tx, audio_rx) = mpsc::unbounded();
    let (update_tx, mut update_rx) = mpsc::unbounded();
    tokio::spawn(stream_session(
        socket,
        stream_config(None, true),
        audio_rx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(Some(vec![BiasTerm::new("SAST-AI")]))),
        update_tx,
    ));
    for chunk in chunks(300, false)
        .chain(chunks(1500, true))
        .chain(chunks(1000, false))
    {
        audio_tx.unbounded_send(chunk).unwrap();
    }

    let mut controller = HandsFreeController::default();
    controller.start(HandsFreeOptions::default());
    let mut effects = Vec::new();
    let mut states = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !effects
            .iter()
            .any(|effect| matches!(effect, HandsFreeEffect::Commit(_)))
        {
            match update_rx.next().await.expect("the session stays open") {
                SocketUpdate::Message(message) => {
                    effects.extend(controller.on_message(message, Instant::now()));
                    states.push(controller.state());
                }
                SocketUpdate::Closed(error) => panic!("the socket closed: {error:?}"),
            }
        }
    })
    .await
    .expect("a final transcript arrives");

    assert!(states.contains(&HandsFreeState::Hearing));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, HandsFreeEffect::ShowPartial(_)))
    );
    let Some(HandsFreeEffect::Commit(text)) = effects.last() else {
        panic!("the last effect is the commit: {effects:?}");
    };
    assert!(text.ends_with("frames"), "{text}");

    drop(audio_tx);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(10), update_rx.next())
            .await
            .unwrap(),
        Some(SocketUpdate::Closed(None))
    ));
}

#[test]
fn stream_config_carries_the_smart_end_of_turn_setting() {
    for enabled in [true, false] {
        let ClientMessage::Config { vad, .. } = stream_config(None, enabled) else {
            panic!("expected a config message");
        };
        let vad = vad.expect("vad options");
        assert_eq!(vad.smart_end_of_turn, Some(enabled));
        assert_eq!(vad.min_silence_ms, Some(300));
    }
}
