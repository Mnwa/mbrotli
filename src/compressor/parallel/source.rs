//! Immutable borrowed/owned bytes and positional regular-file access.
use std::{
    fs::{File, Metadata},
    io,
    path::Path,
    sync::Arc,
    time::SystemTime,
};

/// Caller-defined metadata token. Equality is checked before output mutation.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SourceIdentity(Vec<u8>);
impl From<Vec<u8>> for SourceIdentity {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// Owned random-access input shared by detached tasks.
/// Implementations must fill each requested range or return an error, and keep
/// bytes immutable for the batch lifetime. A blocking implementation can block
/// its executor thread; the library cannot forcibly interrupt that call.
pub trait RandomAccessSource: Send + Sync + 'static {
    /// Current byte length.
    /// # Errors
    /// Returns an I/O error when metadata is unavailable.
    fn len(&self) -> io::Result<u64>;
    /// Whether the current length is zero.
    /// # Errors
    /// Propagates `len` failures.
    fn is_empty(&self) -> io::Result<bool> {
        self.len().map(|n| n == 0)
    }
    /// Fills exactly `dst` from an absolute offset, without a shared cursor.
    /// # Errors
    /// Returns an error for an unavailable or truncated range.
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()>;
    /// Current identity/version token, if available. Metadata alone cannot prove
    /// immutability against every in-place mutation on every filesystem.
    fn identity(&self) -> Option<SourceIdentity> {
        None
    }
}
/// Immutable reference-counted bytes for detached tasks.
#[derive(Clone, Debug)]
pub struct ArcBytesSource(Arc<[u8]>);
impl From<Arc<[u8]>> for ArcBytesSource {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self(bytes)
    }
}
impl AsRef<[u8]> for ArcBytesSource {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
impl RandomAccessSource for ArcBytesSource {
    fn len(&self) -> io::Result<u64> {
        Ok(self.0.len() as u64)
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        let offset = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let end = offset
            .checked_add(dst.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        let bytes = self
            .0
            .get(offset..end)
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        dst.copy_from_slice(bytes);
        Ok(())
    }
}
/// Stable open regular-file handle; positional reads never modify its cursor.
#[derive(Debug)]
pub struct FileSource {
    file: File,
}
impl FileSource {
    /// Opens a regular file for concurrent read-only positional access.
    /// # Errors
    /// Propagates open/metadata errors and rejects non-regular files.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::try_from(File::open(path)?)
    }
}
impl TryFrom<File> for FileSource {
    type Error = io::Error;
    /// Takes ownership of a regular file handle.
    /// # Errors
    /// Rejects non-regular files and unavailable metadata.
    fn try_from(file: File) -> io::Result<Self> {
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "parallel input must be a regular file",
            ));
        }
        Ok(Self { file })
    }
}
impl RandomAccessSource for FileSource {
    fn len(&self) -> io::Result<u64> {
        self.file.metadata().map(|m| m.len())
    }
    fn read_exact_at(&self, mut offset: u64, mut dst: &mut [u8]) -> io::Result<()> {
        while !dst.is_empty() {
            #[cfg(unix)]
            let result = {
                use std::os::unix::fs::FileExt;
                self.file.read_at(dst, offset)
            };
            #[cfg(windows)]
            let result = {
                use std::os::windows::fs::FileExt;
                self.file.seek_read(dst, offset)
            };
            #[cfg(not(any(unix, windows)))]
            let result: io::Result<usize> = Err(io::ErrorKind::Unsupported.into());
            match result {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    offset = offset
                        .checked_add(n as u64)
                        .ok_or(io::ErrorKind::InvalidInput)?;
                    dst = &mut dst[n..];
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn identity(&self) -> Option<SourceIdentity> {
        self.file.metadata().ok().map(metadata_identity)
    }
}
fn metadata_identity(metadata: Metadata) -> SourceIdentity {
    let mut bytes = metadata.len().to_le_bytes().to_vec();
    if let Ok(time) = metadata.modified().and_then(|t| {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(io::Error::other)
    }) {
        bytes.extend_from_slice(&time.as_nanos().to_le_bytes());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        bytes.extend_from_slice(&metadata.dev().to_le_bytes());
        bytes.extend_from_slice(&metadata.ino().to_le_bytes());
        bytes.extend_from_slice(&metadata.ctime().to_le_bytes());
        bytes.extend_from_slice(&metadata.ctime_nsec().to_le_bytes());
    }
    SourceIdentity(bytes)
}
