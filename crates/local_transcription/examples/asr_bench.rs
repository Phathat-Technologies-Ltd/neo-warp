//! Measures Parakeet latency and CPU cost: final decodes of each clip, and a simulated hands-free
//! session that feeds the clip in real time and decodes partials on the stream's schedule.
//!
//! `cargo run --release -p local_transcription --features parakeet --example asr_bench --
//!  <model dir> <clip.wav>...`
//!
//! `ACCEL=cpu|auto` picks the execution provider (`auto` needs `--features coreml`), `THREADS`
//! the CPU intra-op threads, and `POLICY=fixed` the old fixed 600 ms partial schedule.

use std::path::Path;

use instant::Instant;
use local_transcription::decode_wav;
use local_transcription::stream::{StreamConfig, partial_due};
use local_transcription::tdt::{Acceleration, TdtModel};

fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the struct it's given.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// Runs `f`, returning its result, wall ms, and CPU ms.
fn measure<T>(f: impl FnOnce() -> T) -> (T, f64, f64) {
    let cpu = cpu_seconds();
    let wall = Instant::now();
    let result = f();
    (
        result,
        wall.elapsed().as_secs_f64() * 1000.0,
        (cpu_seconds() - cpu) * 1000.0,
    )
}

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let [_, model_dir, clips @ ..] = args.as_slice() else {
        eprintln!("usage: asr_bench <model dir> <clip.wav>...");
        std::process::exit(2);
    };
    let acceleration = match std::env::var("ACCEL").as_deref() {
        Ok("auto") => Acceleration::Auto,
        _ => Acceleration::Cpu,
    };
    let threads = std::env::var("THREADS").ok().and_then(|t| t.parse().ok());
    let fixed = std::env::var("POLICY").as_deref() == Ok("fixed");
    let (model, load_ms, _) =
        measure(|| TdtModel::load_accelerated(Path::new(model_dir), acceleration, threads));
    let (mut model, device) = model.expect("load model");
    println!("load {load_ms:.0} ms, encoder on {device:?}");
    let config = StreamConfig::default();
    for clip in clips {
        let audio = decode_wav(&std::fs::read(clip).expect("read clip")).expect("decode clip");
        let samples = &audio.samples;
        let clip_ms = samples.len() as f64 / 16.0;
        let _ = model.transcribe(samples, None);
        let mut finals = Vec::new();
        let mut text = String::new();
        for _ in 0..3 {
            let (decoded, wall, cpu) = measure(|| model.transcribe(samples, None).expect("final"));
            text = decoded.text;
            finals.push((wall, cpu));
        }
        finals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (final_wall, final_cpu) = finals[1];

        // Real-time session: one 32 ms VAD frame at a time, at most one decode in flight, and a
        // due partial waits (replacing older ones) until the transcriber is free. CPU counts the
        // whole session, including ONNX Runtime threads spinning between decodes.
        let frame_ms = 32.0;
        let mut t = 0.0;
        let mut since_partial = 0.0;
        let mut pending: Option<f64> = None;
        let mut last_decode_ms = 0.0f64;
        let mut partials = 0;
        let mut first_partial = None;
        let mut partial_walls = Vec::new();
        let session_start = Instant::now();
        let session_cpu_start = cpu_seconds();
        let now_ms = || session_start.elapsed().as_secs_f64() * 1000.0;
        while t < clip_ms {
            t = (t + frame_ms).min(clip_ms);
            let lag = t - now_ms();
            if lag > 0.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64(lag / 1000.0));
            }
            since_partial += frame_ms;
            let due = if fixed {
                since_partial >= config.partial_interval_ms as f64
            } else {
                partial_due(
                    &config,
                    t as u32,
                    since_partial as u32,
                    partials == 0 && pending.is_none(),
                    last_decode_ms as u32,
                )
            };
            if due {
                since_partial = 0.0;
                pending = Some(t);
            }
            if now_ms() >= t
                && let Some(at) = pending.take()
            {
                let end = (at * 16.0) as usize;
                let start = end.saturating_sub(config.max_partial_ms as usize * 16);
                let (_, wall, _) = measure(|| model.transcribe(&samples[start..end], None));
                last_decode_ms = wall;
                partials += 1;
                partial_walls.push(wall);
                first_partial.get_or_insert(now_ms());
            }
        }
        let _ = model.transcribe(samples, None);
        let final_latency = now_ms() - clip_ms;
        let session_cpu = (cpu_seconds() - session_cpu_start) * 1000.0;
        let session_wall = now_ms();
        let mean_partial = partial_walls.iter().sum::<f64>() / partial_walls.len().max(1) as f64;
        println!(
            "{clip}: {clip_ms:.0} ms | final {final_wall:.0} ms wall, {:.0}% CPU | session: \
             {} encoder runs ({partials} partials, mean {mean_partial:.0} ms), first partial at \
             {:.0} ms, final latency {final_latency:.0} ms, {:.0}% CPU over the utterance | {text}",
            final_cpu / final_wall * 100.0,
            partials + 1,
            first_partial.unwrap_or(f64::NAN),
            session_cpu / session_wall * 100.0,
        );
    }
}
