//! Raw Rust bindings to Google's Brotli C implementation.
//!
//! The bundled C source is Brotli v1.2.0. This crate intentionally keeps the
//! pointer-based semantics of the upstream API; callers must uphold every
//! buffer validity, lifetime, and aliasing requirement documented by Google
//! Brotli's public C headers.

#![no_std]

use core::ffi::{c_char, c_int, c_uint, c_void};

#[cfg(test)]
extern crate std;

/// Upstream Brotli version vendored by this crate.
pub const UPSTREAM_VERSION: &str = "1.2.0";

pub const BROTLI_FALSE: c_int = 0;
pub const BROTLI_TRUE: c_int = 1;

pub const BROTLI_MIN_WINDOW_BITS: c_int = 10;
pub const BROTLI_MAX_WINDOW_BITS: c_int = 24;
pub const BROTLI_LARGE_MAX_WINDOW_BITS: c_int = 30;
pub const BROTLI_MIN_INPUT_BLOCK_BITS: c_int = 16;
pub const BROTLI_MAX_INPUT_BLOCK_BITS: c_int = 24;
pub const BROTLI_MIN_QUALITY: c_int = 0;
pub const BROTLI_MAX_QUALITY: c_int = 11;
pub const BROTLI_DEFAULT_QUALITY: c_int = 11;
pub const BROTLI_DEFAULT_WINDOW: c_int = 22;

pub const SHARED_BROTLI_MIN_DICTIONARY_WORD_LENGTH: usize = 4;
pub const SHARED_BROTLI_MAX_DICTIONARY_WORD_LENGTH: usize = 31;
pub const SHARED_BROTLI_NUM_DICTIONARY_CONTEXTS: usize = 64;
pub const SHARED_BROTLI_MAX_COMPOUND_DICTS: usize = 15;

#[allow(non_camel_case_types)]
pub type brotli_alloc_func =
    Option<unsafe extern "C" fn(opaque: *mut c_void, size: usize) -> *mut c_void>;

#[allow(non_camel_case_types)]
pub type brotli_free_func = Option<unsafe extern "C" fn(opaque: *mut c_void, address: *mut c_void)>;

#[allow(non_camel_case_types)]
pub type brotli_decoder_metadata_start_func =
    Option<unsafe extern "C" fn(opaque: *mut c_void, size: usize)>;

#[allow(non_camel_case_types)]
pub type brotli_decoder_metadata_chunk_func =
    Option<unsafe extern "C" fn(opaque: *mut c_void, data: *const u8, size: usize)>;

/// Result returned by decoder operations.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliDecoderResult {
    Error = 0,
    Success = 1,
    NeedsMoreInput = 2,
    NeedsMoreOutput = 3,
}

pub const BROTLI_DECODER_RESULT_ERROR: BrotliDecoderResult = BrotliDecoderResult::Error;
pub const BROTLI_DECODER_RESULT_SUCCESS: BrotliDecoderResult = BrotliDecoderResult::Success;
pub const BROTLI_DECODER_RESULT_NEEDS_MORE_INPUT: BrotliDecoderResult =
    BrotliDecoderResult::NeedsMoreInput;
pub const BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT: BrotliDecoderResult =
    BrotliDecoderResult::NeedsMoreOutput;

/// Detailed decoder status and error code.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliDecoderErrorCode {
    NoError = 0,
    Success = 1,
    NeedsMoreInput = 2,
    NeedsMoreOutput = 3,
    ErrorFormatExuberantNibble = -1,
    ErrorFormatReserved = -2,
    ErrorFormatExuberantMetaNibble = -3,
    ErrorFormatSimpleHuffmanAlphabet = -4,
    ErrorFormatSimpleHuffmanSame = -5,
    ErrorFormatClSpace = -6,
    ErrorFormatHuffmanSpace = -7,
    ErrorFormatContextMapRepeat = -8,
    ErrorFormatBlockLength1 = -9,
    ErrorFormatBlockLength2 = -10,
    ErrorFormatTransform = -11,
    ErrorFormatDictionary = -12,
    ErrorFormatWindowBits = -13,
    ErrorFormatPadding1 = -14,
    ErrorFormatPadding2 = -15,
    ErrorFormatDistance = -16,
    ErrorCompoundDictionary = -18,
    ErrorDictionaryNotSet = -19,
    ErrorInvalidArguments = -20,
    ErrorAllocContextModes = -21,
    ErrorAllocTreeGroups = -22,
    ErrorAllocContextMap = -25,
    ErrorAllocRingBuffer1 = -26,
    ErrorAllocRingBuffer2 = -27,
    ErrorAllocBlockTypeTrees = -30,
    ErrorUnreachable = -31,
}

pub const BROTLI_DECODER_NO_ERROR: BrotliDecoderErrorCode = BrotliDecoderErrorCode::NoError;
pub const BROTLI_DECODER_SUCCESS: BrotliDecoderErrorCode = BrotliDecoderErrorCode::Success;
pub const BROTLI_DECODER_NEEDS_MORE_INPUT: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::NeedsMoreInput;
pub const BROTLI_DECODER_NEEDS_MORE_OUTPUT: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::NeedsMoreOutput;
pub const BROTLI_DECODER_ERROR_FORMAT_EXUBERANT_NIBBLE: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatExuberantNibble;
pub const BROTLI_DECODER_ERROR_FORMAT_RESERVED: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatReserved;
pub const BROTLI_DECODER_ERROR_FORMAT_EXUBERANT_META_NIBBLE: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatExuberantMetaNibble;
pub const BROTLI_DECODER_ERROR_FORMAT_SIMPLE_HUFFMAN_ALPHABET: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatSimpleHuffmanAlphabet;
pub const BROTLI_DECODER_ERROR_FORMAT_SIMPLE_HUFFMAN_SAME: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatSimpleHuffmanSame;
pub const BROTLI_DECODER_ERROR_FORMAT_CL_SPACE: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatClSpace;
pub const BROTLI_DECODER_ERROR_FORMAT_HUFFMAN_SPACE: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatHuffmanSpace;
pub const BROTLI_DECODER_ERROR_FORMAT_CONTEXT_MAP_REPEAT: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatContextMapRepeat;
pub const BROTLI_DECODER_ERROR_FORMAT_BLOCK_LENGTH_1: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatBlockLength1;
pub const BROTLI_DECODER_ERROR_FORMAT_BLOCK_LENGTH_2: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatBlockLength2;
pub const BROTLI_DECODER_ERROR_FORMAT_TRANSFORM: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatTransform;
pub const BROTLI_DECODER_ERROR_FORMAT_DICTIONARY: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatDictionary;
pub const BROTLI_DECODER_ERROR_FORMAT_WINDOW_BITS: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatWindowBits;
pub const BROTLI_DECODER_ERROR_FORMAT_PADDING_1: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatPadding1;
pub const BROTLI_DECODER_ERROR_FORMAT_PADDING_2: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatPadding2;
pub const BROTLI_DECODER_ERROR_FORMAT_DISTANCE: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorFormatDistance;
pub const BROTLI_DECODER_ERROR_COMPOUND_DICTIONARY: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorCompoundDictionary;
pub const BROTLI_DECODER_ERROR_DICTIONARY_NOT_SET: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorDictionaryNotSet;
pub const BROTLI_DECODER_ERROR_INVALID_ARGUMENTS: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorInvalidArguments;
pub const BROTLI_DECODER_ERROR_ALLOC_CONTEXT_MODES: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorAllocContextModes;
pub const BROTLI_DECODER_ERROR_ALLOC_TREE_GROUPS: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorAllocTreeGroups;
pub const BROTLI_DECODER_ERROR_ALLOC_CONTEXT_MAP: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorAllocContextMap;
pub const BROTLI_DECODER_ERROR_ALLOC_RING_BUFFER_1: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorAllocRingBuffer1;
pub const BROTLI_DECODER_ERROR_ALLOC_RING_BUFFER_2: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorAllocRingBuffer2;
pub const BROTLI_DECODER_ERROR_ALLOC_BLOCK_TYPE_TREES: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorAllocBlockTypeTrees;
pub const BROTLI_DECODER_ERROR_UNREACHABLE: BrotliDecoderErrorCode =
    BrotliDecoderErrorCode::ErrorUnreachable;
pub const BROTLI_LAST_ERROR_CODE: BrotliDecoderErrorCode = BrotliDecoderErrorCode::ErrorUnreachable;

/// Decoder configuration parameter.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliDecoderParameter {
    DisableRingBufferReallocation = 0,
    LargeWindow = 1,
}

pub const BROTLI_DECODER_PARAM_DISABLE_RING_BUFFER_REALLOCATION: BrotliDecoderParameter =
    BrotliDecoderParameter::DisableRingBufferReallocation;
pub const BROTLI_DECODER_PARAM_LARGE_WINDOW: BrotliDecoderParameter =
    BrotliDecoderParameter::LargeWindow;

/// Brotli encoder operating mode.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliEncoderMode {
    Generic = 0,
    Text = 1,
    Font = 2,
}

pub const BROTLI_MODE_GENERIC: BrotliEncoderMode = BrotliEncoderMode::Generic;
pub const BROTLI_MODE_TEXT: BrotliEncoderMode = BrotliEncoderMode::Text;
pub const BROTLI_MODE_FONT: BrotliEncoderMode = BrotliEncoderMode::Font;
pub const BROTLI_DEFAULT_MODE: BrotliEncoderMode = BROTLI_MODE_GENERIC;

/// Operation requested from the streaming encoder.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliEncoderOperation {
    Process = 0,
    Flush = 1,
    Finish = 2,
    EmitMetadata = 3,
}

pub const BROTLI_OPERATION_PROCESS: BrotliEncoderOperation = BrotliEncoderOperation::Process;
pub const BROTLI_OPERATION_FLUSH: BrotliEncoderOperation = BrotliEncoderOperation::Flush;
pub const BROTLI_OPERATION_FINISH: BrotliEncoderOperation = BrotliEncoderOperation::Finish;
pub const BROTLI_OPERATION_EMIT_METADATA: BrotliEncoderOperation =
    BrotliEncoderOperation::EmitMetadata;

/// Encoder configuration parameter.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliEncoderParameter {
    Mode = 0,
    Quality = 1,
    Lgwin = 2,
    Lgblock = 3,
    DisableLiteralContextModeling = 4,
    SizeHint = 5,
    LargeWindow = 6,
    Npostfix = 7,
    Ndirect = 8,
    StreamOffset = 9,
}

pub const BROTLI_PARAM_MODE: BrotliEncoderParameter = BrotliEncoderParameter::Mode;
pub const BROTLI_PARAM_QUALITY: BrotliEncoderParameter = BrotliEncoderParameter::Quality;
pub const BROTLI_PARAM_LGWIN: BrotliEncoderParameter = BrotliEncoderParameter::Lgwin;
pub const BROTLI_PARAM_LGBLOCK: BrotliEncoderParameter = BrotliEncoderParameter::Lgblock;
pub const BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING: BrotliEncoderParameter =
    BrotliEncoderParameter::DisableLiteralContextModeling;
pub const BROTLI_PARAM_SIZE_HINT: BrotliEncoderParameter = BrotliEncoderParameter::SizeHint;
pub const BROTLI_PARAM_LARGE_WINDOW: BrotliEncoderParameter = BrotliEncoderParameter::LargeWindow;
pub const BROTLI_PARAM_NPOSTFIX: BrotliEncoderParameter = BrotliEncoderParameter::Npostfix;
pub const BROTLI_PARAM_NDIRECT: BrotliEncoderParameter = BrotliEncoderParameter::Ndirect;
pub const BROTLI_PARAM_STREAM_OFFSET: BrotliEncoderParameter = BrotliEncoderParameter::StreamOffset;

/// Format of an attached shared dictionary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrotliSharedDictionaryType {
    Raw = 0,
    Serialized = 1,
}

pub const BROTLI_SHARED_DICTIONARY_RAW: BrotliSharedDictionaryType =
    BrotliSharedDictionaryType::Raw;
pub const BROTLI_SHARED_DICTIONARY_SERIALIZED: BrotliSharedDictionaryType =
    BrotliSharedDictionaryType::Serialized;

/// Opaque decoder state owned by the C library.
#[repr(C)]
pub struct BrotliDecoderState {
    _private: [u8; 0],
}

/// Opaque encoder state owned by the C library.
#[repr(C)]
pub struct BrotliEncoderState {
    _private: [u8; 0],
}

/// Opaque prepared encoder dictionary owned by the C library.
#[repr(C)]
pub struct BrotliEncoderPreparedDictionary {
    _private: [u8; 0],
}

/// Opaque shared dictionary owned by the C library.
#[repr(C)]
pub struct BrotliSharedDictionary {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn BrotliSharedDictionaryCreateInstance(
        alloc_func: brotli_alloc_func,
        free_func: brotli_free_func,
        opaque: *mut c_void,
    ) -> *mut BrotliSharedDictionary;

    pub fn BrotliSharedDictionaryDestroyInstance(dictionary: *mut BrotliSharedDictionary);

    pub fn BrotliSharedDictionaryAttach(
        dictionary: *mut BrotliSharedDictionary,
        dictionary_type: BrotliSharedDictionaryType,
        data_size: usize,
        data: *const u8,
    ) -> c_int;

    pub fn BrotliDecoderSetParameter(
        state: *mut BrotliDecoderState,
        parameter: BrotliDecoderParameter,
        value: c_uint,
    ) -> c_int;

    pub fn BrotliDecoderAttachDictionary(
        state: *mut BrotliDecoderState,
        dictionary_type: BrotliSharedDictionaryType,
        data_size: usize,
        data: *const u8,
    ) -> c_int;

    pub fn BrotliDecoderCreateInstance(
        alloc_func: brotli_alloc_func,
        free_func: brotli_free_func,
        opaque: *mut c_void,
    ) -> *mut BrotliDecoderState;

    pub fn BrotliDecoderDestroyInstance(state: *mut BrotliDecoderState);

    pub fn BrotliDecoderDecompress(
        encoded_size: usize,
        encoded_buffer: *const u8,
        decoded_size: *mut usize,
        decoded_buffer: *mut u8,
    ) -> BrotliDecoderResult;

    pub fn BrotliDecoderDecompressStream(
        state: *mut BrotliDecoderState,
        available_in: *mut usize,
        next_in: *mut *const u8,
        available_out: *mut usize,
        next_out: *mut *mut u8,
        total_out: *mut usize,
    ) -> BrotliDecoderResult;

    pub fn BrotliDecoderHasMoreOutput(state: *const BrotliDecoderState) -> c_int;

    pub fn BrotliDecoderTakeOutput(state: *mut BrotliDecoderState, size: *mut usize) -> *const u8;

    pub fn BrotliDecoderIsUsed(state: *const BrotliDecoderState) -> c_int;

    pub fn BrotliDecoderIsFinished(state: *const BrotliDecoderState) -> c_int;

    pub fn BrotliDecoderGetErrorCode(state: *const BrotliDecoderState) -> BrotliDecoderErrorCode;

    pub fn BrotliDecoderErrorString(code: BrotliDecoderErrorCode) -> *const c_char;

    pub fn BrotliDecoderVersion() -> c_uint;

    pub fn BrotliDecoderSetMetadataCallbacks(
        state: *mut BrotliDecoderState,
        start_func: brotli_decoder_metadata_start_func,
        chunk_func: brotli_decoder_metadata_chunk_func,
        opaque: *mut c_void,
    );

    pub fn BrotliEncoderSetParameter(
        state: *mut BrotliEncoderState,
        parameter: BrotliEncoderParameter,
        value: c_uint,
    ) -> c_int;

    pub fn BrotliEncoderCreateInstance(
        alloc_func: brotli_alloc_func,
        free_func: brotli_free_func,
        opaque: *mut c_void,
    ) -> *mut BrotliEncoderState;

    pub fn BrotliEncoderDestroyInstance(state: *mut BrotliEncoderState);

    pub fn BrotliEncoderPrepareDictionary(
        dictionary_type: BrotliSharedDictionaryType,
        data_size: usize,
        data: *const u8,
        quality: c_int,
        alloc_func: brotli_alloc_func,
        free_func: brotli_free_func,
        opaque: *mut c_void,
    ) -> *mut BrotliEncoderPreparedDictionary;

    pub fn BrotliEncoderDestroyPreparedDictionary(dictionary: *mut BrotliEncoderPreparedDictionary);

    pub fn BrotliEncoderAttachPreparedDictionary(
        state: *mut BrotliEncoderState,
        dictionary: *const BrotliEncoderPreparedDictionary,
    ) -> c_int;

    pub fn BrotliEncoderMaxCompressedSize(input_size: usize) -> usize;

    pub fn BrotliEncoderCompress(
        quality: c_int,
        lgwin: c_int,
        mode: BrotliEncoderMode,
        input_size: usize,
        input_buffer: *const u8,
        encoded_size: *mut usize,
        encoded_buffer: *mut u8,
    ) -> c_int;

    pub fn BrotliEncoderCompressStream(
        state: *mut BrotliEncoderState,
        operation: BrotliEncoderOperation,
        available_in: *mut usize,
        next_in: *mut *const u8,
        available_out: *mut usize,
        next_out: *mut *mut u8,
        total_out: *mut usize,
    ) -> c_int;

    pub fn BrotliEncoderIsFinished(state: *mut BrotliEncoderState) -> c_int;

    pub fn BrotliEncoderHasMoreOutput(state: *mut BrotliEncoderState) -> c_int;

    pub fn BrotliEncoderTakeOutput(state: *mut BrotliEncoderState, size: *mut usize) -> *const u8;

    pub fn BrotliEncoderEstimatePeakMemoryUsage(
        quality: c_int,
        lgwin: c_int,
        input_size: usize,
    ) -> usize;

    pub fn BrotliEncoderGetPreparedDictionarySize(
        dictionary: *const BrotliEncoderPreparedDictionary,
    ) -> usize;

    pub fn BrotliEncoderVersion() -> c_uint;
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr;
    use std::vec;

    #[test]
    fn google_brotli_round_trip() {
        let input = b"Google Brotli through Rust FFI. Google Brotli through Rust FFI.";
        let mut compressed = vec![0; unsafe { BrotliEncoderMaxCompressedSize(input.len()) }];
        let mut compressed_len = compressed.len();

        let encoded = unsafe {
            BrotliEncoderCompress(
                6,
                BROTLI_DEFAULT_WINDOW,
                BROTLI_DEFAULT_MODE,
                input.len(),
                input.as_ptr(),
                &mut compressed_len,
                compressed.as_mut_ptr(),
            )
        };
        assert_eq!(encoded, BROTLI_TRUE);

        let mut decoded = vec![0; input.len()];
        let mut decoded_len = decoded.len();
        let result = unsafe {
            BrotliDecoderDecompress(
                compressed_len,
                compressed.as_ptr(),
                &mut decoded_len,
                decoded.as_mut_ptr(),
            )
        };

        assert_eq!(result, BROTLI_DECODER_RESULT_SUCCESS);
        assert_eq!(&decoded[..decoded_len], input);
    }

    #[test]
    fn creates_streaming_states() {
        let encoder = unsafe { BrotliEncoderCreateInstance(None, None, ptr::null_mut()) };
        let decoder = unsafe { BrotliDecoderCreateInstance(None, None, ptr::null_mut()) };

        assert!(!encoder.is_null());
        assert!(!decoder.is_null());

        unsafe {
            BrotliEncoderDestroyInstance(encoder);
            BrotliDecoderDestroyInstance(decoder);
        }
    }

    #[test]
    fn reports_vendored_version() {
        let expected = (1_u32 << 24) | (2 << 12);
        assert_eq!(unsafe { BrotliEncoderVersion() }, expected);
        assert_eq!(unsafe { BrotliDecoderVersion() }, expected);
    }
}
