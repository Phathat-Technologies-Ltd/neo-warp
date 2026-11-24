//! Scores disfluency removal on a TSV of `kind<TAB>input<TAB>reference` rows, where kind is `D`
//! (disfluent) or `F` (fluent control) and the reference is the input with disfluent words removed.
//! Reports precision and recall of removed words, exact matches, removals on controls, and latency
//! for the rule pass, the model alone, and both.
//!
//! `cargo run --release -p local_transcription --features disfluency --example disfluency_eval --
//!  <eval.tsv> [threshold]`

use std::collections::HashMap;

use instant::Instant;
use local_transcription::DisfluencyTagger;
use local_transcription::disfluency::{
    DEFAULT_THRESHOLD, DisfluencyScorer, clean, clean_model_only,
};

fn norm(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase()
}

fn counts(text: &str) -> HashMap<String, i64> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        *counts.entry(norm(word)).or_default() += 1;
    }
    counts
}

/// Removed words: input counts minus output counts.
fn removed(input: &str, output: &str) -> HashMap<String, i64> {
    let mut removed = counts(input);
    for (word, count) in counts(output) {
        *removed.entry(word).or_default() -= count;
    }
    removed.retain(|_, count| *count > 0);
    removed
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .expect("usage: disfluency_eval <eval.tsv> [threshold]");
    let threshold = args
        .get(2)
        .and_then(|t| t.parse().ok())
        .unwrap_or(DEFAULT_THRESHOLD);
    let rows: Vec<(String, String, String)> = std::fs::read_to_string(path)
        .expect("read eval set")
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            Some((
                parts.next()?.to_owned(),
                parts.next()?.to_owned(),
                parts.next()?.to_owned(),
            ))
        })
        .collect();
    let tagger = DisfluencyTagger::load().expect("load tagger");
    // Warm up ONNX Runtime before timing.
    let _ = tagger.delete_probabilities(&["warm", "up"]);
    let verbose = std::env::var_os("VERBOSE").is_some();
    for mode in ["rules", "model", "rules+model"] {
        let (mut tp, mut fp, mut fn_) = (0i64, 0i64, 0i64);
        let (mut exact, mut control_utterances, mut control_removed, mut control_words) =
            (0, 0, 0, 0);
        let mut disfluent = 0;
        let mut latencies = Vec::new();
        for (kind, input, reference) in &rows {
            let start = Instant::now();
            let output = match mode {
                "rules" => clean(input, &[], None, threshold),
                "model" => clean_model_only(input, &[], &tagger, threshold),
                _ => clean(input, &[], Some(&tagger), threshold),
            };
            latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            let gold = removed(input, reference);
            let predicted = removed(input, &output);
            for (word, count) in &predicted {
                let hit = (*count).min(*gold.get(word).unwrap_or(&0));
                tp += hit;
                fp += count - hit;
            }
            for (word, count) in &gold {
                fn_ += (count - predicted.get(word).unwrap_or(&0)).max(0);
            }
            let same = output
                .split_whitespace()
                .map(norm)
                .eq(reference.split_whitespace().map(norm));
            if kind == "F" {
                let n: i64 = predicted.values().sum();
                control_utterances += i64::from(n > 0);
                control_removed += n;
                control_words += input.split_whitespace().count() as i64;
            } else {
                disfluent += 1;
            }
            exact += i64::from(same);
            if verbose && !same {
                println!("  [{mode}] {kind} {input}\n      -> {output}\n      want {reference}");
            }
        }
        latencies.sort_by(f64::total_cmp);
        let p = tp as f64 / (tp + fp).max(1) as f64;
        let r = tp as f64 / (tp + fn_).max(1) as f64;
        let controls = rows.len() - disfluent;
        println!(
            "{mode:12} P={p:.3} R={r:.3} F1={:.3} exact={exact}/{} controls_touched={control_utterances}/{controls} control_words_removed={control_removed}/{control_words} p50={:.2}ms p95={:.2}ms",
            2.0 * p * r / (p + r).max(1e-9),
            rows.len(),
            latencies[latencies.len() / 2],
            latencies[latencies.len() * 95 / 100],
        );
    }
}
