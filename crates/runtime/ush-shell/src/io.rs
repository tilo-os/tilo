//! What a command reads from and writes to.
//!
//! A command's standard streams are [`Input`] and [`Output`]: shared handles
//! on a reader or a writer, cheap to clone, so `2>&1` makes standard error the
//! same writer as standard output. A stream that is one of the host's own
//! ([`Std`]) says so, so a program the host runs can be handed the host's own
//! descriptor (an editor needs the terminal, not a pipe).
//!
//! A pipe between two commands of the shell's own is [`pipe`]: bytes in
//! memory, bounded, with the end-of-file and broken-pipe rules of a real one,
//! so `cat big | head -1` stops `cat`.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// The host's own standard streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Std {
    In,
    Out,
    Err,
}

type Reader = Box<dyn Read + Send>;
type Writer = Box<dyn Write + Send>;

fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A command's input: a shared reader.
#[derive(Clone)]
pub struct Input {
    reader: Arc<Mutex<Reader>>,
    host: Option<Std>,
}

impl Input {
    pub fn new(reader: impl Read + Send + 'static) -> Self {
        Self {
            reader: Arc::new(Mutex::new(Box::new(reader))),
            host: None,
        }
    }

    /// The host's own standard input, read through `reader`.
    pub fn host(reader: impl Read + Send + 'static) -> Self {
        Self {
            reader: Arc::new(Mutex::new(Box::new(reader))),
            host: Some(Std::In),
        }
    }

    /// Nothing: end of file at once.
    pub fn null() -> Self {
        Self::new(io::empty())
    }

    pub fn bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::new(io::Cursor::new(bytes.into()))
    }

    /// Which of the host's own streams this is, if it is one.
    pub const fn host_stream(&self) -> Option<Std> {
        self.host
    }

    /// Read everything that is left.
    pub fn read_all(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        lock(&self.reader).read_to_end(&mut out)?;
        Ok(out)
    }

    /// Read one line, without its terminator, a byte at a time so nothing
    /// past the line is taken from a stream another command reads next.
    /// `None` at end of file with nothing read.
    pub fn read_line_until(&self, delimiter: u8) -> io::Result<Option<Vec<u8>>> {
        let mut reader = lock(&self.reader);
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            match reader.read(&mut byte) {
                Ok(0) => return Ok((!line.is_empty()).then_some(line)),
                Ok(_) => {
                    if byte[0] == delimiter {
                        return Ok(Some(line));
                    }
                    line.push(byte[0]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Read up to `buf.len()` bytes.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        lock(&self.reader).read(buf)
    }
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Input")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// A command's output: a shared writer.
#[derive(Clone)]
pub struct Output {
    writer: Arc<Mutex<Writer>>,
    host: Option<Std>,
}

impl Output {
    pub fn new(writer: impl Write + Send + 'static) -> Self {
        Self {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            host: None,
        }
    }

    /// One of the host's own output streams, written through `writer`.
    pub fn host(stream: Std, writer: impl Write + Send + 'static) -> Self {
        Self {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            host: Some(stream),
        }
    }

    /// Discards everything.
    pub fn null() -> Self {
        Self::new(io::sink())
    }

    /// A buffer in memory, and the handle that reads it back.
    pub fn buffer() -> (Self, Captured) {
        let shared = Captured(Arc::new(Mutex::new(Vec::new())));
        (Self::new(shared.clone()), shared)
    }

    pub const fn host_stream(&self) -> Option<Std> {
        self.host
    }

    pub fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut writer = lock(&self.writer);
        writer.write_all(bytes)?;
        writer.flush()
    }

    pub fn write_str(&self, text: &str) -> io::Result<()> {
        self.write_all(text.as_bytes())
    }

    /// Whether both handles write to the same place.
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.writer, &other.writer)
    }
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// What an [`Output::buffer`] collected.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *lock(&self.0))
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        lock(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The three standard streams of one command.
#[derive(Clone, Debug)]
pub struct Stdio {
    pub stdin: Input,
    pub stdout: Output,
    pub stderr: Output,
}

impl Stdio {
    /// No input, and both outputs discarded.
    pub fn null() -> Self {
        Self {
            stdin: Input::null(),
            stdout: Output::null(),
            stderr: Output::null(),
        }
    }
}

/// How many bytes a pipe holds before its writer waits for the reader.
const PIPE_CAPACITY: usize = 64 * 1024;

struct PipeState {
    bytes: VecDeque<u8>,
    writer_open: bool,
    reader_open: bool,
}

struct Pipe {
    state: Mutex<PipeState>,
    changed: Condvar,
}

impl Pipe {
    fn state(&self) -> MutexGuard<'_, PipeState> {
        lock(&self.state)
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, PipeState>) -> MutexGuard<'a, PipeState> {
        self.changed
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The reading end of a [`pipe`].
pub struct PipeReader(Arc<Pipe>);

/// The writing end of a [`pipe`].
pub struct PipeWriter(Arc<Pipe>);

/// A pipe in memory between two commands: what one writes, the other reads,
/// in order. The reader sees end of file once the writer is gone; the writer
/// gets [`io::ErrorKind::BrokenPipe`] once the reader is.
pub fn pipe() -> (PipeReader, PipeWriter) {
    let pipe = Arc::new(Pipe {
        state: Mutex::new(PipeState {
            bytes: VecDeque::new(),
            writer_open: true,
            reader_open: true,
        }),
        changed: Condvar::new(),
    });
    (PipeReader(Arc::clone(&pipe)), PipeWriter(pipe))
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut state = self.0.state();
        loop {
            if !state.bytes.is_empty() {
                let n = buf.len().min(state.bytes.len());
                for (slot, byte) in buf.iter_mut().zip(state.bytes.drain(..n)) {
                    *slot = byte;
                }
                drop(state);
                self.0.changed.notify_all();
                return Ok(n);
            }
            if !state.writer_open {
                return Ok(0);
            }
            state = self.0.wait(state);
        }
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.reader_open = false;
        state.bytes.clear();
        drop(state);
        self.0.changed.notify_all();
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut state = self.0.state();
        loop {
            if !state.reader_open {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let room = PIPE_CAPACITY.saturating_sub(state.bytes.len());
            if room > 0 {
                let n = room.min(buf.len());
                state.bytes.extend(buf.iter().take(n));
                drop(state);
                self.0.changed.notify_all();
                return Ok(n);
            }
            state = self.0.wait(state);
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.writer_open = false;
        drop(state);
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_ends_when_its_writer_is_gone() {
        let (mut reader, mut writer) = pipe();
        let feeder = std::thread::spawn(move || {
            for _ in 0..1000 {
                writer.write_all(&[b'x'; 1000]).unwrap();
            }
        });
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        feeder.join().unwrap();
        assert_eq!(all.len(), 1_000_000);
    }

    #[test]
    fn a_writer_learns_its_reader_left() {
        let (reader, mut writer) = pipe();
        drop(reader);
        let error = writer.write_all(b"x").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
