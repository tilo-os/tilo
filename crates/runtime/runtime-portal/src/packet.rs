//! Typed packets on a connected `SOCK_SEQPACKET` socket: one postcard value per
//! packet, with the descriptors the value names carried beside it as
//! `SCM_RIGHTS`. On Windows a packet is a message on a message-mode pipe, and
//! the handles it carries are duplicated into the process at the other end
//! (`packet::windows`).
//!
//! A message type states its descriptors in its own fields: a field marked
//! `#[serde(with = "packet::fd")]` (or [`optional_fd`]) is a descriptor. Writing
//! the value collects those fields' descriptors in the order the encoding visits
//! them, and reading it hands the received descriptors back to the same fields in
//! the same order. So the count each message carries is checked in one place,
//! here: a packet with fewer descriptors than its value names, or more, is
//! refused, and no caller counts them by hand.
//!
//! The packet boundary is the message boundary, so neither side buffers a stream
//! or frames a length.
//!
//! Every packet starts with the link's [`VERSION`], so a side built before a message changed
//! reads it as another version and says so, rather than reading the changed bytes as its own
//! message. A packet without a version is one written before the link was versioned.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(windows)]
mod windows;

/// A descriptor a packet carries.
#[cfg(unix)]
pub type Descriptor = std::os::fd::OwnedFd;
#[cfg(windows)]
pub use windows::Descriptor;
/// There are no packet sockets where there are no descriptors. Nothing can own a
/// value of this type, so copying one owns nothing either.
#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug)]
pub enum Descriptor {}

/// The raw form of a descriptor a value being written names.
#[cfg(unix)]
type Raw = std::os::fd::RawFd;
#[cfg(windows)]
type Raw = std::os::windows::io::RawHandle;

/// The most descriptors (on Windows, handles) one packet carries.
#[cfg(any(unix, windows))]
use crate::process_link::MAX_FDS;

/// The largest packet: a workspace's policy as one app sees it, which the session
/// bounds at 64 KiB, and the rest of its message.
pub const MAX_PACKET: usize = 96 * 1024;

/// The version of every packet on this link, written first: a message whose shape changed is a
/// higher version, and a side of another build answers the mismatch by name instead of reading
/// the bytes as its own message.
pub const VERSION: u8 = 1;

/// The byte a versioned packet starts with. No message variant has this index, so a packet that
/// does not start with it was written before the link carried a version. A page has no
/// descriptors and so no process link: only the Unix side reads and writes it.
#[cfg(unix)]
const VERSIONED: u8 = 0xFF;

thread_local! {
    /// The descriptors of the value being written, in the order its encoding met them.
    #[cfg(any(unix, windows))]
    static OUTGOING: RefCell<Option<Vec<Raw>>> = const { RefCell::new(None) };
    /// The descriptors of the packet being read, until its value takes them.
    static INCOMING: RefCell<Option<VecDeque<Descriptor>>> = const { RefCell::new(None) };
}

/// A message that travels as one packet.
pub trait Packet: Serialize + DeserializeOwned {
    /// What a message must hold beyond its shape to be read (a length under its
    /// limit): the reason it is refused otherwise.
    ///
    /// # Errors
    /// Why the message is refused.
    fn check(&self) -> Result<(), &'static str> {
        Ok(())
    }

    /// Write `self` on `socket` with the descriptors it names.
    ///
    /// # Errors
    /// The socket's error, a value that does not encode or is over [`MAX_PACKET`],
    /// or [`io::ErrorKind::Unsupported`] where there are no packet sockets.
    fn send(&self, socket: &Descriptor) -> io::Result<()> {
        send(socket, self)
    }

    /// The next message on `socket`; `None` once the peer is gone.
    ///
    /// # Errors
    /// The socket's error, or a packet that is not one message with exactly the
    /// descriptors it names.
    fn receive(socket: &Descriptor) -> io::Result<Option<Self>> {
        receive(socket, true)
    }

    /// The next message on `socket` if one is waiting; `None` when none is, or
    /// the peer is gone.
    ///
    /// # Errors
    /// As [`Packet::receive`].
    fn try_receive(socket: &Descriptor) -> io::Result<Option<Self>> {
        match receive(socket, false) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            other => other,
        }
    }
}

#[cfg(any(unix, windows))]
fn invalid(what: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "packet sockets carry descriptors only on Unix",
    )
}

#[cfg_attr(
    not(any(unix, windows)),
    allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "here a descriptor has no values, and its callers hold a reference to one"
    )
)]
fn send<T: Packet>(socket: &Descriptor, message: &T) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        OUTGOING.with(|outgoing| *outgoing.borrow_mut() = Some(Vec::new()));
        let encoded = postcard::to_stdvec(message);
        let raw = OUTGOING
            .with(|outgoing| outgoing.borrow_mut().take())
            .unwrap_or_default();
        let encoded =
            encoded.map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        // An empty packet with no descriptors is how the socket reads its end.
        if (encoded.is_empty() && raw.is_empty())
            || encoded.len().saturating_add(2) > MAX_PACKET
            || raw.len() > MAX_FDS
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut packet = Vec::with_capacity(encoded.len().saturating_add(2));
        packet.extend_from_slice(&[VERSIONED, VERSION]);
        packet.extend_from_slice(&encoded);
        #[cfg(windows)]
        {
            windows::send(socket, &packet, &raw)
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsFd as _, BorrowedFd};
            // SAFETY: every descriptor was read from a field of `message`, which stays
            // borrowed until the packet is sent.
            let fds: Vec<BorrowedFd<'_>> = raw
                .iter()
                .map(|fd| unsafe { BorrowedFd::borrow_raw(*fd) })
                .collect();
            crate::message::send_bytes(socket.as_fd(), &packet, &fds)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (socket, message);
        Err(unsupported())
    }
}

#[cfg_attr(
    not(any(unix, windows)),
    allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "here a descriptor has no values, and its callers hold a reference to one"
    )
)]
fn receive<T: Packet>(socket: &Descriptor, wait: bool) -> io::Result<Option<T>> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        let mut buffer = vec![0_u8; MAX_PACKET];
        let Some((length, fds)) =
            crate::message::receive_waiting(socket.as_fd(), &mut buffer, wait)?
        else {
            return Ok(None);
        };
        decode(buffer.get(..length).unwrap_or_default(), fds).map(Some)
    }
    #[cfg(windows)]
    {
        let Some((bytes, handles)) = windows::receive(socket, wait)? else {
            return Ok(None);
        };
        decode(&bytes, handles).map(Some)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (socket, wait);
        Err(unsupported())
    }
}

/// The message `bytes` hold, with the descriptors `fds` in the fields that name them. A packet
/// of another [`VERSION`], or one written before the link carried a version, is refused naming
/// both, never read by this build's shapes.
#[cfg(any(unix, windows))]
fn decode<T: Packet>(bytes: &[u8], fds: Vec<Descriptor>) -> io::Result<T> {
    match bytes {
        [] => Err(invalid("a packet with no version")),
        [VERSIONED, version, message @ ..] if *version != VERSION => Err(invalid(format!(
            "the other side speaks process link version {version}; this build speaks \
             {VERSION}: update Tilo"
        ))),
        [VERSIONED, _, message @ ..] => decoded(message, fds),
        [VERSIONED] => Err(invalid("a packet that names no version")),
        _ => Err(invalid(
            "a packet with no version: the other side runs a build from before the process link \
             was versioned; update Tilo",
        )),
    }
}

/// The message `bytes` hold at this build's [`VERSION`].
#[cfg(any(unix, windows))]
fn decoded<T: Packet>(bytes: &[u8], fds: Vec<Descriptor>) -> io::Result<T> {
    INCOMING.with(|incoming| *incoming.borrow_mut() = Some(fds.into()));
    let decoded = postcard::take_from_bytes::<T>(bytes);
    let left = INCOMING
        .with(|incoming| incoming.borrow_mut().take())
        .unwrap_or_default();
    let (message, rest) = decoded.map_err(invalid)?;
    if !rest.is_empty() {
        return Err(invalid("a packet holds more than its message"));
    }
    if !left.is_empty() {
        return Err(invalid(
            "a packet carries more descriptors than its message names",
        ));
    }
    message.check().map_err(invalid)?;
    Ok(message)
}

/// A field that is a descriptor the packet carries: `#[serde(with = "packet::fd")]`.
/// It is written as nothing; the descriptor goes beside the packet.
pub mod fd {
    use std::collections::VecDeque;

    use super::{Descriptor, Deserialize as _, Deserializer, INCOMING, Serializer};

    /// # Errors
    /// Outside a packet: a descriptor travels only beside one.
    pub fn serialize<S: Serializer>(fd: &Descriptor, serializer: S) -> Result<S::Ok, S::Error> {
        #[cfg(unix)]
        {
            use serde::ser::Error as _;
            use std::os::fd::AsRawFd as _;
            let carried = super::OUTGOING.with(|outgoing| {
                outgoing
                    .borrow_mut()
                    .as_mut()
                    .map(|fds| fds.push(fd.as_raw_fd()))
                    .is_some()
            });
            if !carried {
                return Err(S::Error::custom(
                    "a descriptor travels only beside a packet",
                ));
            }
            serializer.serialize_unit()
        }
        #[cfg(windows)]
        {
            use serde::ser::Error as _;
            use std::os::windows::io::AsRawHandle as _;
            let carried = super::OUTGOING.with(|outgoing| {
                outgoing
                    .borrow_mut()
                    .as_mut()
                    .map(|handles| handles.push(fd.as_raw_handle()))
                    .is_some()
            });
            if !carried {
                return Err(S::Error::custom(
                    "a descriptor travels only beside a packet",
                ));
            }
            serializer.serialize_unit()
        }
        #[cfg(not(any(unix, windows)))]
        {
            use serde::ser::Error as _;
            let _ = (fd, serializer);
            Err(S::Error::custom("there are no descriptors here"))
        }
    }

    /// # Errors
    /// A packet that carries none for this field.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Descriptor, D::Error> {
        use serde::de::Error as _;
        <()>::deserialize(deserializer)?;
        INCOMING
            .with(|incoming| incoming.borrow_mut().as_mut().and_then(VecDeque::pop_front))
            .ok_or_else(|| D::Error::custom("a packet without a descriptor its message names"))
    }
}

/// A field that is a descriptor the packet may carry:
/// `#[serde(with = "packet::optional_fd")]`.
pub mod optional_fd {
    use super::{Descriptor, Deserialize, Deserializer, Serialize, Serializer};

    struct Carried<'a>(&'a Descriptor);

    impl Serialize for Carried<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            super::fd::serialize(self.0, serializer)
        }
    }

    struct Taken(Descriptor);

    impl<'de> Deserialize<'de> for Taken {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            super::fd::deserialize(deserializer).map(Self)
        }
    }

    /// # Errors
    /// As [`super::fd::serialize`].
    #[allow(
        clippy::ref_option,
        reason = "serde's `with` hands the field by reference"
    )]
    pub fn serialize<S: Serializer>(
        fd: &Option<Descriptor>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        fd.as_ref().map(Carried).serialize(serializer)
    }

    /// # Errors
    /// As [`super::fd::deserialize`].
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Descriptor>, D::Error> {
        Ok(Option::<Taken>::deserialize(deserializer)?.map(|taken| taken.0))
    }
}

/// A connected pair of packet sockets.
///
/// # Errors
/// The system's error, or [`io::ErrorKind::Unsupported`] off Linux and Windows.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    #[cfg(target_os = "linux")]
    {
        use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
        Ok(socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?)
    }
    #[cfg(windows)]
    return windows::pair();
    #[cfg(not(any(target_os = "linux", windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "packet socket pairs are made on Linux and Windows only",
    ))
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    #[derive(Debug, Serialize, Deserialize)]
    enum Message {
        Plain(String),
        One {
            name: String,
            #[serde(with = "fd")]
            connection: Descriptor,
        },
        Maybe {
            #[serde(with = "optional_fd")]
            first: Option<Descriptor>,
            #[serde(with = "fd")]
            second: Descriptor,
        },
        Long(u64),
    }

    impl Packet for Message {
        fn check(&self) -> Result<(), &'static str> {
            match self {
                Self::Long(length) if *length > 10 => Err("over its limit"),
                _ => Ok(()),
            }
        }
    }

    fn connection() -> (std::os::unix::net::UnixStream, Descriptor) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        (ours, theirs.into())
    }

    tilo_test_env::scenario! {
        fn typed_packets_with_their_descriptors;

        phase a_message_arrives_with_the_descriptors_it_names_in_their_fields;
        {
            let (a, b) = pair().unwrap();
            Message::Plain("hello".into()).send(&a).unwrap();
            assert!(matches!(Message::receive(&b).unwrap(), Some(Message::Plain(text)) if text == "hello"));

            let (mut first_end, first) = connection();
            let (mut second_end, second) = connection();
            Message::Maybe { first: Some(first), second }.send(&a).unwrap();
            let Some(Message::Maybe { first: Some(first), second }) = Message::receive(&b).unwrap() else {
                panic!("both descriptors arrive in their fields");
            };
            first_end.write_all(b"1").unwrap();
            second_end.write_all(b"2").unwrap();
            let mut byte = [0_u8; 1];
            std::os::unix::net::UnixStream::from(first).read_exact(&mut byte).unwrap();
            assert_eq!(&byte, b"1", "the first field holds the first descriptor");
            std::os::unix::net::UnixStream::from(second).read_exact(&mut byte).unwrap();
            assert_eq!(&byte, b"2");

            let (_end, second) = connection();
            Message::Maybe { first: None, second }.send(&a).unwrap();
            assert!(matches!(Message::receive(&b).unwrap(), Some(Message::Maybe { first: None, .. })));
            drop(a);
            assert!(Message::receive(&b).unwrap().is_none(), "a closed socket ends");
        }

        phase a_packet_without_its_descriptors_or_with_more_is_refused;
        {
            use std::os::fd::AsFd as _;
            let (a, b) = pair().unwrap();
            let bytes = postcard::to_stdvec(&(1_u32, "name")).unwrap();
            crate::message::send_bytes(a.as_fd(), &bytes, &[]).unwrap();
            assert_eq!(Message::receive(&b).unwrap_err().kind(), io::ErrorKind::InvalidData);
            let (_end, extra) = connection();
            let plain = postcard::to_stdvec(&(0_u32, "hello")).unwrap();
            crate::message::send_bytes(a.as_fd(), &plain, &[extra.as_fd()]).unwrap();
            assert_eq!(Message::receive(&b).unwrap_err().kind(), io::ErrorKind::InvalidData);
            crate::message::send_bytes(a.as_fd(), b"\xff", &[]).unwrap();
            assert!(Message::receive(&b).is_err(), "no message");
        }

        phase a_packet_of_another_build_is_refused_by_name;
        {
            use std::os::fd::AsFd as _;
            let (a, b) = pair().unwrap();
            // A build from before the link carried a version: the message's own bytes, no
            // version first.
            let old = postcard::to_stdvec(&Message::Plain("hello".into())).unwrap();
            crate::message::send_bytes(a.as_fd(), &old, &[]).unwrap();
            let error = Message::receive(&b).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("no version"), "{error}");
            // A build after this one speaks a version this one does not: both are named, so
            // whoever reads it knows which side updates.
            let mut newer = vec![0xFF, VERSION.saturating_add(1)];
            newer.extend(postcard::to_stdvec(&Message::Plain("hello".into())).unwrap());
            crate::message::send_bytes(a.as_fd(), &newer, &[]).unwrap();
            let error = Message::receive(&b).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                error.to_string().contains(&format!("version {}", VERSION + 1)),
                "it names the other side's version: {error}"
            );
            assert!(
                error.to_string().contains(&format!("speaks {VERSION}")),
                "it names this build's version: {error}"
            );
        }

        phase a_message_its_check_refuses_is_refused;
        {
            let (a, b) = pair().unwrap();
            Message::Long(11).send(&a).unwrap();
            assert_eq!(Message::receive(&b).unwrap_err().kind(), io::ErrorKind::InvalidData);
            Message::Long(3).send(&a).unwrap();
            assert!(matches!(Message::receive(&b).unwrap(), Some(Message::Long(3))));
        }

        phase nothing_waiting_is_none_without_blocking;
        {
            let (a, b) = pair().unwrap();
            assert!(Message::try_receive(&b).unwrap().is_none());
            Message::Long(1).send(&a).unwrap();
            assert!(matches!(Message::try_receive(&b).unwrap(), Some(Message::Long(1))));
        }

        phase a_descriptor_outside_a_packet_does_not_encode;
        {
            let (_end, connection) = connection();
            assert!(postcard::to_stdvec(&Message::One { name: "x".into(), connection }).is_err());
        }
    }
}
