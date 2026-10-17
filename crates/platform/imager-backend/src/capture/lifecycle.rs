//! Owned helper lifetime, bounded status framing and revocation. No snapshot
//! descriptors or paths are exposed through this interface.
use super::{State, Status, failure};
use std::{
    io::{self, BufRead, Write},
    process::{Child, ChildStdin},
    sync::mpsc,
};

/// An already elevated helper and its private request directory. Dropping the
/// handle revokes the helper's control pipe and reaps it before releasing custody.
pub struct Capture {
    child: Child,
    input: Option<ChildStdin>,
    events: mpsc::Receiver<io::Result<Status>>,
    operation: String,
    terminal: bool,
    _request: tempfile::TempDir,
}

impl Capture {
    /// Adopt the canonical launcher's child. This does not certify the RPC peer;
    /// the authenticated transport must independently attest that exact PID.
    pub fn attach(
        mut child: Child,
        request: tempfile::TempDir,
        operation: &str,
    ) -> Result<Self, super::Error> {
        if let Err(error) = super::supported() {
            child.stdin = None;
            let _ = child.wait();
            return Err(error);
        }
        let attached = (|| {
            if uuid::Uuid::parse_str(operation)
                .map_err(io::Error::other)?
                .to_string()
                != operation
            {
                return Err(failure("capture operation must be a canonical UUID"));
            }
            let input = child
                .stdin
                .take()
                .ok_or_else(|| failure("capture control pipe missing"))?;
            let output = child
                .stdout
                .take()
                .ok_or_else(|| failure("capture status pipe missing"))?;
            Ok((input, output))
        })();
        let (input, output) = match attached {
            Ok(value) => value,
            Err(error) => {
                child.stdin = None;
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let (sender, events) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut output = io::BufReader::new(output);
            loop {
                let mut frame = Vec::new();
                let result = std::io::Read::take(&mut output, 2049).read_until(b'\n', &mut frame);
                let event = match result {
                    Ok(0) => break,
                    Ok(_) if frame.len() <= 2048 && frame.last() == Some(&b'\n') => {
                        serde_json::from_slice(&frame).map_err(io::Error::other)
                    }
                    Ok(_) => Err(failure("capture status frame exceeds bound")),
                    Err(error) => Err(error),
                };
                let failed = event.is_err();
                // A slow or dropped consumer must not block privileged cleanup.
                if sender.try_send(event).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input),
            events,
            operation: operation.into(),
            terminal: false,
            _request: request,
        })
    }

    /// Closing the control pipe revokes custody even while an RPC is in flight.
    pub fn cancel_now(&mut self) {
        self.input = None;
    }

    pub fn poll(&mut self) -> io::Result<Option<Status>> {
        match self.events.try_recv() {
            Ok(Ok(status)) if status.operation == self.operation => {
                if status.state == State::Released {
                    self.terminal = true;
                }
                Ok(Some(status))
            }
            Ok(Ok(_)) => Err(failure("capture status operation changed")),
            Ok(Err(error)) => Err(error),
            Err(mpsc::TryRecvError::Empty) => {
                if let Some(status) = self.child.try_wait()? {
                    if !status.success() || !self.terminal {
                        return Err(failure("capture helper exited before acknowledged release"));
                    }
                }
                Ok(None)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.input = None;
                if !self.terminal {
                    return Err(failure("capture status channel closed"));
                }
                Ok(None)
            }
        }
    }

    pub fn cancel_and_wait(&mut self) -> io::Result<()> {
        if let Some(mut input) = self.input.take() {
            let _ = input.write_all(b"cancel\n");
            let _ = input.flush();
        }
        let status = self.child.wait()?;
        if !status.success() {
            return Err(failure("capture helper cleanup failed"));
        }
        while let Ok(event) = self.events.recv() {
            let event = event?;
            if event.operation != self.operation {
                return Err(failure("capture status operation changed"));
            }
            if event.state == State::Released {
                self.terminal = true;
            }
        }
        if self.terminal {
            Ok(())
        } else {
            Err(failure("capture release was not acknowledged"))
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.input = None;
        let _ = self.child.wait();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
