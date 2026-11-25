//! A line-wire connection carried to another device: a program of this computer asks its daemon
//! to pass the action frames it sends on to one of the person's devices ([`Carry`]), and the
//! daemon asks that device as this one, over the one endpoint it keeps under the device key. The
//! program never has the key or an endpoint of its own: two endpoints under one key push each
//! other off the network, and only the daemon dials as the device.
//!
//! The connection's first line names the device ([`carry_request`]); the daemon answers whether
//! it carries it ([`CarryAnswer`]), and from then on the connection is the action wire to that
//! device ([`LineClient::carried`]).

use serde::{Deserialize, Serialize};

/// The device a connection is carried to: its key, the workspace it is asked in, and where it is
/// reached when its key alone does not find it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carry {
    /// The device's key, as 64 hex digits.
    pub device: String,
    /// The workspace both devices are in, whose grants decide what the device lets this one do.
    pub workspace: String,
    /// The socket addresses the device answers at directly (`127.0.0.1:7703`), if any.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// The relay the device is reached through, if one is known.
    #[serde(default)]
    pub relay: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CarryLine {
    carry: Carry,
}

/// What a daemon answers a [`Carry`] with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case", deny_unknown_fields)]
pub enum CarryAnswer {
    /// The connection is carried: action frames follow.
    Open,
    /// It is not, and why.
    Refused { reason: String },
}

/// The carry a connection's first line asks for, if that is what it is.
#[must_use]
pub fn carry_request(line: &[u8]) -> Option<Carry> {
    serde_json::from_slice::<CarryLine>(line)
        .ok()
        .map(|line| line.carry)
}

/// The first line that asks for `carry`, without its newline.
#[must_use]
pub fn carry_line(carry: &Carry) -> String {
    serde_json::to_string(&CarryLine {
        carry: carry.clone(),
    })
    .unwrap_or_default()
}

/// `answer` as the line a daemon writes, without its newline.
#[must_use]
pub fn answer_line(answer: &CarryAnswer) -> String {
    serde_json::to_string(answer).unwrap_or_default()
}

#[cfg(any(unix, windows))]
mod client {
    use std::io::{self, Read as _, Write as _};

    use super::{Carry, CarryAnswer, carry_line};
    use crate::remote::LineClient;

    /// The longest answer to a carry read.
    const MAX_ANSWER: usize = 64 * 1024;

    impl LineClient {
        /// A client of the device `carry` names, carried by the daemon listening at the local
        /// socket `path`: every request goes to that device as the daemon's device asks it.
        ///
        /// # Errors
        /// Nothing listens at `path` ([`io::ErrorKind::NotFound`] or as the socket says), the
        /// daemon refused to carry the connection ([`io::ErrorKind::PermissionDenied`], with its
        /// reason), or it answered something else.
        pub fn carried(path: &std::path::Path, carry: &Carry) -> io::Result<Self> {
            let mut stream = tilo_socket::connect(path)?;
            writeln!(stream, "{}", carry_line(carry))?;
            // Read byte by byte: what follows the answer is the client's, never this read's.
            let mut answer = Vec::new();
            let mut byte = [0_u8; 1];
            loop {
                match stream.read(&mut byte)? {
                    0 => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!(
                                "this computer's daemon closed without saying whether it carries \
                                 this connection to {}",
                                carry.device
                            ),
                        ));
                    }
                    _ if byte == *b"\n" => break,
                    _ if answer.len() >= MAX_ANSWER => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "this computer's daemon answered with a line too long to read",
                        ));
                    }
                    _ => answer.extend_from_slice(&byte),
                }
            }
            match serde_json::from_slice::<CarryAnswer>(&answer) {
                Ok(CarryAnswer::Open) => Self::of_stream(stream),
                Ok(CarryAnswer::Refused { reason }) => {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, reason))
                }
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "this computer's daemon does not carry connections to another device: it \
                         answered {}",
                        String::from_utf8_lossy(&answer)
                    ),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_carry_line_reads_back_and_nothing_else_reads_as_one() {
        let carry = Carry {
            device: "ab".repeat(32),
            workspace: "w".into(),
            addresses: vec!["127.0.0.1:7703".into()],
            relay: None,
        };
        assert_eq!(carry_request(carry_line(&carry).as_bytes()), Some(carry));
        assert_eq!(carry_request(br#"{"forward":{"device":"x"}}"#), None);
        assert_eq!(
            carry_request(br#"{"carry":{"device":"x","workspace":"w","port":1}}"#),
            None,
            "an unknown field is refused"
        );
    }
}
