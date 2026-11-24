//! Removes filler words, repeats, and restarts from voice transcripts before they reach an input,
//! per the "Remove filler words" setting, for local and hosted transcription alike. Parakeet's
//! decoder-side filler suppression stays off: a filler is the model's best cue for a restart ("go
//! to the, uh, open the file"), so dropping it first leaves the restart in.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use local_transcription::DisfluencyTagger;
use local_transcription::disfluency::{
    DEFAULT_THRESHOLD, DisfluencyScorer, clean, remove_disfluencies,
};

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Applies the `voice_remove_fillers` setting, which background transcription reads.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether filler words are removed from transcripts.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

static TAGGER: OnceLock<Option<DisfluencyTagger>> = OnceLock::new();

/// Loads the bundled tagger on a background thread, once. Until it has loaded, finals get the rule
/// pass only, so a transcript never waits on ONNX Runtime starting up.
pub fn preload() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("disfluency-model".to_owned())
        .spawn(|| {
            TAGGER.get_or_init(|| match DisfluencyTagger::load() {
                Ok(tagger) => Some(tagger),
                Err(error) => {
                    log::warn!("Couldn't load the disfluency model, using rules only: {error}");
                    None
                }
            });
        });
    if let Err(error) = spawned {
        log::warn!("Couldn't start loading the disfluency model: {error}");
    }
}

fn tagger() -> Option<&'static DisfluencyTagger> {
    preload();
    TAGGER.get().and_then(Option::as_ref)
}

/// Cleans a final transcript with the rule pass and the model, which takes a few milliseconds;
/// call it off the UI thread. `protected` terms are kept.
pub fn clean_final(text: &str, protected: &[String]) -> String {
    if !is_enabled() {
        return text.to_owned();
    }
    let scorer = tagger().map(|tagger| tagger as &dyn DisfluencyScorer);
    clean(text, protected, scorer, DEFAULT_THRESHOLD)
}

/// Cleans a partial transcript with the rule pass only, which is cheap enough for every update.
pub fn clean_partial(text: &str, protected: &[String]) -> String {
    if !is_enabled() {
        return text.to_owned();
    }
    remove_disfluencies(text, protected)
}
