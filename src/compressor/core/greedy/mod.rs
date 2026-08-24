//! Quality 3, 4 and 5 encoders: greedy backward references and meta-blocks.

pub(crate) mod backward_references;
pub(crate) mod bitstream;
pub(crate) mod command;
pub(crate) mod context_model;
pub(crate) mod dictionary;
pub(crate) mod encoder;
pub(crate) mod hashers;
pub(crate) mod histogram;
pub(crate) mod metablock;
pub(crate) mod params;
pub(crate) mod ringbuffer;
pub(crate) mod score;
pub(crate) mod split;
pub(crate) mod tables;
