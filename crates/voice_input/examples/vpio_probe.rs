//! Starts the voice-processing unit on this Mac and measures how much of a played tone the
//! microphone still hears. Run with
//! `cargo run -p voice_input --features voice_processing --example vpio_probe`.

#[cfg(all(target_os = "macos", feature = "voice_processing"))]
fn main() -> anyhow::Result<()> {
    use voice_input::voice_processing::probe_duplex;

    struct Stderr;
    impl log::Log for Stderr {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }
        fn log(&self, record: &log::Record) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
        fn flush(&self) {}
    }
    log::set_logger(&Stderr).map_err(|error| anyhow::anyhow!("{error}"))?;
    log::set_max_level(log::LevelFilter::Info);

    let rate = 24_000.0;
    let tone: Vec<f32> = (0..rate as usize * 3)
        .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate).sin())
        .collect();
    let raw = probe_duplex(&tone, 440.0, 2.5, true)?;
    println!("canceller bypassed: {raw:?}");
    let cancelled = probe_duplex(&tone, 440.0, 2.5, false)?;
    println!("canceller on:       {cancelled:?}");
    println!(
        "440 Hz echo reduction: {:.1} dB (played amplitude 0.3)",
        20.0 * (raw.during_tone / cancelled.during_tone.max(1e-9)).log10()
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "voice_processing")))]
fn main() {
    eprintln!("vpio_probe needs macOS and the voice_processing feature");
}
