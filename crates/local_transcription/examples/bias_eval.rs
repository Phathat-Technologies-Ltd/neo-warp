//! Evaluates contextual biasing: transcribes each WAV in a manifest once without bias and once per
//! bias list, and writes one JSON line per utterance with every transcript and decode time.
//!
//! `cargo run --release -p local_transcription --features parakeet --example bias_eval --
//!  <model dir> <manifest.jsonl> <lists.json> <out.jsonl>`
//!
//! Manifest lines: `{"wav": path, ...}` (other fields are copied through). `lists.json` maps a
//! list name to an array of `{"text": ..., "trusted": bool}` bias terms.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;
use instant::Instant;

use local_transcription::bias::{BiasConfig, BiasTerm, BoostingTree};
use local_transcription::context_correction::{
    CorrectionContext, CorrectionLog, DEFAULT_THRESHOLD, Vocabulary, correct,
};
use local_transcription::decode_wav;
use local_transcription::tdt::TdtModel;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, model_dir, manifest, lists, out] = args.as_slice() else {
        eprintln!("usage: bias_eval <model dir> <manifest.jsonl> <lists.json> <out.jsonl>");
        std::process::exit(2);
    };
    let weight = std::env::var("BIAS_WEIGHT")
        .ok()
        .and_then(|w| w.parse().ok())
        .unwrap_or(BiasConfig::default().weight);
    let config = BiasConfig {
        weight,
        ..BiasConfig::default()
    };
    let mut model = TdtModel::load_cpu(Path::new(model_dir)).expect("load model");
    let lists: BTreeMap<String, Vec<BiasTerm>> =
        serde_json::from_str(&std::fs::read_to_string(lists).expect("read lists"))
            .expect("parse lists");
    let trees: BTreeMap<&String, BoostingTree> = lists
        .iter()
        .map(|(name, terms)| {
            let started = Instant::now();
            let tree = BoostingTree::build(terms, config);
            eprintln!(
                "{name}: {} terms, {} nodes, built in {:?}",
                tree.term_count(),
                tree.node_count(),
                started.elapsed()
            );
            (name, tree)
        })
        .collect();
    let vocabularies: BTreeMap<&String, Vocabulary> = lists
        .iter()
        .map(|(name, terms)| {
            let mut vocabulary = Vocabulary::default();
            vocabulary.extend(terms.iter().map(|term| term.text.as_str()));
            (name, vocabulary)
        })
        .collect();
    let log = CorrectionLog::default();
    let corrected = |text: &str, vocabulary: &Vocabulary| {
        correct(text, vocabulary, &log, DEFAULT_THRESHOLD, CorrectionContext::Prose).text
    };
    let mut out = std::fs::File::create(out).expect("create output");
    let manifest = std::fs::File::open(manifest).expect("open manifest");
    for line in std::io::BufReader::new(manifest).lines() {
        let line = line.expect("read manifest");
        let mut row: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&line).expect("parse manifest line");
        let wav = row["wav"].as_str().expect("wav path").to_owned();
        let audio = decode_wav(&std::fs::read(&wav).expect("read wav")).expect("decode wav");
        let started = Instant::now();
        let encoded = model.encode(&audio.samples).expect("encode");
        row.insert("encode_ms".into(), elapsed_ms(started).into());
        let started = Instant::now();
        let base = model.decode(&encoded, None).expect("decode");
        row.insert("base_ms".into(), elapsed_ms(started).into());
        let mut base_corrected = serde_json::Map::new();
        let mut outputs_corrected = serde_json::Map::new();
        let mut outputs = serde_json::Map::new();
        let mut times = serde_json::Map::new();
        for (name, tree) in &trees {
            let started = Instant::now();
            let decoded = model.decode(&encoded, Some(tree)).expect("decode");
            times.insert((*name).clone(), elapsed_ms(started).into());
            let vocabulary = &vocabularies[name];
            base_corrected.insert((*name).clone(), corrected(&base.text, vocabulary).into());
            outputs_corrected.insert((*name).clone(), corrected(&decoded.text, vocabulary).into());
            outputs.insert((*name).clone(), decoded.text.into());
        }
        row.insert("base".into(), base.text.into());
        row.insert("base_corrected".into(), base_corrected.into());
        row.insert("biased_corrected".into(), outputs_corrected.into());
        row.insert("biased".into(), outputs.into());
        row.insert("biased_ms".into(), times.into());
        writeln!(out, "{}", serde_json::Value::Object(row)).expect("write output");
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
