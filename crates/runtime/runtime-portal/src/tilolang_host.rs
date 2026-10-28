//! The tilolang host's door: where the session hands a tilolang host each app
//! instance it runs.
//!
//! A tilolang host is one process per room and author that runs the tilolang
//! apps of that author in that room, each as an instance of its own
//! (`docs/designs/app-language/ui-host.md`). It has no app identity of its own:
//! everything an instance is arrives over this door, from the session, which
//! made it as that app's launch:
//!
//! 1. the instance's broker connection, which the session attested against a
//!    launch record of the instance carrying the app's creation record and
//!    launch ticket, exactly as for a launched app;
//! 2. its identity-service connection;
//! 3. its verified module, in a sealed memfd ([`Module`]);
//! 4. its window: the patch connection to the window's process, which shows
//!    what the instance sends and reports what the person did.
//!
//! A message is one `SOCK_SEQPACKET` packet in the framing of `jobs`: the verb
//! [`INSTANCE_VERB`] and the request ([`InstanceRequest`], JSON), with the four
//! descriptors in that order. The host never names an app to the session: the
//! broker and the compositor attribute each connection from the session's own
//! records. An instance ends when its window's connection closes.

use std::io;

use serde::{Deserialize, Serialize};

pub use crate::windows::Descriptor;

/// The variable the host reads its door's descriptor from.
pub const DOOR_FD_ENV: &str = "TILO_SVC_TILOLANG_HOST_FD";

/// The variable naming the host's storage: the one directory of its room and
/// author, keyed by package inside.
pub const STORAGE_ENV: &str = "TILO_TILOLANG_HOST_STORAGE";

pub const INSTANCE_VERB: &[u8] = b"instance";

/// The largest request: a few names and numbers.
pub const MAX_MESSAGE: usize = 4096;

/// The largest module the door carries.
pub const MAX_MODULE: u64 = 64 << 20;

/// The session's request to run one more app instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRequest {
    /// The app's package id, as the session launched it.
    pub app: String,
    /// The room the instance runs in.
    pub room: String,
    /// The window's instance: the durable id its tab keeps.
    pub instance: u64,
    /// The participant at the window, whose steps the instance's undo takes
    /// back.
    pub person: String,
    /// The length of the module in its memfd.
    pub module_len: u64,
}

/// What one instance runs: the package's compiled module, its ids lock and the
/// metadata of the packages it uses, as the package's payload holds them
/// (`app.wasm`, `app.ids`, `imports/*.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub wasm: Vec<u8>,
    pub ids: String,
    pub imports: Vec<Vec<u8>>,
}

const MODULE_MAGIC: &[u8; 8] = b"tlmodul1";

impl Module {
    /// The module of the package whose payload is `payload`.
    ///
    /// # Errors
    /// The module is missing or a file cannot be read.
    pub fn read(payload: &std::path::Path) -> io::Result<Self> {
        let wasm = std::fs::read(payload.join("app.wasm"))?;
        let ids = match std::fs::read_to_string(payload.join("app.ids")) {
            Ok(ids) => ids,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut paths: Vec<std::path::PathBuf> = match std::fs::read_dir(payload.join("imports")) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        paths.sort();
        let imports = paths
            .iter()
            .map(std::fs::read)
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self { wasm, ids, imports })
    }

    /// The module as the bytes its memfd holds.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.wasm
                .len()
                .saturating_add(self.ids.len())
                .saturating_add(64),
        );
        out.extend_from_slice(MODULE_MAGIC);
        let mut field = |bytes: &[u8]| {
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(bytes);
        };
        field(&self.wasm);
        field(self.ids.as_bytes());
        for import in &self.imports {
            field(import);
        }
        out
    }

    /// The module `bytes` hold.
    ///
    /// # Errors
    /// The bytes are no module.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a tilolang module");
        let mut rest = bytes.strip_prefix(MODULE_MAGIC).ok_or_else(invalid)?;
        let mut fields = Vec::new();
        while !rest.is_empty() {
            let (len, after) = rest.split_at_checked(8).ok_or_else(invalid)?;
            let len = u64::from_le_bytes(len.try_into().map_err(|_| invalid())?);
            let len = usize::try_from(len).map_err(|_| invalid())?;
            let (field, after) = after.split_at_checked(len).ok_or_else(invalid)?;
            fields.push(field.to_vec());
            rest = after;
        }
        let mut fields = fields.into_iter();
        let wasm = fields.next().ok_or_else(invalid)?;
        let ids = String::from_utf8(fields.next().ok_or_else(invalid)?).map_err(|_| invalid())?;
        Ok(Self {
            wasm,
            ids,
            imports: fields.collect(),
        })
    }
}

/// The descriptors one instance arrives with, in the order the door carries
/// them.
#[derive(Debug)]
pub struct InstanceFds {
    pub broker: Descriptor,
    pub identity: Descriptor,
    pub module: Descriptor,
    pub window: Descriptor,
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "only the Linux shell runs tilolang hosts",
    )
}

/// A connected pair of door ends: the session keeps one and hands the other
/// to the host it starts.
///
/// # Errors
/// The socket error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    crate::windows::pair()
}

/// The door the session handed this process at its start, once.
#[must_use]
pub fn take_door() -> Option<Descriptor> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd: i32 = std::env::var(DOOR_FD_ENV).ok()?.parse().ok()?;
        if fd < 0 || TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the session handed this descriptor to the process for the
        // door alone, and the flag above lets it be owned exactly once.
        Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Hand the host behind `door` the instance `request` with its descriptors.
///
/// # Errors
/// The socket's error, a request that does not encode, or
/// [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_instance(
    door: &Descriptor,
    request: &InstanceRequest,
    fds: &InstanceFds,
) -> io::Result<()> {
    let encoded = serde_json::to_vec(request).map_err(io::Error::other)?;
    if encoded.len() > MAX_MESSAGE {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        crate::jobs::send(
            door.as_fd(),
            &[INSTANCE_VERB, &encoded],
            &[
                fds.broker.as_fd(),
                fds.identity.as_fd(),
                fds.module.as_fd(),
                fds.window.as_fd(),
            ],
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, fds);
        Err(unsupported())
    }
}

/// The next instance on `door` and its descriptors; `None` at end of file,
/// when the session closed its end.
///
/// # Errors
/// The socket's error; a message that is no instance request or does not
/// carry exactly its four descriptors.
pub fn receive_instance(door: &Descriptor) -> io::Result<Option<(InstanceRequest, InstanceFds)>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        // The verb, its separator and the request.
        let mut buffer = vec![
            0_u8;
            MAX_MESSAGE
                .saturating_add(INSTANCE_VERB.len())
                .saturating_add(1)
        ];
        let Some((length, fds)) = crate::jobs::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        let [verb, request] = crate::jobs::decode(message)[..] else {
            return Err(invalid("the door carried something other than an instance"));
        };
        if verb != INSTANCE_VERB {
            return Err(invalid("the door carried something other than an instance"));
        }
        let request: InstanceRequest = serde_json::from_slice(request)
            .map_err(|_| invalid("an instance request that does not decode"))?;
        let [broker, identity, module, window] = <[Descriptor; 4]>::try_from(fds)
            .map_err(|_| invalid("an instance without its four connections"))?;
        if request.module_len > MAX_MODULE {
            return Err(invalid("a module over the door's limit"));
        }
        Ok(Some((
            request,
            InstanceFds {
                broker,
                identity,
                module,
                window,
            },
        )))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;

    fn connection() -> Descriptor {
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.into()
    }

    #[test]
    fn an_instance_crosses_the_door_with_its_four_connections() {
        let (shell, host) = pair().unwrap();
        let request = InstanceRequest {
            app: "so.tilo.lang.notes".into(),
            room: "7a110d00-0000-4000-8000-000000000001".into(),
            instance: 7,
            person: "ada".into(),
            module_len: 12,
        };
        let fds = InstanceFds {
            broker: connection(),
            identity: connection(),
            module: connection(),
            window: connection(),
        };
        send_instance(&shell, &request, &fds).unwrap();
        let (received, _fds) = receive_instance(&host).unwrap().unwrap();
        assert_eq!(received, request);
        drop(shell);
        assert!(
            receive_instance(&host).unwrap().is_none(),
            "closed door ends"
        );
    }

    #[test]
    fn a_message_without_its_connections_is_refused() {
        use std::os::fd::AsFd as _;
        let (shell, host) = pair().unwrap();
        let request = br#"{"app":"a","room":"r","instance":1,"person":"p","module_len":1}"#;
        let one = connection();
        crate::jobs::send(shell.as_fd(), &[INSTANCE_VERB, request], &[one.as_fd()]).unwrap();
        assert!(receive_instance(&host).is_err());
        crate::jobs::send(shell.as_fd(), &[b"open", request], &[]).unwrap();
        assert!(receive_instance(&host).is_err());
    }

    #[test]
    fn a_module_is_its_bytes_again() {
        let module = Module {
            wasm: vec![0, 97, 115, 109, 1],
            ids: "Note = 1\n".into(),
            imports: vec![b"{}".to_vec(), b"{\"app\":\"x\"}".to_vec()],
        };
        assert_eq!(Module::decode(&module.encode()).unwrap(), module);
        assert!(Module::decode(b"garbage").is_err());
        let mut cut = module.encode();
        cut.truncate(cut.len() - 1);
        assert!(Module::decode(&cut).is_err());
    }
}
