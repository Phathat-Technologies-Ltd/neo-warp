//! Prints the microphone mode before and while the voice-processing unit captures, to check
//! whether Mic Mode applies to it. Run with `--features voice_processing`; pass `--picker` to
//! open the system picker during capture.

fn main() -> anyhow::Result<()> {
    let print = |when: &str| {
        println!(
            "{when}: {:?}",
            voice_input::microphone_mode::microphone_mode()
        )
    };
    print("idle");
    let picker = std::env::args().any(|arg| arg == "--picker");
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        print("capturing on voice processing");
        if picker {
            voice_input::microphone_mode::show_microphone_mode_picker();
        }
        for _ in 0..5 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            print("capturing");
        }
    });
    let tone = vec![0.0; 24_000];
    let levels = voice_input::voice_processing::probe_duplex(&tone, 440.0, 6.0, false)?;
    println!("{levels:?}");
    handle.join().ok();
    print("after");
    Ok(())
}
