//! Compares filler suppression in the Parakeet decoder with the text-side disfluency passes on
//! clips listed in a manifest of `wav<TAB>kind<TAB>spoken text<TAB>clean reference` rows (kind `D`
//! disfluent, `F` fluent control). Reports WER against the clean reference, fillers left, words
//! removed from controls relative to plain decoding, and latency.
//!
//! `cargo run --release -p local_transcription --features parakeet,disfluency --example
//!  filler_eval -- <model dir> <manifest.tsv>`

use std::collections::HashMap;
use std::path::Path;

use instant::Instant;
use local_transcription::disfluency::{DEFAULT_THRESHOLD, clean};
use local_transcription::filler::{DEFAULT_FILLER_PENALTY, FillerSuppression};
use local_transcription::tdt::TdtModel;
use local_transcription::{DisfluencyTagger, decode_wav};

const FILLERS: &[&str] = &[
    "uh", "um", "uhm", "umm", "er", "erm", "eh", "ah", "hmm", "hm", "mm",
];

fn norm(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = if x == y {
                previous
            } else {
                1 + previous.min(row[j]).min(row[j + 1])
            };
            previous = current;
        }
    }
    row[b.len()]
}

/// Words of `before` missing from `after`, as a multiset difference.
fn lost(before: &str, after: &str) -> usize {
    let mut counts: HashMap<String, i64> = HashMap::new();
    for w in norm(before) {
        *counts.entry(w).or_default() += 1;
    }
    for w in norm(after) {
        *counts.entry(w).or_default() -= 1;
    }
    counts.values().filter(|c| **c > 0).sum::<i64>() as usize
}

#[derive(Default)]
struct Score {
    errors: usize,
    reference_words: usize,
    fillers_left: usize,
    control_words_lost: usize,
    controls_touched: usize,
    ms: Vec<f64>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, model_dir, manifest] = args.as_slice() else {
        eprintln!("usage: filler_eval <model dir> <manifest.tsv>");
        std::process::exit(2);
    };
    let penalty = std::env::var("FILLER_PENALTY")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_FILLER_PENALTY);
    let base = Path::new(manifest).parent().unwrap_or(Path::new("."));
    let mut model = TdtModel::load_cpu(Path::new(model_dir)).expect("load model");
    let tagger = DisfluencyTagger::load().expect("load tagger");
    let verbose = std::env::var_os("VERBOSE").is_some();
    let pipelines = [
        "asr",
        "decoder",
        "asr+rules",
        "asr+rules+model",
        "decoder+rules+model",
    ];
    let mut scores: Vec<Score> = pipelines.iter().map(|_| Score::default()).collect();
    let (mut plain_ms, mut suppressed_ms) = (Vec::new(), Vec::new());
    for line in std::fs::read_to_string(manifest).expect("manifest").lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        let [wav, kind, spoken, reference] = parts[..] else {
            continue;
        };
        let wav = base.join(Path::new(wav).file_name().expect("file name"));
        let audio = decode_wav(&std::fs::read(&wav).expect("read wav")).expect("decode wav");
        let encoded = model.encode(&audio.samples).expect("encode");
        model.set_filler_suppression(None);
        let start = Instant::now();
        let plain = model.decode(&encoded, None).expect("decode").text;
        plain_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        model.set_filler_suppression(Some(FillerSuppression {
            penalty,
            ..FillerSuppression::for_language(Some("en"))
        }));
        let start = Instant::now();
        let suppressed = model.decode(&encoded, None).expect("decode").text;
        suppressed_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        let reference_words = norm(reference);
        for (pipeline, score) in pipelines.iter().zip(&mut scores) {
            let start = Instant::now();
            let output = match *pipeline {
                "asr" => plain.clone(),
                "decoder" => suppressed.clone(),
                "asr+rules" => clean(&plain, &[], None, DEFAULT_THRESHOLD),
                "asr+rules+model" => clean(&plain, &[], Some(&tagger), DEFAULT_THRESHOLD),
                _ => clean(&suppressed, &[], Some(&tagger), DEFAULT_THRESHOLD),
            };
            score.ms.push(start.elapsed().as_secs_f64() * 1000.0);
            let words = norm(&output);
            score.errors += edit_distance(&words, &reference_words);
            score.reference_words += reference_words.len();
            score.fillers_left += words
                .iter()
                .filter(|w| FILLERS.contains(&w.as_str()))
                .count();
            if kind == "F" {
                let n = lost(&plain, &output);
                score.control_words_lost += n;
                score.controls_touched += usize::from(n > 0);
            }
            if verbose {
                println!("{pipeline:20} {kind} {output}");
            }
        }
        if verbose {
            println!("{:20} {kind} {spoken}\n", "spoken");
        }
    }
    let fillers_heard = scores[0].fillers_left;
    plain_ms.sort_by(f64::total_cmp);
    suppressed_ms.sort_by(f64::total_cmp);
    println!(
        "decode p50: plain {:.2} ms, with suppression {:.2} ms",
        plain_ms[plain_ms.len() / 2],
        suppressed_ms[suppressed_ms.len() / 2]
    );
    println!(
        "| pipeline | WER vs clean ref | fillers left (of {fillers_heard} in ASR) | control words lost | controls touched | text p50 ms |"
    );
    println!("|---|---|---|---|---|---|");
    for (pipeline, mut score) in pipelines.iter().zip(scores) {
        score.ms.sort_by(f64::total_cmp);
        println!(
            "| {pipeline} | {:.1}% | {} | {} | {} | {:.2} |",
            100.0 * score.errors as f64 / score.reference_words.max(1) as f64,
            score.fillers_left,
            score.control_words_lost,
            score.controls_touched,
            score.ms[score.ms.len() / 2],
        );
    }
}
