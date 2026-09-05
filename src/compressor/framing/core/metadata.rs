//! Bounded, self-contained metadata streams; no borrowed dictionary is retained.

use super::number;
use crate::compressor::framing::{FramingError, MetadataEncoding, MetadataField};
use crate::compressor::{Compressor, WindowEncoding};

pub(super) fn serialize(
    fields: &[MetadataField<'_>],
    codes: Option<&[[u8; 2]]>,
    capacity: usize,
) -> Result<Vec<u8>, FramingError> {
    let mut content = Vec::with_capacity(capacity);
    for field in fields {
        if codes.is_none_or(|codes| codes.contains(&field.code)) {
            content.extend_from_slice(&field.code);
            number(field.value.len() as u64, &mut content)?;
            content.extend_from_slice(field.value);
        }
    }
    Ok(content)
}

pub(super) fn encode(
    compressor: &mut Compressor,
    kind: u8,
    content: Vec<u8>,
    encoding: MetadataEncoding<'_>,
    references: Vec<u8>,
) -> Result<(Vec<u8>, Vec<u8>), FramingError> {
    let mut header = vec![kind];
    if matches!(encoding, MetadataEncoding::Uncompressed) {
        header.push(0);
        return Ok((header, content));
    }
    let shared = matches!(encoding, MetadataEncoding::Shared { .. })
        || compressor.config().window().encoding() == WindowEncoding::Large;
    header.push(if shared { 3 } else { 2 });
    number(content.len() as u64, &mut header)?;
    if shared {
        if references.is_empty() {
            header.push(0);
        } else {
            header.extend_from_slice(&references);
        }
    }
    let bound =
        Compressor::max_compressed_size(content.len()).map_err(|_| FramingError::Overflow)?;
    let mut encoded = vec![0; bound];
    let written = match encoding {
        MetadataEncoding::Shared { dictionary, .. } => {
            compressor.compress_with_dictionary_to_slice(dictionary, &content, &mut encoded)?
        }
        _ => compressor.compress_to_slice(&content, &mut encoded)?,
    };
    encoded.truncate(written);
    Ok((header, encoded))
}
