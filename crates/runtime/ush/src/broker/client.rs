//! The guest's half of the broker's protocol: one request, and the answer,
//! with a host command's output streamed as it comes.

use std::io::{BufRead as _, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::{Request, Response, SocketAddress, WireFrame};

/// Ask the broker one question, from inside a guest. A host command's output
/// is discarded; use [`ask_streaming`] to see it.
///
/// # Errors
///
/// Returns an error if the socket can't be reached, the request can't be
/// serialized, the write/read fails, or the reply isn't valid JSON.
pub fn ask(socket: &Path, req: &Request) -> std::io::Result<Response> {
    ask_streaming(socket, req, &mut std::io::sink(), &mut std::io::sink())
}

/// Ask the broker while copying host-command output to the supplied streams as
/// soon as it is produced. The output is not repeated in the response, which
/// carries only the exit status.
///
/// # Errors
///
/// Returns an error when the broker cannot be reached, the socket protocol is
/// invalid, or streamed output cannot be written.
pub fn ask_streaming(
    socket: &Path,
    req: &Request,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> std::io::Result<Response> {
    ask_streaming_with_progress(socket, req, stdout, stderr, false)
}

/// Streaming variant which also renders a sparse liveness message on stderr
/// while a host command is silent. Callers should enable this only for a TTY.
///
/// # Errors
///
/// Returns an error when the broker cannot be reached, the socket protocol is
/// invalid, or streamed output cannot be written.
pub fn ask_streaming_with_progress(
    socket: &Path,
    req: &Request,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    show_progress: bool,
) -> std::io::Result<Response> {
    let address = SocketAddress::new(socket)?;
    let stream = UnixStream::connect(&address.path)?;
    let mut writer = stream.try_clone()?;
    let mut bytes = serde_json::to_vec(req)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "broker closed the connection before replying",
            ));
        }
        // Non-command requests retain the original one-line wire format.
        if let Ok(response) = serde_json::from_str::<Response>(&line) {
            return Ok(response);
        }
        match serde_json::from_str::<WireFrame>(&line)? {
            WireFrame::Output { stderr: true, data } => {
                stderr.write_all(&data)?;
                stderr.flush()?;
            }
            WireFrame::Output {
                stderr: false,
                data,
            } => {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            WireFrame::Heartbeat => {
                if show_progress {
                    stderr.write_all(b"Still working...\n")?;
                    stderr.flush()?;
                }
            }
            WireFrame::Complete { response } => return Ok(response),
        }
    }
}
