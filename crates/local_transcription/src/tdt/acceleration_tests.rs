use std::path::Path;

use super::route::{Route, cache_dir, routes};
use super::{COREML_ENCODER_DIR, FP16_ENCODER_FILE, has_accelerated_encoder};

/// A model directory and its Core ML cache, with the given accelerated encoders in place.
fn model_dir(root: &Path, package: bool, onnx: bool) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = root.join("parakeet-tdt-0.6b-v3-int8");
    std::fs::create_dir_all(&dir).unwrap();
    if package {
        std::fs::create_dir_all(dir.join(COREML_ENCODER_DIR)).unwrap();
    }
    if onnx {
        std::fs::write(dir.join(FP16_ENCODER_FILE), b"").unwrap();
    }
    let cache = cache_dir(&dir);
    std::fs::create_dir_all(&cache).unwrap();
    (dir, cache)
}

#[test]
fn prefers_the_package_then_onnx() {
    let root = tempfile::tempdir().unwrap();
    let (dir, cache) = model_dir(root.path(), true, true);
    assert!(has_accelerated_encoder(&dir));
    assert_eq!(routes(&dir, &cache, true), [Route::Package, Route::Onnx]);
}

#[test]
fn tries_only_the_encoders_that_are_there() {
    let root = tempfile::tempdir().unwrap();
    let (dir, cache) = model_dir(root.path(), false, false);
    assert!(!has_accelerated_encoder(&dir));
    assert_eq!(routes(&dir, &cache, true), []);

    std::fs::write(dir.join(FP16_ENCODER_FILE), b"").unwrap();
    assert!(has_accelerated_encoder(&dir));
    assert_eq!(routes(&dir, &cache, true), [Route::Onnx]);

    std::fs::remove_file(dir.join(FP16_ENCODER_FILE)).unwrap();
    std::fs::create_dir_all(dir.join(COREML_ENCODER_DIR)).unwrap();
    assert!(has_accelerated_encoder(&dir));
    assert_eq!(routes(&dir, &cache, true), [Route::Package]);
}

#[test]
fn skips_the_package_where_macos_cant_load_it() {
    let root = tempfile::tempdir().unwrap();
    let (dir, cache) = model_dir(root.path(), true, true);
    assert_eq!(routes(&dir, &cache, false), [Route::Onnx]);
}

#[test]
fn a_verdict_against_one_route_leaves_the_other() {
    let root = tempfile::tempdir().unwrap();
    let (dir, cache) = model_dir(root.path(), true, true);
    std::fs::write(cache.join("verdict-package"), "cpu").unwrap();
    assert_eq!(routes(&dir, &cache, true), [Route::Onnx]);

    std::fs::remove_file(cache.join("verdict-package")).unwrap();
    // The verdict ONNX Runtime's route wrote before the package existed.
    std::fs::write(cache.join("verdict"), "cpu").unwrap();
    assert_eq!(routes(&dir, &cache, true), [Route::Package]);
}

#[test]
fn falls_back_after_loads_keep_failing_to_finish() {
    let root = tempfile::tempdir().unwrap();
    let (dir, cache) = model_dir(root.path(), true, true);
    std::fs::write(cache.join("loading-package"), "2").unwrap();
    assert_eq!(routes(&dir, &cache, true), [Route::Package, Route::Onnx]);
    std::fs::write(cache.join("loading-package"), "3").unwrap();
    assert_eq!(routes(&dir, &cache, true), [Route::Onnx]);
    std::fs::write(cache.join("loading"), "3").unwrap();
    assert_eq!(routes(&dir, &cache, true), []);
}

#[test]
fn caches_beside_the_model_dir() {
    let dir = Path::new("/models/parakeet-tdt-0.6b-v3-int8");
    assert_eq!(
        cache_dir(dir),
        Path::new("/models/parakeet-tdt-0.6b-v3-int8.coreml-cache")
    );
}

/// Loads the precompiled encoder in `PARAKEET_COREML_DIR` (an unpacked
/// `encoder-model.fp16.mlmodelc`) and checks it pads and trims like the ONNX encoder. Run with
/// `--run-ignored only`.
#[cfg(all(feature = "coreml", target_os = "macos"))]
#[test]
#[ignore = "needs the 1.2 GB Core ML encoder"]
fn package_encodes_padded_input() {
    let path = std::env::var("PARAKEET_COREML_DIR").expect("set PARAKEET_COREML_DIR");
    let function = super::package::Function::load(Path::new(&path), 500).unwrap();
    let frames = 300;
    let mut padded = vec![0.0f32; crate::tdt::N_MELS * 500];
    for (mel, row) in padded.as_chunks_mut::<500>().0.iter_mut().enumerate() {
        for (t, value) in row[..frames].iter_mut().enumerate() {
            *value = ((mel * 7 + t * 13) % 17) as f32 / 8.0 - 1.0;
        }
    }
    let (encoded, dim, steps) = function.encode(&padded, frames).unwrap();
    assert_eq!((dim, steps), (1024, 38));
    assert_eq!(encoded.len(), dim * steps);
    assert!(encoded.iter().all(|value| value.is_finite()));
}
