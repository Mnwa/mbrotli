//! Primitives shared by every RFC 9841 feature.
//!
//! [RFC 9841] adds three separable things to RFC 7932: Large Window Brotli,
//! shared dictionaries, and a framing container. The first is implemented; the
//! second exists as far as the context, its indexes and its search, but no
//! encoder consults one yet; the third is not written. What they all sit on
//! lives here, so the dictionary format and the container can never disagree
//! with each other about how a value is encoded.
//!
//! - [`window`] resolves a stream's window and writes its header.
//! - [`prefix`] turns the attached dictionaries into one logical byte
//!   sequence and maps backward distances onto it.
//! - [`prepared`] is the hash index built over one attachment.
//! - [`context`] owns the two together and searches them.
//! - [`search`] is the match finders' view of an attached context.
//!
//! [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

pub(crate) mod context;
pub(crate) mod prefix;
pub(crate) mod prepared;
pub(crate) mod search;
pub(crate) mod window;
