//! Spike: captures the microphone and a Core Audio global process tap (everything the Mac plays)
//! through one private aggregate device, plays test audio from another process (`afplay`), then
//! runs WebRTC AEC3 (the pure-Rust `sonora` port) offline with the tap as the far end and reports
//! the echo reduction. Run with `cargo run -p voice_input --example system_aec_probe`.

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("system_aec_probe needs macOS 14.2+");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::process::Command;
    use std::ptr::{NonNull, null, null_mut};
    use std::time::{Duration, Instant};

    use anyhow::{Context, bail};
    use objc2::AnyThread;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_core_audio::*;
    use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
    use objc2_core_foundation::{CFDictionary, CFString};
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
    use parking_lot::Mutex;
    use sonora::config::{EchoCanceller, NoiseSuppression};
    use sonora::{AudioProcessing, Config, StreamConfig};

    #[derive(Default)]
    struct Capture {
        mic: Vec<f32>,
        tap: Vec<f32>,
        layout_logged: bool,
        peaks: Vec<f32>,
    }

    static CAPTURE: Mutex<Option<Capture>> = Mutex::new(None);

    fn check(status: i32, what: &str) -> anyhow::Result<()> {
        if status != 0 {
            let bytes = status.to_be_bytes();
            bail!(
                "{what} failed: {status} ({})",
                String::from_utf8_lossy(&bytes)
            );
        }
        Ok(())
    }

    fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    unsafe fn get<T: Copy>(object: u32, selector: u32, scope: u32, init: T) -> anyhow::Result<T> {
        let mut value = init;
        let mut size = size_of::<T>() as u32;
        let addr = address(selector, scope);
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&addr),
                0,
                null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        check(status, "AudioObjectGetPropertyData")?;
        Ok(value)
    }

    unsafe fn device_uid(device: u32) -> anyhow::Result<Retained<NSString>> {
        let cf: *const CFString = unsafe {
            get(
                device,
                kAudioDevicePropertyDeviceUID,
                kAudioObjectPropertyScopeGlobal,
                null(),
            )?
        };
        // CFString is toll-free bridged to NSString; the property getter returns +1.
        Ok(unsafe { Retained::from_raw(cf as *mut NSString) }.context("null device UID")?)
    }

    unsafe extern "C-unwind" fn io_proc(
        _device: AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        input: NonNull<AudioBufferList>,
        _input_time: NonNull<AudioTimeStamp>,
        _output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        _client: *mut c_void,
    ) -> i32 {
        let list = unsafe { input.as_ref() };
        let buffers = unsafe {
            std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize)
        };
        let mut guard = CAPTURE.lock();
        let Some(capture) = guard.as_mut() else {
            return 0;
        };
        if !capture.layout_logged {
            capture.layout_logged = true;
            let layout: Vec<_> = buffers
                .iter()
                .map(|b| (b.mNumberChannels, b.mDataByteSize))
                .collect();
            eprintln!("input buffers (channels, bytes): {layout:?}");
        }
        let as_f32 = |b: &objc2_core_audio_types::AudioBuffer| unsafe {
            std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4)
        };
        capture.peaks.resize(buffers.len(), 0.0);
        for (peak, buffer) in capture.peaks.iter_mut().zip(buffers) {
            *peak = as_f32(buffer).iter().fold(*peak, |p, s| p.max(s.abs()));
        }
        if buffers.len() < 2 {
            return 0;
        }
        let mic = &buffers[0];
        let tap = &buffers[buffers.len() - 1];
        let mic_ch = mic.mNumberChannels.max(1) as usize;
        let tap_ch = tap.mNumberChannels.max(1) as usize;
        capture
            .mic
            .extend(as_f32(mic).chunks(mic_ch).map(|frame| frame[0]));
        capture.tap.extend(
            as_f32(tap)
                .chunks(tap_ch)
                .map(|frame| frame.iter().sum::<f32>() / tap_ch as f32),
        );
        0
    }

    struct Session {
        tap: u32,
        aggregate: u32,
        proc_id: AudioDeviceIOProcID,
        rate: f64,
    }

    impl Session {
        unsafe fn start() -> anyhow::Result<Self> {
            unsafe {
                let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                    CATapDescription::alloc(),
                    &NSArray::new(),
                );
                description.setPrivate(true);
                description.setName(&NSString::from_str("system-aec-probe tap"));
                let mut tap = 0;
                check(
                    AudioHardwareCreateProcessTap(Some(&description), &mut tap),
                    "AudioHardwareCreateProcessTap",
                )?;
                let tap_uuid = description.UUID().UUIDString();

                let input_device: u32 = get(
                    kAudioObjectSystemObject as u32,
                    kAudioHardwarePropertyDefaultInputDevice,
                    kAudioObjectPropertyScopeGlobal,
                    0,
                )?;
                let input_uid = device_uid(input_device)?;

                let key = |k: &std::ffi::CStr| NSString::from_str(k.to_str().unwrap());
                let sub_device = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                    &[&*key(kAudioSubDeviceUIDKey)],
                    &[Retained::into_super(Retained::into_super(
                        input_uid.clone(),
                    ))],
                );
                let sub_tap = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                    &[&*key(kAudioSubTapUIDKey), &*NSString::from_str("drift")],
                    &[
                        Retained::into_super(Retained::into_super(tap_uuid)),
                        Retained::into_super(Retained::into_super(Retained::into_super(
                            NSNumber::new_bool(true),
                        ))),
                    ],
                );
                let objects: Vec<Retained<AnyObject>> = vec![
                    Retained::into_super(Retained::into_super(NSString::from_str(
                        "com.warp.system-aec-probe",
                    ))),
                    Retained::into_super(Retained::into_super(NSString::from_str(
                        "system-aec-probe",
                    ))),
                    Retained::into_super(Retained::into_super(input_uid)),
                    Retained::into_super(Retained::into_super(Retained::into_super(
                        NSNumber::new_bool(true),
                    ))),
                    Retained::into_super(Retained::into_super(Retained::into_super(
                        NSNumber::new_bool(true),
                    ))),
                    Retained::into_super(Retained::into_super(NSArray::from_retained_slice(&[
                        sub_device,
                    ]))),
                    Retained::into_super(Retained::into_super(NSArray::from_retained_slice(&[
                        sub_tap,
                    ]))),
                ];
                let keys = [
                    key(kAudioAggregateDeviceUIDKey),
                    key(kAudioAggregateDeviceNameKey),
                    key(kAudioAggregateDeviceMainSubDeviceKey),
                    key(kAudioAggregateDeviceIsPrivateKey),
                    key(kAudioAggregateDeviceTapAutoStartKey),
                    key(kAudioAggregateDeviceSubDeviceListKey),
                    key(kAudioAggregateDeviceTapListKey),
                ];
                let mut key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
                let mut objects = objects;
                if std::env::var_os("SYSTEM_AEC_PROBE_NO_TAP").is_some() {
                    key_refs.pop();
                    objects.pop();
                }
                let dict =
                    NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &objects);
                let cf_dict = &*(Retained::as_ptr(&dict) as *const CFDictionary);
                let mut aggregate = 0;
                check(
                    AudioHardwareCreateAggregateDevice(cf_dict, NonNull::from(&mut aggregate)),
                    "AudioHardwareCreateAggregateDevice",
                )?;
                let rate: f64 = get(
                    aggregate,
                    kAudioDevicePropertyNominalSampleRate,
                    kAudioObjectPropertyScopeGlobal,
                    0.0,
                )?;

                *CAPTURE.lock() = Some(Capture::default());
                let mut proc_id: AudioDeviceIOProcID = None;
                check(
                    AudioDeviceCreateIOProcID(
                        aggregate,
                        Some(io_proc),
                        null_mut(),
                        NonNull::from(&mut proc_id),
                    ),
                    "AudioDeviceCreateIOProcID",
                )?;
                check(AudioDeviceStart(aggregate, proc_id), "AudioDeviceStart")?;
                Ok(Self {
                    tap,
                    aggregate,
                    proc_id,
                    rate,
                })
            }
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                AudioDeviceStop(self.aggregate, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
                AudioHardwareDestroyAggregateDevice(self.aggregate);
                AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }

    fn energy(samples: &[f32]) -> f64 {
        samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64
    }

    fn db(ratio: f64) -> f64 {
        10.0 * ratio.max(1e-12).log10()
    }

    /// Music-like test signal: a slowly changing chord with a noisy, amplitude-modulated layer.
    fn write_music(path: &str, seconds: f32) -> anyhow::Result<()> {
        let rate = 48_000u32;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec)?;
        let mut seed = 1u32;
        for i in 0..(rate as f32 * seconds) as usize {
            let t = i as f32 / rate as f32;
            let root = [220.0, 261.6, 196.0, 293.7][(t / 1.0) as usize % 4];
            let tonal: f32 = [1.0, 1.25, 1.5, 2.0]
                .iter()
                .map(|m| (2.0 * std::f32::consts::PI * root * m * t).sin())
                .sum::<f32>()
                / 4.0;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
            let beat = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 2.0 * t).sin().max(0.0);
            let sample = 0.25 * tonal + 0.15 * noise * beat;
            writer.write_sample((sample * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
        Ok(())
    }

    struct Clip {
        name: &'static str,
        command: Vec<String>,
    }

    fn run_case(clip: &Clip) -> anyhow::Result<()> {
        let created = Instant::now();
        let session = unsafe { Session::start()? };
        println!(
            "tap + aggregate created and started in {} ms",
            created.elapsed().as_millis()
        );
        let rate = session.rate;
        let opened = Instant::now();
        while CAPTURE.lock().as_ref().is_some_and(|c| c.mic.is_empty()) {
            if opened.elapsed() > Duration::from_secs(15) {
                bail!(
                    "no audio after 15 s; microphone or system audio permission pending or denied"
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        println!(
            "first audio {} ms after AudioDeviceStart",
            opened.elapsed().as_millis()
        );
        std::thread::sleep(Duration::from_millis(1000));
        let quiet_end = CAPTURE.lock().as_ref().map_or(0, |c| c.mic.len());
        let started = Instant::now();
        let status = Command::new(&clip.command[0])
            .args(&clip.command[1..])
            .status()?;
        let played = started.elapsed();
        std::thread::sleep(Duration::from_millis(300));
        let play_end = CAPTURE.lock().as_ref().map_or(0, |c| c.mic.len());
        drop(session);
        let capture = CAPTURE.lock().take().unwrap_or_default();
        eprintln!("per-buffer peaks: {:?}", capture.peaks);
        if !status.success() {
            bail!("{} exited with {status}", clip.command[0]);
        }
        let len = capture.mic.len().min(capture.tap.len());
        let (mic, tap) = (&capture.mic[..len], &capture.tap[..len]);
        let rate_hz = rate as u32;
        if ![16_000, 32_000, 48_000].contains(&rate_hz) {
            bail!("aggregate runs at {rate} Hz; the spike only handles 16/32/48 kHz");
        }
        let frame = rate_hz as usize / 100;

        let mut apm_aec = AudioProcessing::builder()
            .config(Config {
                echo_canceller: Some(EchoCanceller::default()),
                ..Default::default()
            })
            .capture_config(StreamConfig::new(rate_hz, 1))
            .render_config(StreamConfig::new(rate_hz, 1))
            .build();
        let mut apm_ns = AudioProcessing::builder()
            .config(Config {
                echo_canceller: Some(EchoCanceller::default()),
                noise_suppression: Some(NoiseSuppression::default()),
                ..Default::default()
            })
            .capture_config(StreamConfig::new(rate_hz, 1))
            .render_config(StreamConfig::new(rate_hz, 1))
            .build();

        let mut out_aec = vec![0.0f32; len];
        let mut out_ns = vec![0.0f32; len];
        let mut scratch = vec![0.0f32; frame];
        let mut aec_time = Duration::ZERO;
        let frames = len / frame;
        for f in 0..frames {
            let range = f * frame..(f + 1) * frame;
            let t0 = Instant::now();
            apm_aec.process_render_f32(&[&tap[range.clone()]], &mut [&mut scratch])?;
            apm_aec
                .process_capture_f32(&[&mic[range.clone()]], &mut [&mut out_aec[range.clone()]])?;
            aec_time += t0.elapsed();
            apm_ns.process_render_f32(&[&tap[range.clone()]], &mut [&mut scratch])?;
            apm_ns
                .process_capture_f32(&[&mic[range.clone()]], &mut [&mut out_ns[range.clone()]])?;
        }
        let stats = apm_aec.statistics().clone();
        let usable = frames * frame;
        let play_end = play_end.min(usable);
        let quiet = 0..quiet_end.min(usable);
        let playing = quiet_end.min(play_end)..play_end;
        let tail = (playing.start + playing.end) / 2..playing.end;

        let floor = energy(&mic[quiet.clone()]);
        let mic_play = energy(&mic[playing.clone()]);
        let tap_play = energy(&tap[playing.clone()]);
        let echo_raw = (mic_play - floor).max(1e-12);
        println!(
            "== {} ({:.1} s played, aggregate {rate} Hz) ==",
            clip.name,
            played.as_secs_f32()
        );
        println!(
            "  tap level {:.1} dBFS, mic noise floor {:.1} dBFS, mic during playback {:.1} dBFS (echo-to-floor {:.1} dB)",
            db(tap_play),
            db(floor),
            db(mic_play),
            db(echo_raw / floor.max(1e-12))
        );
        for (label, out) in [("AEC3", &out_aec), ("AEC3+NS", &out_ns)] {
            let whole = energy(&out[playing.clone()]);
            let tail_raw = energy(&mic[tail.clone()]);
            let tail_out = energy(&out[tail.clone()]);
            let out_floor = energy(&out[quiet.clone()]);
            println!(
                "  {label:8} reduction whole {:.1} dB, second half {:.1} dB; residual above its own floor {:.1} dB",
                db(mic_play / whole),
                db(tail_raw / tail_out),
                db(tail_out / out_floor.max(1e-12)),
            );
        }
        println!(
            "  AEC3 cpu: {:.3} ms per 10 ms frame ({:.1}% of one core); stats erle={:?} delay_ms={:?}",
            aec_time.as_secs_f64() * 1000.0 / frames as f64,
            aec_time.as_secs_f64() / (frames as f64 * 0.01) * 100.0,
            stats.echo_return_loss_enhancement,
            stats.delay_ms,
        );
        if std::env::var_os("SYSTEM_AEC_PROBE_DUMP").is_some() {
            let spec = hound::WavSpec {
                channels: 3,
                sample_rate: rate_hz,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let path = format!("/tmp/system_aec_probe_{}.wav", clip.name);
            let mut writer = hound::WavWriter::create(&path, spec)?;
            for i in 0..usable {
                writer.write_sample(mic[i])?;
                writer.write_sample(tap[i])?;
                writer.write_sample(out_aec[i])?;
            }
            writer.finalize()?;
            println!("  wrote {path} (mic, tap, aec)");
        }
        Ok(())
    }

    pub fn run() -> anyhow::Result<()> {
        let dir = std::env::temp_dir();
        let music = dir.join("system_aec_probe_music.wav");
        write_music(music.to_str().unwrap(), 5.0)?;
        let speech = dir.join("system_aec_probe_speech.aiff");
        let status = Command::new("say")
            .args([
                "-o",
                speech.to_str().unwrap(),
                "The quick brown fox jumps over the lazy dog. This sentence is played by another process so the canceller has to remove it from the microphone.",
            ])
            .status()?;
        if !status.success() {
            bail!("say -o failed");
        }
        let volume = std::env::var("SYSTEM_AEC_PROBE_VOLUME").unwrap_or_else(|_| "0.5".into());
        let clips = [
            Clip {
                name: "afplay_speech",
                command: vec![
                    "afplay".into(),
                    "-v".into(),
                    volume.clone(),
                    speech.to_str().unwrap().into(),
                ],
            },
            Clip {
                name: "afplay_music",
                command: vec![
                    "afplay".into(),
                    "-v".into(),
                    volume.clone(),
                    music.to_str().unwrap().into(),
                ],
            },
            Clip {
                name: "say_live",
                command: vec![
                    "say".into(),
                    "Hands free mode should not transcribe this sentence spoken by the system voice.".into(),
                ],
            },
        ];
        for clip in &clips {
            if let Err(error) = run_case(clip) {
                println!("== {} failed: {error:#}", clip.name);
            }
        }
        Ok(())
    }
}
