//! Logging that can never take the compositor down.
//!
//! The compositor's stderr is the session's flight recorder (a pipe). Rust's
//! `eprintln!` panics when a write to stderr fails, and tracing's formatter
//! reports its own failed writes with `eprintln!` too. A recorder that dies
//! (for example on a full disk) closes that pipe, and the next log line would
//! then panic outside any helper boundary and end the session.
//!
//! [`install`] puts a private pipe in front of fd 2. A relay thread drains it
//! into the original stderr and drops whatever the original refuses, so a
//! write to fd 2 only fails if this process itself is broken.

use std::io::{ErrorKind, Read, Write};

/// Route fd 2 through the relay. Leaves stderr untouched if any step fails.
pub fn install() {
    let Ok(original) = rustix::io::dup(std::io::stderr()) else {
        return;
    };
    let Ok((reader, writer)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) else {
        return;
    };
    // The reader must exist before fd 2 points at the pipe; otherwise the
    // first log line would meet a pipe with no reader.
    let relay = std::thread::Builder::new()
        .name("stderr-relay".into())
        .spawn(move || relay(std::fs::File::from(reader), std::fs::File::from(original)));
    if relay.is_err() {
        return;
    }
    // On failure fd 2 stays as it was and the relay ends when `writer` drops.
    let _ = rustix::stdio::dup2_stderr(&writer);
}

fn relay(mut from: std::fs::File, mut to: std::fs::File) {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => {
                // A refused write (closed recorder, full disk) drops these
                // bytes; the next ones are tried again.
                let _ = to.write_all(buffer.get(..read).unwrap_or_default());
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::relay;
    use std::io::{Read, Write};

    #[test]
    fn a_closed_destination_never_stops_the_relay_from_draining() {
        let (source_reader, source_writer) = rustix::pipe::pipe().expect("source pipe");
        let (sink_reader, sink_writer) = rustix::pipe::pipe().expect("sink pipe");
        drop(sink_reader); // every forward now fails with EPIPE
        let thread = std::thread::spawn(move || {
            relay(
                std::fs::File::from(source_reader),
                std::fs::File::from(sink_writer),
            );
        });
        let mut source = std::fs::File::from(source_writer);
        // Far more than a pipe buffer: a relay that stopped draining would block here.
        for _ in 0..64 {
            source
                .write_all(&[b'x'; 16 * 1024])
                .expect("writes keep succeeding");
        }
        drop(source);
        thread.join().expect("relay ends cleanly at end of input");
    }

    #[test]
    fn output_reaches_a_working_destination_in_order() {
        let (source_reader, source_writer) = rustix::pipe::pipe().expect("source pipe");
        let (sink_reader, sink_writer) = rustix::pipe::pipe().expect("sink pipe");
        let thread = std::thread::spawn(move || {
            relay(
                std::fs::File::from(source_reader),
                std::fs::File::from(sink_writer),
            );
        });
        let mut source = std::fs::File::from(source_writer);
        source
            .write_all(b"first line\nsecond line\n")
            .expect("write");
        drop(source);
        thread.join().expect("relay ends");
        let mut received = String::new();
        std::fs::File::from(sink_reader)
            .read_to_string(&mut received)
            .expect("read");
        assert_eq!(received, "first line\nsecond line\n");
    }
}
