//! Opaque, host-validated execution backends; implementation tokens stay private.

use fearless_simd::Level;

/// A supported execution backend, independent of the SIMD implementation crate.
///
/// Use the default for normal compression, or enumerate [`Self::available`] for
/// reproducible measurements and differential tests. Unsupported backends cannot
/// be constructed through this API.
///
/// # Examples
///
/// ```
/// use mbrotli::{Backend, Compressor, EncoderConfig};
/// let mut compressor = Compressor::builder(EncoderConfig::default())
///     .with_backend(Backend::SCALAR).build()?;
/// assert!(!compressor.compress(b"payload")?.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Copy, Clone)]
pub struct Backend(pub(super) Level);

impl Backend {
    /// Portable scalar implementation, without explicit SIMD kernels.
    pub const SCALAR: Self = Self(Level::fallback());

    /// Returns every distinct backend this host can execute, scalar first.
    ///
    /// Detection occurs here, never inside a compression loop.
    pub fn available() -> Vec<Self> {
        let detected = Self::default().0;
        let mut backends = vec![Self::SCALAR];
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if let Some(token) = detected.as_sse2() {
                backends.push(Self(Level::Sse2(token)));
            }
            if let Some(token) = detected.as_sse4_2() {
                backends.push(Self(Level::Sse4_2(token)));
            }
            if let Some(token) = detected.as_avx2() {
                backends.push(Self(Level::Avx2(token)));
            }
            if let Some(token) = detected.as_avx512() {
                backends.push(Self(Level::Avx512(token)));
            }
        }
        #[cfg(target_arch = "aarch64")]
        if let Some(token) = detected.as_neon() {
            backends.push(Self(Level::Neon(token)));
        }
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        if let Some(token) = detected.as_wasm_simd128() {
            backends.push(Self(Level::WasmSimd128(token)));
        }
        let _ = detected;
        backends.dedup();
        backends
    }

    /// Stable diagnostic name, available without allocating or formatting.
    pub const fn name(self) -> &'static str {
        match self.0 {
            Level::Fallback(_) => "fallback",
            #[cfg(target_arch = "aarch64")]
            Level::Neon(_) => "neon",
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Level::Sse2(_) => "sse2",
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Level::Sse4_2(_) => "sse4.2",
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Level::Avx2(_) => "avx2",
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Level::Avx512(_) => "avx512",
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            Level::WasmSimd128(_) => "wasm-simd128",
            _ => "native",
        }
    }
}

impl Default for Backend {
    fn default() -> Self {
        Self(Level::try_detect().unwrap_or_else(Level::baseline))
    }
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl PartialEq for Backend {
    fn eq(&self, other: &Self) -> bool {
        core::mem::discriminant(&self.0) == core::mem::discriminant(&other.0)
    }
}
impl Eq for Backend {}
