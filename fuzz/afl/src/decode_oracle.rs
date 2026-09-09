//! Independent C outcome classification; resource limits are not format errors.
use google_brotli_ffi as ffi;

#[derive(Debug)]
pub(crate) enum Outcome {
    Success { payload: Vec<u8>, consumed: usize },
    Invalid,
    NeedsInput,
    OutputBudget,
    AllocationFailure,
    AttachmentRejected,
    UnsupportedWindow,
}

pub(crate) fn decode(
    input: &[u8],
    capacity: usize,
    attachment: Option<(ffi::BrotliSharedDictionaryType, &[u8])>,
) -> Outcome {
    if input.first() == Some(&0x11)
        && input
            .get(1)
            .is_some_and(|byte| (31..=62).contains(&(byte & 63)))
    {
        return Outcome::UnsupportedWindow;
    }
    let mut output = vec![0; capacity];
    // SAFETY: all caller bytes and exclusive output stay live through decoder
    // destruction. Cursor/length pairs describe exactly their slice bounds.
    // The C state is checked before use and released on every exit below.
    unsafe {
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        if state.is_null() {
            return Outcome::AllocationFailure;
        }
        assert_eq!(
            ffi::BrotliDecoderSetParameter(state, ffi::BROTLI_DECODER_PARAM_LARGE_WINDOW, 1),
            ffi::BROTLI_TRUE
        );
        if let Some((kind, source)) = attachment
            && ffi::BrotliDecoderAttachDictionary(state, kind, source.len(), source.as_ptr())
                != ffi::BROTLI_TRUE
        {
            ffi::BrotliDecoderDestroyInstance(state);
            return Outcome::AttachmentRejected;
        }
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let result = ffi::BrotliDecoderDecompressStream(
            state,
            &raw mut available_in,
            &raw mut next_in,
            &raw mut available_out,
            &raw mut next_out,
            std::ptr::null_mut(),
        );
        let error = ffi::BrotliDecoderGetErrorCode(state) as i32;
        ffi::BrotliDecoderDestroyInstance(state);
        match result {
            ffi::BrotliDecoderResult::Success => {
                output.truncate(output.len() - available_out);
                Outcome::Success {
                    payload: output,
                    consumed: input.len() - available_in,
                }
            }
            ffi::BrotliDecoderResult::NeedsMoreInput => Outcome::NeedsInput,
            ffi::BrotliDecoderResult::NeedsMoreOutput => Outcome::OutputBudget,
            ffi::BrotliDecoderResult::Error if (-30..=-21).contains(&error) => {
                Outcome::AllocationFailure
            }
            ffi::BrotliDecoderResult::Error => Outcome::Invalid,
        }
    }
}
