//! `tilo status` for the device admin without sudo.
//!
//! Which deployments are on the disk is known only to root: `bootc status` refuses everyone
//! else, and the ESP is mounted root-only because it holds the boot loader's random seed.
//! `tilo-deployment-status.socket` (group `wheel`) runs `tilo-deployment serve-status` per
//! connection, which takes one word, `text` or `json`, and answers with the listing's exit
//! status on the first line and the listing after it.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const SOCKET: &str = "/run/tilo-deployment/status.sock";

/// Above the service's own `TimeoutStartSec=`, so its verdict arrives before this gives up.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(75);

/// Where the service listens, or `None` on a system that has no such socket (an image older
/// than it), where the helper runs directly and says what it needs.
pub fn socket() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("TILO_DEPLOYMENT_STATUS_SOCKET")
        .map_or_else(|| std::path::PathBuf::from(SOCKET), Into::into);
    path.exists().then_some(path)
}

/// The listing's own exit status and text.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub code: u8,
    pub text: String,
}

pub fn ask(socket: &Path, json: bool) -> Result<Answer> {
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => bail!(
            "what is on this machine is shown to its admins; \
             as another account, run `sudo tilo status`"
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("connecting to {}", socket.display()));
        }
    };
    stream.set_read_timeout(Some(ANSWER_TIMEOUT))?;
    stream
        .write_all(if json { b"json\n" } else { b"text\n" })
        .context("asking for the status")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("asking for the status")?;
    let mut raw = String::new();
    stream
        .take(4 << 20)
        .read_to_string(&mut raw)
        .context("reading the status")?;
    parse(&raw)
}

fn parse(raw: &str) -> Result<Answer> {
    let (first, text) = raw.split_once('\n').unwrap_or((raw, ""));
    let Ok(code) = first.trim().parse::<u8>() else {
        bail!(
            "the status service gave no answer; `journalctl -u 'tilo-deployment-status@*'` says why"
        )
    };
    Ok(Answer {
        code,
        text: text.to_owned(),
    })
}

pub fn print(answer: &Answer) -> ExitCode {
    if answer.code == 0 {
        print!("{}", answer.text);
        ExitCode::SUCCESS
    } else {
        eprint!("{}", answer.text);
        ExitCode::from(answer.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn serve_once(
        answer: &'static str,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::thread::JoinHandle<String>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            stream.write_all(answer.as_bytes()).unwrap();
            request
        });
        (dir, path, server)
    }

    #[test]
    fn asks_for_one_word_and_returns_the_listing_with_its_status() {
        let (_dir, path, server) = serve_once("0\nDeployments:\n  booted    image\n");
        let answer = ask(&path, false).unwrap();
        assert_eq!(server.join().unwrap(), "text\n");
        assert_eq!(
            answer,
            Answer {
                code: 0,
                text: "Deployments:\n  booted    image\n".to_owned()
            }
        );
    }

    #[test]
    fn json_is_its_own_request() {
        let (_dir, path, server) = serve_once("0\n{}\n");
        ask(&path, true).unwrap();
        assert_eq!(server.join().unwrap(), "json\n");
    }

    #[test]
    fn a_failed_listing_keeps_its_status() {
        let (_dir, path, server) = serve_once("1\ntilo-deployment: no ESP\n");
        let answer = ask(&path, false).unwrap();
        server.join().unwrap();
        assert_eq!(answer.code, 1);
        assert_eq!(answer.text, "tilo-deployment: no ESP\n");
    }

    #[test]
    fn a_service_that_dies_without_an_answer_is_an_error() {
        let (_dir, path, server) = serve_once("");
        let error = ask(&path, false).unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains("gave no answer"), "{error}");
    }
}
