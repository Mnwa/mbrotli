//! Host-validated tokens are selected once and retained with the encoder.
//!
//! Dynamic calls stop at this boundary. Each selected implementation enters a
//! feature-enabled function once and passes its concrete token to inner loops.

use fearless_simd::{Level, Simd, dispatch};

use super::fast::{FastCore, encode_fragment};
use super::greedy::backward_references::{ReferenceState, create_backward_references};
use super::greedy::hashers::{MatchFinder, with_matcher};
use super::greedy::params::GreedyParams;
use super::hq::h10::BinaryTreeMatcher;
use super::hq::params::{HqParams, HqQuality};
use super::hq::zopfli::{
    ZopfliState, ZopfliWorkspace, create_hq_zopfli_backward_references,
    create_zopfli_backward_references,
};
use super::rfc9841::context::SharedContextInner;
use super::shared::bits::BitWriter;
use super::shared::command::Command;
use super::shared::ringbuffer::{BlockSpan, Window};

/// Borrowed greedy state for one monomorphized scan.
pub(crate) struct GreedyInput<'a> {
    pub(crate) matcher: &'a mut MatchFinder,
    pub(crate) params: &'a GreedyParams,
    pub(crate) window: Window<'a>,
    pub(crate) span: BlockSpan,
    pub(crate) attached: Option<&'a SharedContextInner>,
    pub(crate) references: &'a mut ReferenceState,
    pub(crate) commands: &'a mut Vec<Command>,
}

/// Borrowed high-quality state for one monomorphized search.
pub(crate) struct HqInput<'a> {
    pub(crate) matcher: &'a mut BinaryTreeMatcher,
    pub(crate) params: &'a HqParams,
    pub(crate) window: Window<'a>,
    pub(crate) span: BlockSpan,
    pub(crate) attached: Option<&'a SharedContextInner>,
    pub(crate) references: &'a mut ZopfliState,
    pub(crate) workspace: &'a mut ZopfliWorkspace,
    pub(crate) commands: &'a mut Vec<Command>,
}

/// Type-erased outer boundary; no feature detection is performed by its calls.
pub(crate) trait Kernels: Send + Sync {
    fn fast(
        &self,
        core: &mut FastCore,
        input: &[u8],
        is_last: bool,
        table: &mut [i32],
        writer: &mut BitWriter<'_>,
    );
    fn greedy(&self, input: GreedyInput<'_>);
    fn hq(&self, input: HqInput<'_>);
    fn stitch(
        &self,
        matcher: &mut BinaryTreeMatcher,
        input_size: usize,
        position: usize,
        window: Window<'_>,
    );
}

/// The boxed proof tokens are zero-sized for all currently supported backends.
struct Selected<S>(S);

/// Resolves the backend once, when a retained encoder is constructed.
pub(crate) fn select(level: Level) -> Box<dyn Kernels> {
    dispatch!(level, simd => Box::new(Selected(simd)) as Box<dyn Kernels>)
}

impl<S: Simd> Kernels for Selected<S> {
    fn fast(
        &self,
        core: &mut FastCore,
        input: &[u8],
        is_last: bool,
        table: &mut [i32],
        writer: &mut BitWriter<'_>,
    ) {
        self.0.vectorize(
            #[inline(always)]
            || encode_fragment(self.0, core, input, is_last, table, writer),
        );
    }

    fn greedy(&self, input: GreedyInput<'_>) {
        let GreedyInput {
            matcher,
            params,
            window,
            span,
            attached,
            references,
            commands,
        } = input;
        self.0.vectorize(
            #[inline(always)]
            || match attached {
                None => with_matcher!(matcher, |finder| create_backward_references::<_, _, false>(
                    self.0, finder, params, window, span, None, references, commands
                )),
                Some(_) => {
                    with_matcher!(matcher, |finder| create_backward_references::<_, _, true>(
                        self.0, finder, params, window, span, attached, references, commands
                    ))
                }
            },
        );
    }

    fn hq(&self, input: HqInput<'_>) {
        let HqInput {
            matcher,
            params,
            window,
            span,
            attached,
            references,
            workspace,
            commands,
        } = input;
        self.0.vectorize(
            #[inline(always)]
            || match params.quality {
                HqQuality::Q10 => create_zopfli_backward_references(
                    self.0,
                    span.bytes as usize,
                    span.position as usize,
                    window.data,
                    window.mask,
                    params,
                    attached,
                    matcher,
                    workspace,
                    references,
                    commands,
                ),
                HqQuality::Q11 => create_hq_zopfli_backward_references(
                    self.0,
                    span.bytes as usize,
                    span.position as usize,
                    window.data,
                    window.mask,
                    params,
                    attached,
                    matcher,
                    workspace,
                    references,
                    commands,
                ),
            },
        );
    }

    fn stitch(
        &self,
        matcher: &mut BinaryTreeMatcher,
        input_size: usize,
        position: usize,
        window: Window<'_>,
    ) {
        self.0.vectorize(
            #[inline(always)]
            || {
                matcher.stitch_to_previous_block(
                    self.0,
                    input_size,
                    position,
                    window.data,
                    window.mask,
                )
            },
        );
    }
}
