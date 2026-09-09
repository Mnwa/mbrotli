//! Pinned C fixture producer. Owns encoder state; no Rust encoder participates.
use google_brotli_ffi as ffi;

pub struct Encoder<'a>(*mut ffi::BrotliEncoderState, Vec<Attached<'a>>);
struct Attached<'a> {
    _source: &'a [u8],
    prepared: *mut ffi::BrotliEncoderPreparedDictionary,
}
impl Drop for Attached<'_> {
    fn drop(&mut self) {
        // SAFETY: uniquely owned dictionary; Encoder drops its state first.
        unsafe {
            ffi::BrotliEncoderDestroyPreparedDictionary(self.prepared);
        }
    }
}
impl<'a> Encoder<'a> {
    pub fn new(parameters: &[(ffi::BrotliEncoderParameter, u32)]) -> Self {
        // SAFETY: default C allocators need no context; the owned pointer is
        // checked before use and destroyed exactly once by Drop.
        let encoder = Self(
            unsafe { ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut()) },
            Vec::new(),
        );
        assert!(!encoder.0.is_null());
        for &(parameter, value) in parameters {
            // SAFETY: live, exclusively owned encoder, still before first input.
            assert_eq!(
                unsafe { ffi::BrotliEncoderSetParameter(encoder.0, parameter, value) },
                ffi::BROTLI_TRUE,
                "generator rejection: {parameter:?}={value}"
            );
        }
        encoder
    }
    pub fn attach(&mut self, prefix: &'a [u8]) {
        self.attach_type(prefix, ffi::BROTLI_SHARED_DICTIONARY_RAW);
    }
    #[cfg(feature = "experimental")]
    pub fn attach_serialized(&mut self, bytes: &'a [u8]) {
        self.attach_type(bytes, ffi::BROTLI_SHARED_DICTIONARY_SERIALIZED);
    }
    fn attach_type(&mut self, prefix: &'a [u8], kind: ffi::BrotliSharedDictionaryType) {
        // SAFETY: prefix remains borrowed by the owned attachment until after
        // encoder destruction; all returned pointers are checked before use.
        unsafe {
            let prepared = ffi::BrotliEncoderPrepareDictionary(
                kind,
                prefix.len(),
                prefix.as_ptr(),
                11,
                None,
                None,
                std::ptr::null_mut(),
            );
            assert!(!prepared.is_null());
            let attachment = Attached {
                _source: prefix,
                prepared,
            };
            assert_eq!(
                ffi::BrotliEncoderAttachPreparedDictionary(self.0, prepared),
                ffi::BROTLI_TRUE
            );
            self.1.push(attachment);
        }
    }
    pub fn push(
        &mut self,
        input: &[u8],
        operation: ffi::BrotliEncoderOperation,
        chunk: usize,
        take: bool,
    ) -> Vec<u8> {
        let mut result = Vec::new();
        let mut remaining = input.len();
        let mut cursor = input.as_ptr();
        loop {
            let mut buffer = vec![0; chunk];
            let mut available = if take { 0 } else { buffer.len() };
            let mut output = buffer.as_mut_ptr();
            // SAFETY: cursor/remaining track the live immutable input; C writes
            // at most available bytes to this exclusive buffer. Encoder stays
            // alive throughout the call and TakeOutput's borrow is copied before
            // another encoder call can invalidate it.
            unsafe {
                assert_eq!(
                    ffi::BrotliEncoderCompressStream(
                        self.0,
                        operation,
                        &raw mut remaining,
                        &raw mut cursor,
                        &raw mut available,
                        &raw mut output,
                        std::ptr::null_mut()
                    ),
                    ffi::BROTLI_TRUE
                );
                if take {
                    let mut length = chunk;
                    let bytes = ffi::BrotliEncoderTakeOutput(self.0, &raw mut length);
                    if length != 0 {
                        result.extend_from_slice(std::slice::from_raw_parts(bytes, length));
                    }
                } else {
                    result.extend_from_slice(&buffer[..buffer.len() - available]);
                }
                if remaining == 0 && ffi::BrotliEncoderHasMoreOutput(self.0) == ffi::BROTLI_FALSE {
                    if operation == ffi::BROTLI_OPERATION_FINISH
                        && ffi::BrotliEncoderIsFinished(self.0) != ffi::BROTLI_TRUE
                    {
                        // TakeOutput can drain the last internal byte before
                        // CompressStream has committed its finished state.
                        continue;
                    }
                    break;
                }
            }
        }
        result
    }
}
impl Drop for Encoder<'_> {
    fn drop(&mut self) {
        // SAFETY: uniquely owned, live state; there are no outstanding C borrows.
        unsafe {
            ffi::BrotliEncoderDestroyInstance(self.0);
        }
    }
}
