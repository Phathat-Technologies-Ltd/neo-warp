//! Runs the app's system echo cancellation pipeline for a few seconds and prints what the tap
//! carried. By default `afplay` plays a `say` clip from another process; with `--silent-self` this
//! process instead keeps a silent output stream open and nothing else plays, which must not be
//! judged a blocked tap. Launch it from a signed `.app` bundle with `open` so the system audio
//! recording grant applies; results go to the path given with `--out` (default
//! `$TMPDIR/system_tap_check.txt`).

use std::process::Command;
use std::time::Duration;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let silent_self = args.iter().any(|arg| arg == "--silent-self");
    let out = args
        .iter()
        .position(|arg| arg == "--out")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("system_tap_check.txt")
                .display()
                .to_string()
        });

    let mut player = None;
    let mut _stream = None;
    if silent_self {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no output device"))?;
        let config = device.default_output_config()?.config();
        let stream = device.build_output_stream(
            config,
            |data: &mut [f32], _| data.fill(0.0),
            |error| eprintln!("output error: {error}"),
            None,
        )?;
        stream.play()?;
        _stream = Some(stream);
    } else {
        let clip = std::env::temp_dir().join("system_tap_check.aiff");
        let clip = clip.display().to_string();
        Command::new("say")
            .args([
                "-o",
                &clip,
                "Testing the system audio tap, one two three four five six.",
            ])
            .status()?;
        player = Some(Command::new("afplay").args(["-v", "0.4", &clip]).spawn()?);
    }
    let duration = Duration::from_secs(if silent_self { 6 } else { 4 });
    let result = voice_input::voice_processing::system_aec::measure_tap(duration);
    if let Some(mut player) = player {
        let _ = player.kill();
    }
    let report = match result {
        Ok(levels) => format!("{levels:?}\n"),
        Err(error) => format!("error: {error:#}\n"),
    };
    std::fs::write(&out, &report)?;
    print!("{report}");
    Ok(())
}
