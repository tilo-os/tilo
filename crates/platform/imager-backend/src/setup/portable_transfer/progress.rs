//! Actual stream-byte progress, with no source names or estimated totals.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchivePhase {
    Archiving,
    VerifyingArchive,
    VerifyingSource,
    DeliveringCiphertext,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveProgress {
    pub phase: ArchivePhase,
    pub completed: u64,
    pub total: Option<u64>,
}
pub type Observer<'a> = dyn FnMut(ArchiveProgress) -> io::Result<()> + 'a;
pub(super) struct Stream<'a, 'b, T> {
    inner: T,
    current: ArchiveProgress,
    observer: &'a mut Observer<'b>,
}
impl<'a, 'b, T> Stream<'a, 'b, T> {
    pub(super) fn new(
        inner: T,
        phase: ArchivePhase,
        observer: &'a mut Observer<'b>,
    ) -> io::Result<Self> {
        let current = ArchiveProgress {
            phase,
            completed: 0,
            total: None,
        };
        observer(current)?;
        Ok(Self {
            inner,
            current,
            observer,
        })
    }
    pub(super) fn into_inner(self) -> T {
        self.inner
    }
    fn advance(&mut self, count: usize) -> io::Result<()> {
        if count != 0 {
            self.current.completed = self
                .current
                .completed
                .checked_add(count as u64)
                .ok_or_else(|| changed("Archive progress exceeds native bounds"))?;
            (self.observer)(self.current).map_err(|error| {
                if error.kind() == io::ErrorKind::Interrupted {
                    io::Error::other(error)
                } else {
                    error
                }
            })?;
        }
        Ok(())
    }
}
impl<T: Write> Write for Stream<'_, '_, T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.advance(count)?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
impl<T: Read> Read for Stream<'_, '_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.advance(count)?;
        Ok(count)
    }
}
