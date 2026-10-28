//! The socket between `tilo-boot-signin` and the greeter it starts in boot
//! mode (docs/security/sign-in.md §3.4).
//!
//! One message is one packet of a `SOCK_SEQPACKET` pair, so neither side
//! reads half a message or two. The supervisor tells the greeter what to
//! show; the greeter answers with what the person did. A secret travels as
//! bytes in a buffer that is wiped when it is dropped, on both sides; every
//! other message is JSON.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::net::{AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::flow::Note;
use crate::people::Person;

/// What the supervisor tells the greeter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "kebab-case")]
pub enum ToGreeter {
    /// The first message: the device's sign-in comforts, as the volume's
    /// `tilo-comforts` token has them. The greeter clamps every value.
    Hello { comforts: serde_json::Value },
    /// Everyone who may sign in, with `highlighted` the one to ask first.
    People {
        people: Vec<Person>,
        highlighted: usize,
    },
    /// Ask `person` for their secret, with `note` under the field.
    Ask {
        person: usize,
        note: Note,
        several: bool,
    },
    /// The secret given is being checked.
    Checking,
    /// The sign-in is over; the greeter quits.
    Finish { unlocked: bool },
}

/// What the person asked the machine to do from the power menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    PowerOff,
    Reboot,
}

/// What the greeter tells the supervisor.
#[derive(Debug, PartialEq)]
pub enum FromGreeter {
    /// `person` (an index into the people sent) typed `secret`.
    Secret {
        person: usize,
        secret: Zeroizing<String>,
    },
    /// "Can't sign in": the other ways in.
    CantSignIn,
    Power(Power),
}

/// The tags that open a packet from the greeter.
const SECRET: u8 = b'S';
const CANT_SIGN_IN: u8 = b'C';
const POWER_OFF: u8 = b'P';
const REBOOT: u8 = b'R';
/// A packet from the supervisor: JSON after the tag.
const JSON: u8 = b'J';

/// The longest secret a packet carries. The TPM takes far shorter ones.
pub const SECRET_LIMIT: usize = 4096;
/// The largest packet either side reads; the people list is the largest.
const PACKET_LIMIT: usize = 64 * 1024;

/// One end of the socket pair.
pub struct Link(OwnedFd);

impl Link {
    /// Both ends: the supervisor keeps the first and hands the second to the
    /// greeter. Neither is inherited by a program run in between.
    ///
    /// # Errors
    ///
    /// When the kernel has no socket to give.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (ours, theirs) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        Ok((Self(ours), Self(theirs)))
    }

    /// The end a process was handed.
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self(fd)
    }

    pub fn into_fd(self) -> OwnedFd {
        self.0
    }

    /// # Errors
    ///
    /// When the greeter is gone.
    pub fn send_to_greeter(&self, message: &ToGreeter) -> io::Result<()> {
        let mut packet = vec![JSON];
        serde_json::to_writer(&mut packet, message)?;
        self.send(&packet)
    }

    /// The greeter's next message, waiting for it.
    ///
    /// # Errors
    ///
    /// When the greeter is gone or sent something that is no message.
    pub fn recv_from_greeter(&self) -> io::Result<FromGreeter> {
        let packet = self.recv()?;
        let (tag, body) = packet.split_first().ok_or_else(malformed)?;
        match (*tag, body) {
            (CANT_SIGN_IN, []) => Ok(FromGreeter::CantSignIn),
            (POWER_OFF, []) => Ok(FromGreeter::Power(Power::PowerOff)),
            (REBOOT, []) => Ok(FromGreeter::Power(Power::Reboot)),
            (SECRET, [a, b, c, d, secret @ ..]) if secret.len() <= SECRET_LIMIT => {
                let person = usize::try_from(u32::from_le_bytes([*a, *b, *c, *d]))
                    .map_err(|_| malformed())?;
                let secret = std::str::from_utf8(secret).map_err(|_| malformed())?;
                Ok(FromGreeter::Secret {
                    person,
                    secret: Zeroizing::new(secret.to_owned()),
                })
            }
            _ => Err(malformed()),
        }
    }

    /// # Errors
    ///
    /// When the supervisor is gone, or the secret is longer than a packet
    /// carries.
    pub fn send_to_supervisor(&self, message: &FromGreeter) -> io::Result<()> {
        let mut packet = Zeroizing::new(Vec::new());
        match message {
            FromGreeter::CantSignIn => packet.push(CANT_SIGN_IN),
            FromGreeter::Power(Power::PowerOff) => packet.push(POWER_OFF),
            FromGreeter::Power(Power::Reboot) => packet.push(REBOOT),
            FromGreeter::Secret { person, secret } => {
                if secret.len() > SECRET_LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "the secret is longer than the sign-in takes",
                    ));
                }
                let person = u32::try_from(*person).map_err(|_| malformed())?;
                packet.reserve_exact(5_usize.saturating_add(secret.len()));
                packet.push(SECRET);
                packet.extend_from_slice(&person.to_le_bytes());
                packet.extend_from_slice(secret.as_bytes());
            }
        }
        self.send(&packet)
    }

    /// The supervisor's next message, waiting for it.
    ///
    /// # Errors
    ///
    /// When the supervisor is gone or sent something that is no message.
    pub fn recv_from_supervisor(&self) -> io::Result<ToGreeter> {
        let packet = self.recv()?;
        match packet.split_first() {
            Some((&JSON, body)) => Ok(serde_json::from_slice(body)?),
            _ => Err(malformed()),
        }
    }

    fn send(&self, packet: &[u8]) -> io::Result<()> {
        loop {
            match rustix::net::send(&self.0, packet, SendFlags::NOSIGNAL) {
                Ok(sent) if sent == packet.len() => return Ok(()),
                Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// One packet; the end of the socket is an error, since both sides only
    /// close it when they are done with each other.
    fn recv(&self) -> io::Result<Zeroizing<Vec<u8>>> {
        let mut packet = Zeroizing::new(vec![0_u8; PACKET_LIMIT]);
        loop {
            match rustix::net::recv(&self.0, packet.as_mut_slice(), RecvFlags::TRUNC) {
                Ok((0, _)) => return Err(io::ErrorKind::UnexpectedEof.into()),
                // A packet larger than the buffer is cut; TRUNC reports its
                // whole length, so a cut one is refused rather than misread.
                Ok((_, length)) if length > PACKET_LIMIT => return Err(malformed()),
                Ok((length, _)) => {
                    packet.truncate(length);
                    return Ok(packet);
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl AsFd for Link {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "not a sign-in message")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::SecretKind;

    #[test]
    fn messages_cross_whole_in_both_directions() {
        let (supervisor, greeter) = Link::pair().unwrap();
        let people = vec![Person {
            uid: 1000,
            name: "ada".into(),
            display_name: "Ada Lovelace".into(),
            secret: SecretKind::Pin,
        }];
        let sent = [
            ToGreeter::Hello {
                comforts: serde_json::json!({"screen_reader": true}),
            },
            ToGreeter::People {
                people,
                highlighted: 0,
            },
            ToGreeter::Ask {
                person: 0,
                note: Note::Wrong {
                    secret: SecretKind::Pin,
                    tries_left: Some(3),
                },
                several: false,
            },
            ToGreeter::Checking,
            ToGreeter::Finish { unlocked: true },
        ];
        for message in &sent {
            supervisor.send_to_greeter(message).unwrap();
        }
        for message in &sent {
            assert_eq!(&greeter.recv_from_supervisor().unwrap(), message);
        }

        let answers = [
            FromGreeter::Secret {
                person: 0,
                secret: Zeroizing::new("1234 ünï".into()),
            },
            FromGreeter::CantSignIn,
            FromGreeter::Power(Power::Reboot),
        ];
        for answer in &answers {
            greeter.send_to_supervisor(answer).unwrap();
        }
        for answer in &answers {
            assert_eq!(&supervisor.recv_from_greeter().unwrap(), answer);
        }
    }

    #[test]
    fn a_closed_end_and_a_malformed_packet_are_errors() {
        let (supervisor, greeter) = Link::pair().unwrap();
        greeter.send(b"Sxx").unwrap();
        greeter.send(&[SECRET, 0, 0, 0, 0, 0xff]).unwrap();
        greeter.send(b"?").unwrap();
        for _ in 0..3 {
            assert_eq!(
                supervisor.recv_from_greeter().unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        drop(greeter);
        assert_eq!(
            supervisor.recv_from_greeter().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn a_secret_longer_than_the_limit_is_not_sent() {
        let (_supervisor, greeter) = Link::pair().unwrap();
        let long = FromGreeter::Secret {
            person: 0,
            secret: Zeroizing::new("1".repeat(SECRET_LIMIT + 1)),
        };
        assert_eq!(
            greeter.send_to_supervisor(&long).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
