use std::sync::Mutex;

use parakeet_rs::ort::environment::Environment;

use crate::TranscribeError;

/// The process's ONNX Runtime environment, shared by every engine because ONNX Runtime allows only
/// one per process.
pub(crate) fn environment() -> Result<Environment, TranscribeError> {
    static ENVIRONMENT: Mutex<Option<Environment>> = Mutex::new(None);
    let mut environment = ENVIRONMENT
        .lock()
        .map_err(|_| TranscribeError::Engine("the ONNX Runtime lock is poisoned".to_owned()))?;
    if let Some(environment) = environment.as_ref() {
        return Ok(environment.clone());
    }
    let built = parakeet_rs::ort::init()
        .with_name("local_transcription")
        .build()
        .map_err(|error| TranscribeError::Engine(error.to_string()))?;
    *environment = Some(built.clone());
    Ok(built)
}
