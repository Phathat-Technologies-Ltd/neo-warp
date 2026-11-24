//! The precompiled Core ML encoder (`encoder-model.fp16.mlmodelc`), run natively. It's a
//! multifunction ML Program with a function per input length, `frames_<n>`, sharing one set of
//! fp16 weights.

use std::cell::RefCell;
use std::path::Path;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread as _, available};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSInteger, NSNumber, NSString, NSURL};

use crate::TranscribeError;
use crate::tdt::N_MELS;

/// Whether this macOS can load multifunction models.
pub(super) fn is_supported() -> bool {
    available!(macos = 15.0)
}

/// One function of the compiled encoder, for inputs padded to `frames` feature frames.
pub(super) struct Function {
    model: Retained<MLModel>,
    frames: usize,
}

// SAFETY: Core ML models may be used from any thread; `Function` hands its model to one thread at
// a time and never shares it.
unsafe impl Send for Function {}

fn ns_error(context: &str, error: Retained<NSError>) -> TranscribeError {
    TranscribeError::Engine(format!("{context}: {}", error.localizedDescription()))
}

impl Function {
    /// Loads the `frames_<frames>` function of the compiled model at `path` on every compute unit
    /// Core ML picks from, the Neural Engine first.
    pub(super) fn load(path: &Path, frames: usize) -> Result<Self, TranscribeError> {
        let url = NSURL::from_directory_path(path).ok_or_else(|| {
            TranscribeError::Engine(format!("not a model path: {}", path.display()))
        })?;
        autoreleasepool(|_| {
            // SAFETY: plain Objective-C calls with valid, retained arguments.
            let model = unsafe {
                let config = MLModelConfiguration::new();
                config.setComputeUnits(MLComputeUnits::All);
                config.setFunctionName(Some(&NSString::from_str(&format!("frames_{frames}"))));
                MLModel::modelWithContentsOfURL_configuration_error(&url, &config)
            }
            .map_err(|error| ns_error("loading the Core ML encoder", error))?;
            Ok(Self { model, frames })
        })
    }

    /// Encodes `padded`, `[N_MELS][self.frames]` row-major features of which the first `frames`
    /// columns are real, returning the output `[dim][steps]` row-major, `dim`, and `steps`.
    pub(super) fn encode(
        &self,
        padded: &[f32],
        frames: usize,
    ) -> Result<(Vec<f32>, usize, usize), TranscribeError> {
        if padded.len() != N_MELS * self.frames {
            return Err(TranscribeError::Engine(format!(
                "expected {} features, got {}",
                N_MELS * self.frames,
                padded.len()
            )));
        }
        autoreleasepool(|_| {
            let signal = new_array(&[1, N_MELS, self.frames], MLMultiArrayDataType::Float32)?;
            write_f32(&signal, padded);
            let length = new_array(&[1], MLMultiArrayDataType::Int32)?;
            // SAFETY: `length` is a one-element Int32 array.
            unsafe { length.setObject_atIndexedSubscript(&NSNumber::new_i32(frames as i32), 0) };
            // SAFETY: plain Objective-C calls with valid, retained arguments; the dictionary
            // holds `MLFeatureValue`s keyed by `NSString`, as the provider requires.
            let output = unsafe {
                let keys = [
                    &*NSString::from_str("audio_signal"),
                    &*NSString::from_str("length"),
                ];
                let signal = MLFeatureValue::featureValueWithMultiArray(&signal);
                let length = MLFeatureValue::featureValueWithMultiArray(&length);
                let values: [&AnyObject; 2] = [&signal, &length];
                let inputs = NSDictionary::from_slices(&keys, &values);
                let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                    MLDictionaryFeatureProvider::alloc(),
                    &inputs,
                )
                .map_err(|error| ns_error("building the Core ML input", error))?;
                self.model
                    .predictionFromFeatures_error(ProtocolObject::from_ref(&*provider))
                    .map_err(|error| ns_error("running the Core ML encoder", error))?
            };
            let outputs = output_array(&output, "outputs")?;
            let lengths = output_array(&output, "encoded_lengths")?;
            // SAFETY: reads an element of an array Core ML returned.
            let valid = unsafe { lengths.objectAtIndexedSubscript(0) }.as_isize();
            read_encoded(&outputs, valid)
        })
    }
}

fn new_array(
    shape: &[usize],
    data_type: MLMultiArrayDataType,
) -> Result<Retained<MLMultiArray>, TranscribeError> {
    let shape: Vec<Retained<NSNumber>> = shape.iter().map(|&n| NSNumber::new_usize(n)).collect();
    // SAFETY: allocates a zeroed array of the given shape and type.
    unsafe {
        MLMultiArray::initWithShape_dataType_error(
            MLMultiArray::alloc(),
            &NSArray::from_retained_slice(&shape),
            data_type,
        )
    }
    .map_err(|error| ns_error("allocating a Core ML array", error))
}

fn usizes(numbers: &NSArray<NSNumber>) -> Vec<usize> {
    numbers.iter().map(|number| number.as_usize()).collect()
}

/// Copies `values`, `[N_MELS][frames]` row-major, into the `[1, N_MELS, frames]` Float32 `array`.
fn write_f32(array: &MLMultiArray, values: &[f32]) {
    // SAFETY: a plain Objective-C getter.
    let Some(&frames) = usizes(&*unsafe { array.shape() }).last() else {
        return;
    };
    let handler = RcBlock::new(
        |bytes: NonNull<std::ffi::c_void>,
         _size: NSInteger,
         array_strides: NonNull<NSArray<NSNumber>>| {
            // SAFETY: Core ML passes the array's strides, valid for the call.
            let [_, row, column] = usizes(unsafe { array_strides.as_ref() })[..] else {
                return;
            };
            let data = bytes.cast::<f32>().as_ptr();
            for (mel, values) in values.chunks_exact(frames).enumerate() {
                for (frame, value) in values.iter().enumerate() {
                    // SAFETY: the index lies inside the array by its shape and strides.
                    unsafe { *data.add(mel * row + frame * column) = *value };
                }
            }
        },
    );
    // SAFETY: the handler writes only inside the buffer, within the call.
    unsafe { array.getMutableBytesWithHandler(&handler) };
}

fn output_array(
    output: &ProtocolObject<dyn MLFeatureProvider>,
    name: &str,
) -> Result<Retained<MLMultiArray>, TranscribeError> {
    // SAFETY: plain Objective-C calls on a provider Core ML returned.
    unsafe { output.featureValueForName(&NSString::from_str(name)) }
        .and_then(|value| unsafe { value.multiArrayValue() })
        .ok_or_else(|| TranscribeError::Engine(format!("the Core ML encoder returned no {name}")))
}

/// Reads the first `valid` steps of the `[1, dim, steps]` Float32 `array` as `[dim][valid]`.
fn read_encoded(
    array: &MLMultiArray,
    valid: isize,
) -> Result<(Vec<f32>, usize, usize), TranscribeError> {
    // SAFETY: plain Objective-C getters.
    let (data_type, shape, array_strides) =
        unsafe { (array.dataType(), array.shape(), array.strides()) };
    let shape = usizes(&shape);
    let array_strides = usizes(&array_strides);
    let (&[_, dim, steps], &[_, row, column]) = (&shape[..], &array_strides[..]) else {
        return Err(TranscribeError::Engine(format!(
            "unexpected Core ML output shape {shape:?}"
        )));
    };
    if data_type != MLMultiArrayDataType::Float32 {
        return Err(TranscribeError::Engine(format!(
            "unexpected Core ML output type {data_type:?}"
        )));
    }
    let valid = (valid.max(0) as usize).min(steps);
    let encoded = RefCell::new(Vec::with_capacity(dim * valid));
    let handler = RcBlock::new(|bytes: NonNull<std::ffi::c_void>, _size: NSInteger| {
        let data = bytes.cast::<f32>().as_ptr();
        let mut encoded = encoded.borrow_mut();
        for d in 0..dim {
            // SAFETY: the index lies inside the array by its shape and strides.
            encoded.extend((0..valid).map(|t| unsafe { *data.add(d * row + t * column) }));
        }
    });
    // SAFETY: the handler reads only inside the buffer, within the call.
    unsafe { array.getBytesWithHandler(&handler) };
    drop(handler);
    Ok((encoded.into_inner(), dim, valid))
}
