//! Wire primitives shared by every RFC 9841 feature.
//!
//! [RFC 9841] adds three separable things to RFC 7932: Large Window Brotli,
//! shared dictionaries, and a framing container. The first is implemented; the
//! other two are not written yet. What they all sit on lives here, so the
//! dictionary format and the container can never disagree with each other about
//! how a value is encoded.
//!
//! [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

pub(crate) mod window;
