//! Outside origins on the platform: what keeps the resources of a type with an origin
//! (`type Message (origin: imap.message)`) in step with its service (convergence.md, "Outside
//! services"; `tilolang::origin`).
//!
//! An [`Origin`] is native code the instance runs beside the package: the connection to the
//! service, with the service's credentials, which package code never sees. Each frame the
//! instance polls it: it writes what the service said into the resources that hold its kinds
//! (one change by the origin, through [`tilolang_runtime::Runtime::apply_origin`]) and tells
//! the service what people changed in the fields people change. The outside effects package
//! code hands its capabilities (`outside("smtp.send", ...)`) are delivered to it from what the
//! action declared, as every outside effect is.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::{Delivery, OutsideSink};

use crate::background::Waker;
use crate::host::Host;

/// What a poll did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// It wrote into the workspace: the instance hands the change to the API.
    pub wrote: bool,
    /// An effect it was handed has an outcome now: the effects are settled again.
    pub settle: bool,
}

/// What an origin is polled with.
pub struct OriginCx<'a> {
    pub host: &'a Host,
    /// The package the instance runs.
    pub app: &'a str,
    /// Wakes the instance's frame loop when the service answers.
    pub waker: &'a Waker,
    /// The time now, as the runtime records it.
    pub now: String,
}

/// The platform's side of an outside service.
pub trait Origin {
    /// The origin's name: the first part of its kinds (`imap`).
    fn name(&self) -> &str;

    /// Whether the outside effects of `capability` (`smtp.send`) are the origin's to carry out.
    fn serves(&self, capability: &str) -> bool;

    /// Whether the origin has something to do: the service answered (and woke the instance),
    /// or people changed what the service is told. The instance polls it only then, so nothing
    /// runs while nothing changed.
    fn due(&self) -> bool;

    /// Take in what the service said and tell it what people changed.
    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled;

    /// An outside effect for one of the origin's capabilities: hand it to the service, or say
    /// what became of it since it was handed over.
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery;

    /// What became of an effect this origin took and settled nothing of yet, or `None` while
    /// the service has not answered: asked on every settle after [`Self::deliver`] answered
    /// [`Delivery::Pending`], which hands it over once. An origin that answers `Pending` answers
    /// here once the service answered, its service going away included.
    fn outcome(&mut self, capability: &str, payload: &Json) -> Option<Delivery> {
        let _ = (capability, payload);
        None
    }
}

/// An origin as the instance holds it.
pub type SharedOrigin = Rc<RefCell<dyn Origin>>;

/// The origins of an instance, which it may gain after its runtime was set up.
pub type Origins = Rc<RefCell<Vec<SharedOrigin>>>;

/// The outside sink of an instance: each origin's capabilities go to it, the rest to the sink
/// it had.
pub struct RoutingSink {
    pub origins: Origins,
    pub rest: Box<dyn OutsideSink>,
}

impl OutsideSink for RoutingSink {
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        for origin in self.origins.borrow().iter() {
            // Busy answering a poll: nothing went, and the effect is handed over on the next
            // settle. The instance polls its origins and settles one after the other, so only
            // a settle from within a poll gets here.
            let Ok(mut origin) = origin.try_borrow_mut() else {
                tracing::warn!(capability, "an effect was handed to an origin busy polling");
                return Delivery::Deferred;
            };
            if origin.serves(capability) {
                return origin.deliver(capability, payload);
            }
        }
        self.rest.deliver(capability, payload)
    }

    fn outcome(&mut self, capability: &str, payload: &Json) -> Option<Delivery> {
        for origin in self.origins.borrow().iter() {
            let Ok(mut origin) = origin.try_borrow_mut() else {
                tracing::warn!(capability, "an effect's outcome was asked of an origin busy polling");
                return None;
            };
            if origin.serves(capability) {
                return origin.outcome(capability, payload);
            }
        }
        self.rest.outcome(capability, payload)
    }
}

/// A type of a loaded package that holds one of an origin's kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginType {
    pub app: String,
    /// The type's id.
    pub ty: String,
    /// The kind it holds: `imap.message`.
    pub kind: String,
    /// The id of each of its fields, by name.
    pub fields: BTreeMap<String, String>,
}

impl OriginType {
    /// The id of the field named `name`, when the type declares it.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

/// The types of `meta` that hold kinds of the origin `origin`.
#[must_use]
pub fn origin_types(meta: &Meta, origin: &str) -> Vec<OriginType> {
    meta.types
        .iter()
        .filter_map(|t| {
            let kind = t.origin.as_ref()?;
            (kind.split('.').next() == Some(origin)).then(|| OriginType {
                app: meta.app.clone(),
                ty: t.id.clone(),
                kind: kind.clone(),
                fields: t
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.id.clone()))
                    .collect(),
            })
        })
        .collect()
}

/// Whether any type of `meta` holds a kind of `origin`.
#[must_use]
pub fn uses_origin(meta: &Meta, origin: &str) -> bool {
    !origin_types(meta, origin).is_empty()
}

/// How an origin reaches the connector the session runs for it: the socket the session handed
/// both ends of, made into the origin's native client (the mail origin's over the mail
/// connector, the `YouTube` origin's over the `YouTube` one).
///
/// An origin's own crate registers one as it loads ([`register_remote`]), so the crate that
/// keeps a package's documents (a session's `tilolang_service::ResourceService`, a tilolang
/// host's instance) reaches a connector without naming the origin's crate or the service it
/// speaks.
#[cfg(any(unix, windows))]
pub type Remote = dyn Fn(tilo_socket::Stream, Waker) -> Result<SharedOrigin, String> + Send + Sync;

#[cfg(any(unix, windows))]
static REMOTES: std::sync::Mutex<Vec<(&'static str, &'static Remote)>> =
    std::sync::Mutex::new(Vec::new());

/// Have the origin `name` reached over `remote` from now on: its crate's client of the
/// connector the session runs for it. A crate registers its own at its start (a `OnceLock`,
/// so its client lives as long as the process).
#[cfg(any(unix, windows))]
pub fn register_remote(name: &'static str, remote: &'static Remote) {
    let mut remotes = REMOTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    remotes.retain(|(registered, _)| *registered != name);
    remotes.push((name, remote));
}

/// The origin `name`'s client of the connector at the far end of `stream`, when a crate
/// registered one here.
#[cfg(any(unix, windows))]
pub fn remote(
    name: &str,
    stream: tilo_socket::Stream,
    waker: Waker,
) -> Result<SharedOrigin, String> {
    let remote = REMOTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(registered, _)| *registered == name)
        .map(|(_, remote)| *remote);
    let Some(remote) = remote else {
        return Err(format!(
            "no crate registered a client for the origin `{name}`: this build has no connector for it"
        ));
    };
    remote(stream, waker)
}

/// Whether some crate registered a client for the origin `name`: a service whose resources are
/// kept in step by a connector process the session runs (`imap`, `youtube`).
///
/// The origins the session itself writes are no connector's (the types whose kinds the device
/// derives, `session.layout`): what writes them runs in the session's own process.
#[must_use]
pub fn served(name: &str) -> bool {
    // A page reaches no connector process: no origin is served there.
    #[cfg(not(any(unix, windows)))]
    {
        let _ = name;
        false
    }
    #[cfg(any(unix, windows))]
    REMOTES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|(registered, _)| *registered == name)
}

/// What an origin's service holds in a sample workspace, taken into the package `app` of `host`
/// as of the time `now`: a fixture answer of the service, written by the origin's own conversion
/// of an answer, so it lands as the service's change (`$origin`) exactly as a real answer does.
pub type Sample = dyn Fn(&Host, &str, &str) -> Result<(), String> + Send + Sync;

static SAMPLES: std::sync::Mutex<Vec<(&'static str, &'static Sample)>> =
    std::sync::Mutex::new(Vec::new());

/// Have the origin `name` answer a sample workspace with `sample` from now on: its crate's
/// fixture of what its service holds. A crate registers its own as [`register_remote`] does.
pub fn register_sample(name: &'static str, sample: &'static Sample) {
    let mut samples = SAMPLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    samples.retain(|(registered, _)| *registered != name);
    samples.push((name, sample));
}

/// What the services of `app`'s origins hold in a sample workspace, taken in through each
/// origin, as of `now`: what the package's first scenario, its seed, stands on. A scenario is no
/// action of the app's surface and writes only through the app's own actions, so the records an
/// outside service keeps (a mail account and its mail, a channel and its videos) come in this
/// way, before it, and its actions act on them. An origin no crate registered a sample for adds
/// nothing.
///
/// # Errors
/// `app` is not loaded, or a sample could not be taken in whole.
pub fn take_in_samples(host: &Host, app: &str, now: &str) -> Result<(), String> {
    let Some(package) = host.package(app) else {
        return Err(format!("{app} is not loaded"));
    };
    let samples: Vec<(&'static str, &'static Sample)> = SAMPLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(name, _)| uses_origin(&package.meta, name))
        .copied()
        .collect();
    for (name, sample) in samples {
        sample(host, app, now)
            .map_err(|error| format!("what {name} holds for {app} was not taken in: {error}"))?;
    }
    Ok(())
}

/// Write `message` as one JSON frame under `framing` and flush `to`.
///
/// # Errors
/// The stream's error, or a message that does not encode or is larger than `framing`'s limit.
pub fn write_json_frame<T: Serialize>(
    framing: wire_frame::Framing,
    to: &mut impl Write,
    message: &T,
) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    framing.write(to, &body).map_err(io::Error::from)?;
    to.flush()
}

/// Read one JSON frame under `framing`; `None` at a clean end of the stream.
///
/// # Errors
/// The stream's error, a truncated or oversized frame, or JSON that does not decode.
pub fn read_json_frame<T: for<'de> Deserialize<'de>>(
    framing: wire_frame::Framing,
    from: &mut impl Read,
) -> io::Result<Option<T>> {
    let Some(body) = framing.read(from).map_err(io::Error::from)? else {
        return Ok(None);
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_frame_flushes_enforces_limit_and_rejects_partial_prefix() {
        let framing = wire_frame::Framing::big_endian(16);

        let mut writer = io::BufWriter::new(Vec::new());
        write_json_frame(framing, &mut writer, &"ok").unwrap();
        assert!(writer.buffer().is_empty(), "write_json_frame must flush");
        let wire = writer.into_inner().unwrap();
        assert_eq!(wire, b"\x00\x00\x00\x04\"ok\"");

        let mut reading = wire.as_slice();
        assert_eq!(
            read_json_frame::<String>(framing, &mut reading).unwrap(),
            Some("ok".to_owned())
        );
        assert_eq!(
            read_json_frame::<String>(framing, &mut reading).unwrap(),
            None
        );

        let mut refused = Vec::new();
        let too_big = write_json_frame(framing, &mut refused, &"0123456789abcdef").unwrap_err();
        assert_eq!(too_big.kind(), io::ErrorKind::InvalidInput);
        assert!(refused.is_empty());

        for partial in [&[0_u8][..], &[0, 0][..], &[0, 0, 0][..]] {
            let mut slice = partial;
            let error = read_json_frame::<String>(framing, &mut slice).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }

        let mut truncated_body = &b"\x00\x00\x00\x04\"o"[..];
        let error = read_json_frame::<String>(framing, &mut truncated_body).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

        let mut oversized = &b"\x00\x00\x00\x11\"0123456789abcde\""[..];
        let error = read_json_frame::<String>(framing, &mut oversized).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut malformed = &b"\x00\x00\x00\x03not"[..];
        let error = read_json_frame::<String>(framing, &mut malformed).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
