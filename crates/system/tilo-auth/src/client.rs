use crate::protocol::{MAX_LINE, PendingSignIn, Reply, Request};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use zeroize::Zeroizing;

/// Long enough for an Argon2id unwrap or a slow TPM, short enough that a
/// wedged daemon does not hang a sign-in screen for good.
const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Client {
    reader: BufReader<UnixStream>,
}

impl Client {
    pub fn connect(socket: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    fn exchange(&mut self, request: &Request) -> io::Result<Zeroizing<Vec<u8>>> {
        let mut line = Zeroizing::new(serde_json::to_vec(request).map_err(io::Error::other)?);
        line.push(b'\n');
        self.reader.get_mut().write_all(&line)?;
        let mut reply = Zeroizing::new(Vec::new());
        (&mut self.reader)
            .take(u64::try_from(MAX_LINE).unwrap_or(u64::MAX))
            .read_until(b'\n', &mut reply)?;
        if reply.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tilo-authd closed the connection without answering",
            ));
        }
        Ok(reply)
    }

    pub fn call(&mut self, request: &Request) -> io::Result<Reply> {
        let reply = self.exchange(request)?;
        serde_json::from_slice(&reply).map_err(io::Error::other)
    }

    pub fn pending_sign_in(&mut self) -> io::Result<Option<String>> {
        let reply = self.exchange(&Request::PendingSignIn)?;
        serde_json::from_slice::<Option<PendingSignIn>>(&reply)
            .map(|p| p.map(|p| p.name))
            .map_err(io::Error::other)
    }
}
