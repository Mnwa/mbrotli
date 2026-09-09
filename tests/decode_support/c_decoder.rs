use google_brotli_ffi as ffi;
pub struct Decoder(*mut ffi::BrotliDecoderState);
impl Default for Decoder {
    fn default() -> Self {
        // SAFETY: default allocators need no context. Ownership lasts until Drop.
        unsafe {
            let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
            assert!(!state.is_null());
            assert_eq!(
                ffi::BrotliDecoderSetParameter(state, ffi::BROTLI_DECODER_PARAM_LARGE_WINDOW, 1),
                ffi::BROTLI_TRUE
            );
            Self(state)
        }
    }
}
impl Decoder {
    pub fn process(&mut self, input: &[u8], output: &mut [u8]) -> (usize, usize, bool) {
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        // SAFETY: slice buffers stay live and disjoint for the call; C advances
        // cursor/length pairs within them. The decoder is exclusively borrowed.
        let result = unsafe {
            ffi::BrotliDecoderDecompressStream(
                self.0,
                &raw mut available_in,
                &raw mut next_in,
                &raw mut available_out,
                &raw mut next_out,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(result, ffi::BROTLI_DECODER_RESULT_ERROR);
        (
            input.len() - available_in,
            output.len() - available_out,
            result == ffi::BROTLI_DECODER_RESULT_SUCCESS,
        )
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: exclusively owned live state with no outstanding C borrows.
        unsafe {
            ffi::BrotliDecoderDestroyInstance(self.0);
        }
    }
}
