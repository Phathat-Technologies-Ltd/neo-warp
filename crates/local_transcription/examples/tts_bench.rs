//! Times a speech engine on replies read from a file, one per line, and writes each reply's audio
//! as a WAV, downloading the engine's model first if needed. Usage: `tts_bench <kokoro|pocket|tiny_voca> <models dir> <texts file> <out dir>`.

use instant::Instant;
use std::sync::atomic::AtomicBool;

use local_transcription::speech::chunker::{ChunkerOptions, SentenceChunker};
use local_transcription::speech::{KokoroSpeaker, PocketSpeaker, Speaker, TinyVocaSpeaker};
use local_transcription::{ModelStore, kokoro_82m_v1_int8, pocket_tts_int8, tiny_voca};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, engine, models, texts, out] = args.as_slice() else {
        panic!("usage: tts_bench <kokoro|pocket|tiny_voca> <models dir> <texts file> <out dir>");
    };
    let spec = match engine.as_str() {
        "kokoro" => kokoro_82m_v1_int8(),
        "pocket" => pocket_tts_int8(),
        "tiny_voca" => tiny_voca(),
        other => panic!("unknown engine {other}"),
    };
    let store = ModelStore::new(models, spec);
    if !store.is_installed() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let started = Instant::now();
        runtime
            .block_on(store.download(&reqwest::Client::new(), &AtomicBool::new(false), |_| {}))
            .unwrap();
        println!("downloaded in {:.1} s", started.elapsed().as_secs_f64());
    }
    let speaker: Box<dyn Speaker> = match engine.as_str() {
        "kokoro" => Box::new(KokoroSpeaker::new(store)),
        "pocket" => Box::new(PocketSpeaker::new(store)),
        _ => Box::new(TinyVocaSpeaker::new(store)),
    };
    let voice = speaker.voices().remove(0).id;
    let started = Instant::now();
    speaker.synthesize("Warm up.", &voice, 1.0).unwrap();
    println!(
        "load+warmup {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    std::fs::create_dir_all(out).unwrap();
    let texts = std::fs::read_to_string(texts).unwrap();
    for (index, reply) in texts
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let mut chunker = SentenceChunker::new(ChunkerOptions::default());
        let mut sentences = chunker.push(reply);
        sentences.extend(chunker.finish());
        let started = Instant::now();
        let mut first_audio = None;
        let mut samples = Vec::new();
        let mut sample_rate = 24_000;
        for sentence in &sentences {
            let audio = speaker.synthesize(sentence, &voice, 1.0).unwrap();
            first_audio.get_or_insert(started.elapsed());
            sample_rate = audio.sample_rate;
            samples.extend(audio.samples);
        }
        let elapsed = started.elapsed().as_secs_f64();
        let duration = samples.len() as f64 / f64::from(sample_rate);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer =
            hound::WavWriter::create(format!("{out}/{:02}.wav", index + 1), spec).unwrap();
        for sample in samples {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        println!(
            "{:02} sentences={} first_audio_ms={:.0} rtf={:.3} audio_s={duration:.2}",
            index + 1,
            sentences.len(),
            first_audio.unwrap_or_default().as_secs_f64() * 1000.0,
            elapsed / duration.max(1e-6),
        );
    }
}
