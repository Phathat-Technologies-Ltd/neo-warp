//! Evaluates end-of-turn detection on Kokoro-synthesized utterances: Smart Turn's classification
//! of complete vs. cut-off utterances, and how fixed silence timeouts and smart end-of-turn split
//! speech with mid-thought pauses.
//!
//! `cargo run --release -p local_transcription --features kokoro,silero,smart_turn --example
//! eot_eval -- <models dir> [features dump path]`. The models dir must hold Kokoro and Silero; Smart
//! Turn downloads into it.

use instant::Instant;
use std::sync::atomic::AtomicBool;

use local_transcription::speech::{KokoroSpeaker, Speaker, VoiceId};
use local_transcription::stream::{ServerMessage, StreamConfig, StreamOutput, UtteranceStream};
use local_transcription::{
    ModelStore, SileroVad, SmartTurnDetector, TurnDetector, VAD_SAMPLE_RATE, ends_mid_thought,
    kokoro_82m_v1_int8, silero_vad_v5, smart_turn_features, smart_turn_v3_2,
};

/// (prefix that ends mid-thought, continuation completing it).
const PAIRS: &[(&str, &str)] = &[
    ("List the files in", "the source directory."),
    ("Can you check whether the", "tests pass on main?"),
    (
        "I want to refactor the parser so that",
        "it handles comments.",
    ),
    ("Run cargo test for the", "local transcription crate."),
    ("Open the file called", "main dot rust."),
    ("Show me the last", "five commits."),
    ("Delete the branch named", "feature login."),
    ("What does this function", "return when the input is empty?"),
    ("Install the dependencies and then", "start the dev server."),
    ("Find all the places where we", "call the logger."),
    ("Rename the variable from count to", "total."),
    ("Why is the build failing on", "Linux?"),
    ("Summarize the changes in", "this pull request."),
    ("Add a unit test for", "the date parser."),
    ("Move the config loading into", "its own module."),
    ("Explain how the cache works and", "when it gets cleared."),
    ("Check the logs from", "yesterday's deploy."),
    ("Create a new directory called", "scripts."),
    (
        "Kill the process that is listening on",
        "port eight thousand.",
    ),
    ("Make the button blue and", "a bit larger."),
    ("Search the codebase for", "uses of unwrap."),
    (
        "Commit these changes with the message",
        "fix the login bug.",
    ),
    ("I think the problem is that the", "timeout is too short."),
    ("Compare this branch with", "master."),
    ("Update the readme to mention", "the new flag."),
    ("Stop the server, um,", "and restart it."),
    ("Tell me what the", "error means."),
    ("Write a script that", "backs up the database."),
    ("Go to the top of", "the file."),
    ("Is there a way to", "speed up the build?"),
];

const VOICES: &[&str] = &["af_heart", "am_michael", "af_bella", "am_adam"];
const PAUSES_MS: &[u32] = &[500, 800, 1200];

fn resample_24k_to_16k(input: &[f32]) -> Vec<f32> {
    const TAPS: isize = 32;
    let cutoff = 7_000.0 / 24_000.0;
    let out_len = input.len() * 2 / 3;
    (0..out_len)
        .map(|n| {
            let center = n as f64 * 1.5;
            let base = center.floor() as isize;
            let mut acc = 0.0;
            for k in (base - TAPS)..=(base + TAPS) {
                if k < 0 || k as usize >= input.len() {
                    continue;
                }
                let x = k as f64 - center;
                let sinc = if x == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let window = 0.5 + 0.5 * (std::f64::consts::PI * x / (TAPS as f64 + 1.0)).cos();
                acc += f64::from(input[k as usize]) * sinc * window;
            }
            acc as f32
        })
        .collect()
}

fn silence(ms: u32) -> Vec<f32> {
    vec![0.0; (ms * VAD_SAMPLE_RATE / 1000) as usize]
}

fn trim(samples: &[f32]) -> Vec<f32> {
    let start = samples.iter().position(|s| s.abs() > 0.01).unwrap_or(0);
    let end = samples
        .iter()
        .rposition(|s| s.abs() > 0.01)
        .map_or(0, |i| i + 1);
    samples[start..end.max(start)].to_vec()
}

fn rss_kb() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug)]
enum Policy {
    Fixed(u32),
    Smart,
    /// Smart Turn plus the transcript guard, given the exact text when the speech ends.
    SmartText,
}

struct EndpointResult {
    /// SpeechEnd times in ms from the start of the audio.
    ends: Vec<u64>,
}

fn run_stream(
    models: &std::path::Path,
    policy: Policy,
    audio: &[f32],
    detector: Option<Box<dyn TurnDetector>>,
    transcript: Option<(&str, usize)>,
) -> EndpointResult {
    let vad = SileroVad::load(&ModelStore::new(models, silero_vad_v5())).expect("silero loads");
    let mut stream = UtteranceStream::new(Box::new(vad), StreamConfig::default());
    match policy {
        Policy::Fixed(ms) => stream.set_min_silence_ms(ms),
        Policy::Smart | Policy::SmartText => {
            stream = stream.with_turn_detector(detector.expect("smart needs a detector"));
        }
    }
    let mut ends = Vec::new();
    for (index, chunk) in audio.chunks(512).enumerate() {
        let outputs = stream.push_samples(chunk).expect("stream runs");
        if let Some((text, at)) = transcript
            && (index * 512..(index + 1) * 512).contains(&at)
        {
            stream.note_partial(1, text);
        }
        if outputs
            .iter()
            .any(|o| matches!(o, StreamOutput::Message(ServerMessage::SpeechEnd)))
        {
            ends.push(((index + 1) * 512) as u64 * 1000 / u64::from(VAD_SAMPLE_RATE));
        }
    }
    EndpointResult { ends }
}

struct Counts {
    tp: u32,
    fp: u32,
    tn: u32,
    fn_: u32,
}

impl Counts {
    fn add(&mut self, predicted_complete: bool, complete: bool) {
        match (predicted_complete, complete) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, false) => self.tn += 1,
            (false, true) => self.fn_ += 1,
        }
    }

    fn report(&self, name: &str) {
        let total = (self.tp + self.fp + self.tn + self.fn_) as f64;
        let precision = self.tp as f64 / (self.tp + self.fp).max(1) as f64;
        let recall = self.tp as f64 / (self.tp + self.fn_).max(1) as f64;
        println!(
            "{name}: n={total} acc={:.1}% precision={precision:.3} recall={recall:.3} \
             (tp {} fp {} tn {} fn {})",
            100.0 * (self.tp + self.tn) as f64 / total,
            self.tp,
            self.fp,
            self.tn,
            self.fn_
        );
    }
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let models = std::path::PathBuf::from(args.next().expect("models dir"));
    let dump = args.next();

    let turn_store = ModelStore::new(&models, smart_turn_v3_2());
    if !turn_store.is_installed() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime
            .block_on(turn_store.download(&reqwest::Client::new(), &AtomicBool::new(false), |_| {}))
            .expect("smart turn downloads");
    }
    let speaker = KokoroSpeaker::new(ModelStore::new(&models, kokoro_82m_v1_int8()));
    let synth = |text: &str, voice: &str| {
        let audio = speaker
            .synthesize(text, &VoiceId::new(voice), 1.0)
            .expect("kokoro synthesizes");
        trim(&resample_24k_to_16k(&audio.samples))
    };

    if std::env::var_os("EOT_RSS_ONLY").is_some() {
        // ONNX Runtime itself is already loaded in the app for Silero and Parakeet.
        let _vad = SileroVad::load(&ModelStore::new(&models, silero_vad_v5())).unwrap();
        let before = rss_kb();
        let mut detector = SmartTurnDetector::load(&turn_store).expect("smart turn loads");
        let audio = silence(3000);
        let cpu = Instant::now();
        for _ in 0..50 {
            detector.completion_probability(&audio).unwrap();
        }
        println!(
            "Smart Turn alone: RSS +{} MB after load + 50 runs, {:.1} ms per run",
            rss_kb().saturating_sub(before) / 1024,
            cpu.elapsed().as_secs_f64() * 1000.0 / 50.0
        );
        return;
    }
    let rss_before = rss_kb();
    let load_start = Instant::now();
    let mut detector = SmartTurnDetector::load(&turn_store).expect("smart turn loads");
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    if let Some(dump) = dump {
        let audio = synth("List the files in", "af_heart");
        let mut input = silence(300);
        input.extend(&audio);
        input.extend(silence(200));
        let features = smart_turn_features(&input);
        let bytes: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(format!("{dump}.audio.f32"), bytes).unwrap();
        let bytes: Vec<u8> = features.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(format!("{dump}.features.f32"), bytes).unwrap();
        println!(
            "dumped; rust probability {}",
            detector.completion_probability(&input).unwrap()
        );
    }

    let mut audio_counts = Counts {
        tp: 0,
        fp: 0,
        tn: 0,
        fn_: 0,
    };
    let mut text_counts = Counts {
        tp: 0,
        fp: 0,
        tn: 0,
        fn_: 0,
    };
    let mut hybrid_counts = Counts {
        tp: 0,
        fp: 0,
        tn: 0,
        fn_: 0,
    };
    let mut latencies = Vec::new();
    let mut cpu_ms = 0.0;
    let mut utterances = Vec::new();
    for &voice in VOICES {
        for &(prefix, rest) in PAIRS {
            let full_text = format!("{prefix} {rest}");
            let full = synth(&full_text, voice);
            let partial = synth(prefix, voice);
            let continuation = synth(rest, voice);
            for (text, audio, complete) in [
                (&full_text, &full, true),
                (&prefix.to_owned(), &partial, false),
            ] {
                let mut input = silence(300);
                input.extend(audio.iter());
                input.extend(silence(200));
                let start = Instant::now();
                let probability = detector.completion_probability(&input).unwrap();
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                cpu_ms += ms;
                latencies.push(ms);
                audio_counts.add(probability >= 0.5, complete);
                let text_incomplete = ends_mid_thought(text);
                text_counts.add(!text_incomplete, complete);
                hybrid_counts.add(probability >= 0.5 && !text_incomplete, complete);
            }
            utterances.push((full, partial, continuation));
        }
    }
    let rss_after = rss_kb();
    println!("== Classification (Smart Turn v3.2 on audio + 200 ms pause) ==");
    audio_counts.report("smart-turn audio");
    text_counts.report("text guard alone (exact text, ends_mid_thought)");
    hybrid_counts.report("smart-turn audio + text guard");
    let mean = latencies.iter().sum::<f64>() / latencies.len() as f64;
    println!(
        "inference latency: mean {mean:.1} ms p50 {:.1} ms p95 {:.1} ms max {:.1} ms; load {load_ms:.0} ms",
        percentile(&mut latencies, 0.5),
        percentile(&mut latencies, 0.95),
        percentile(&mut latencies, 1.0)
    );
    println!(
        "RSS growth from loading + running Smart Turn: {} MB (total cpu {cpu_ms:.0} ms)",
        rss_after.saturating_sub(rss_before) / 1024
    );

    println!("\n== Endpointing through Silero + UtteranceStream ==");
    let policies = if std::env::var_os("EOT_SMART_ONLY").is_some() {
        vec![Policy::Smart, Policy::SmartText]
    } else {
        vec![
            Policy::Fixed(300),
            Policy::Fixed(800),
            Policy::Fixed(1500),
            Policy::Smart,
            Policy::SmartText,
        ]
    };
    for policy in policies {
        let mut end_delays = Vec::new();
        let mut missed = 0;
        let mut cut = vec![0u32; PAUSES_MS.len()];
        let mut paused_total = 0u32;
        for ((full, partial, continuation), (prefix, rest)) in
            utterances.iter().zip(PAIRS.iter().cycle())
        {
            let full_text = format!("{prefix} {rest}");
            let mut audio = silence(500);
            audio.extend(full.iter());
            let speech_end_ms = audio.len() as u64 * 1000 / u64::from(VAD_SAMPLE_RATE);
            audio.extend(silence(3000));
            let detector = matches!(policy, Policy::Smart | Policy::SmartText).then(|| {
                Box::new(SmartTurnDetector::load(&turn_store).unwrap()) as Box<dyn TurnDetector>
            });
            let speech_end = full.len() + silence(500).len() - 1;
            let transcript =
                matches!(policy, Policy::SmartText).then_some((full_text.as_str(), speech_end));
            let result = run_stream(&models, policy, &audio, detector, transcript);
            match result.ends.first() {
                Some(&end) => end_delays.push(end.saturating_sub(speech_end_ms) as f64),
                None => missed += 1,
            }
            paused_total += 1;
            for (index, &pause) in PAUSES_MS.iter().enumerate() {
                let mut audio = silence(500);
                audio.extend(partial.iter());
                let pause_start = audio.len() as u64 * 1000 / u64::from(VAD_SAMPLE_RATE);
                audio.extend(silence(pause));
                audio.extend(continuation.iter());
                audio.extend(silence(3000));
                let detector = matches!(policy, Policy::Smart | Policy::SmartText).then(|| {
                    Box::new(SmartTurnDetector::load(&turn_store).unwrap()) as Box<dyn TurnDetector>
                });
                let at = silence(500).len() + partial.len() - 1;
                let transcript = matches!(policy, Policy::SmartText).then_some((*prefix, at));
                let result = run_stream(&models, policy, &audio, detector, transcript);
                if result
                    .ends
                    .iter()
                    .any(|&end| end > pause_start && end <= pause_start + u64::from(pause) + 64)
                {
                    cut[index] += 1;
                }
            }
        }
        let mean = end_delays.iter().sum::<f64>() / end_delays.len().max(1) as f64;
        let p50 = percentile(&mut end_delays, 0.5);
        let p90 = percentile(&mut end_delays, 0.9);
        let cuts = PAUSES_MS
            .iter()
            .zip(&cut)
            .map(|(pause, cut)| {
                format!(
                    "{pause}ms: {:.0}%",
                    100.0 * f64::from(*cut) / f64::from(paused_total)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{policy:?}: end-of-turn delay after complete utterances mean {mean:.0} ms p50 {p50:.0} \
             ms p90 {p90:.0} ms (missed {missed}); cut off at mid-thought pauses {cuts}"
        );
    }
}
