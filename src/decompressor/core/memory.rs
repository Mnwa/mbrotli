use super::super::DecodeError;
use alloc::vec::Vec;

#[derive(Debug, Default)]
pub(super) struct Memory {
    pub(super) live: usize,
    pub(super) limit: Option<usize>,
}

impl Memory {
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(super) fn resize<T: Default + Clone>(
        &mut self,
        buffer: &mut Vec<T>,
        length: usize,
    ) -> Result<(), DecodeError> {
        if length > buffer.capacity() {
            // An allocator may implement realloc as allocate/copy/free. The
            // old buffer is still live when the replacement is requested.
            let replacement = length
                .checked_mul(size_of::<T>())
                .ok_or(DecodeError::SizeOverflow)?;
            let next = self
                .live
                .checked_add(replacement)
                .ok_or(DecodeError::SizeOverflow)?;
            if let Some(limit) = self.limit
                && next > limit
            {
                return Err(DecodeError::MemoryLimitExceeded { limit });
            }
            let before = buffer.capacity();
            buffer
                .try_reserve_exact(length - buffer.len())
                .map_err(|_| DecodeError::AllocationFailed)?;
            self.live += (buffer.capacity() - before) * size_of::<T>();
        }
        buffer.resize(length, T::default());
        Ok(())
    }
}
