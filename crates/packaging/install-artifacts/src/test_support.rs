//! Explicit dev-only fixtures. These are not native-host or provenance evidence.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
// Same sizes, offsets, identities and payloads as Python valid_fixture().
// These are ordinary synthetic bytes, not filesystems or a bootable UKI.
pub fn manifest_value() -> Value {
    let root = "12345678-1234-4234-8234-123456789abc";
    json!({"schema":1,"purpose":"qos-internal-installer","release_id":"fixture-only",
        "architecture":"x86_64","firmware":"uefi","secure_boot":"disabled",
        "installer_protocol":1,"source_image_bytes":65536,
        "components":[
            {"role":"staging-esp","filename":"staging-esp.img","byte_size":4096,
             "sha256":format!("{:x}",Sha256::digest(vec![b'E';4096])),
             "partition_type_guid":"c12a7328-f81f-11d2-ba4b-00a0c93ec93b","filesystem_type":"vfat","filesystem_uuid":"1234-ABCD",
             "minimum_partition_bytes":4096,"source_offset":17408},
            {"role":"staging-root","filename":"staging-root.img","byte_size":8192,
             "sha256":format!("{:x}",Sha256::digest(vec![b'R';8192])),
             "partition_type_guid":"0fc63daf-8483-4772-8e79-3d69d8477de4","filesystem_type":"btrfs","filesystem_uuid":root,
             "minimum_partition_bytes":8192,"source_offset":21504}],
        "boot_binding":{"efi_path":"/EFI/Linux/fixture.efi","uki_architecture":"x86_64",
            "uki_sha256":"a".repeat(64),"composefs_digest":"b".repeat(128),
            "root_locator":format!("UUID={root}"),
            "uki_cmdline":format!("root=UUID={root} rw qos.install composefs={}","b".repeat(128))}})
}
pub fn manifest_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
#[cfg(any(unix, test))]
mod server {
    use super::response;
    use crate::{catalog::InternalRelease, transport::InternalTransport};
    use reqwest::Url;
    use std::{
        io::{Read, Write},
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };
    use std::{
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };
    pub struct Server {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
        consumptions: Arc<std::sync::atomic::AtomicU64>,
    }
    impl Server {
        #[cfg(test)]
        pub(crate) fn origin(&self) -> Url {
            Url::parse(&self.url).unwrap()
        }
        pub fn selection(&self, manifest: &[u8]) -> crate::ReleaseSelection {
            crate::ReleaseSelection {
                release: self.release(manifest),
            }
        }
        #[cfg(unix)]
        pub fn client(
            &self,
            root: &std::path::Path,
            preference: &std::path::Path,
        ) -> crate::Client {
            crate::Client::fixture(
                root,
                crate::preference::PreferenceStore::at(preference.to_owned()),
                self.transport(),
                self.consumptions.clone(),
            )
        }
        #[cfg(unix)]
        pub fn client_with_preference_fault(
            &self,
            root: &std::path::Path,
            preference: &std::path::Path,
            fault: super::PreferenceFault,
        ) -> (
            crate::Client,
            std::sync::mpsc::Receiver<super::PreferenceFault>,
        ) {
            let (ack, observed) = std::sync::mpsc::channel();
            let store = crate::preference::PreferenceStore::at(preference.to_owned())
                .with_hooks(super::PreferenceHooks::Fault { fault, ack });
            (
                crate::Client::fixture(root, store, self.transport(), self.consumptions.clone()),
                observed,
            )
        }
        #[cfg(unix)]
        pub fn client_with_preference_hold(
            &self,
            root: &std::path::Path,
            preference: &std::path::Path,
        ) -> (
            crate::Client,
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::Sender<()>,
        ) {
            let (ack, observed) = std::sync::mpsc::channel();
            let (release, held) = std::sync::mpsc::channel();
            let store = crate::preference::PreferenceStore::at(preference.to_owned()).with_hooks(
                super::PreferenceHooks::Hold {
                    ack,
                    release: Arc::new(Mutex::new(held)),
                },
            );
            (
                crate::Client::fixture(root, store, self.transport(), self.consumptions.clone()),
                observed,
                release,
            )
        }
        pub fn consumptions(&self) -> u64 {
            self.consumptions.load(Ordering::Acquire)
        }

        pub fn new(responses: Vec<Vec<u8>>) -> Self {
            Self::on_request(responses, || {})
        }
        pub fn on_request(
            responses: Vec<Vec<u8>>,
            mut on_request: impl FnMut() + Send + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!(
                "http://{}/releases/manifest.json",
                listener.local_addr().unwrap()
            );
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            let thread = thread::spawn(move || {
                let mut responses = responses.into_iter();
                while !done.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            // Darwin can inherit O_NONBLOCK from the listener. A
                            // temporary WouldBlock is not an empty HTTP request.
                            socket.set_nonblocking(false).unwrap();
                            socket
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut request = Vec::new();
                            let mut byte = [0];
                            while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                                if socket.read(&mut byte).unwrap_or(0) == 0 {
                                    break;
                                }
                                request.push(byte[0]);
                            }
                            seen.lock()
                                .unwrap()
                                .push(String::from_utf8(request).unwrap());
                            on_request();
                            let response =
                                responses.next().unwrap_or_else(|| response(500, "", b""));
                            let _ = socket.write_all(&response);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(e) => panic!("{e}"),
                    }
                }
            });
            Self {
                url,
                requests,
                stop,
                thread: Some(thread),
                consumptions: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            }
        }
        pub fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
        pub(crate) fn transport(&self) -> InternalTransport {
            InternalTransport::client(Url::parse(&self.url).unwrap(), Duration::from_secs(2))
                .unwrap()
        }
        pub(crate) fn release(&self, bytes: &[u8]) -> InternalRelease {
            InternalRelease::transport_fixture(bytes, &self.url)
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }
}
#[cfg(unix)]
pub use server::Server;
#[cfg(all(test, not(unix)))]
pub(crate) use server::Server;
pub fn response(status: u16, headers: &str, bytes: &[u8]) -> Vec<u8> {
    let mut result =
        format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    result.extend_from_slice(bytes);
    result
}

pub fn selection_from_catalog(raw: &str) -> Result<crate::ReleaseSelection, crate::Error> {
    let catalog = crate::catalog_data::parse_catalog(raw).map_err(crate::Error::invalid_input)?;
    match catalog.releases.into_iter().next() {
        Some(crate::catalog_data::CatalogRelease::Internal(metadata)) => {
            Ok(crate::catalog::fixture_selection(metadata))
        }
        _ => Err(crate::Error::invalid_input(
            "Fixture catalog must begin with an internal selection",
        )),
    }
}

#[cfg(unix)]
thread_local! {
    static FIXTURE_INTERVAL: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}
#[cfg(unix)]
pub(crate) fn fixture_interval() -> Option<u64> {
    FIXTURE_INTERVAL.with(|value| value.get())
}
#[cfg(unix)]
pub(crate) fn with_fixture_interval(run: impl FnOnce()) {
    struct Reset(Option<u64>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FIXTURE_INTERVAL.with(|v| v.set(self.0));
        }
    }
    let _reset = Reset(FIXTURE_INTERVAL.with(|v| v.replace(Some(1024))));
    run();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferenceFault {
    BeforePublish,
    BeforeCommit,
}
#[cfg(unix)]
pub(crate) enum PreferenceHooks {
    Fault {
        fault: PreferenceFault,
        ack: std::sync::mpsc::Sender<PreferenceFault>,
    },
    Hold {
        ack: std::sync::mpsc::Sender<()>,
        release: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
    },
}
#[cfg(unix)]
impl PreferenceHooks {
    pub(crate) fn save<T>(&self, save: impl FnOnce() -> T) -> T {
        use atomic_file::test_support::{Checkpoint, with_failure, with_observer};
        match self {
            Self::Fault { fault, ack } => {
                let fault = *fault;
                let checkpoint = match fault {
                    PreferenceFault::BeforePublish => Checkpoint::BeforePublish,
                    PreferenceFault::BeforeCommit => Checkpoint::BeforeCommit,
                };
                let ack = ack.clone();
                with_observer(
                    move |event| {
                        if event.checkpoint == checkpoint {
                            ack.send(fault).expect("preference checkpoint receiver");
                        }
                    },
                    || with_failure(checkpoint, save),
                )
            }
            Self::Hold { ack, release } => {
                let ack = ack.clone();
                let release = release.clone();
                with_observer(
                    move |event| {
                        if event.checkpoint == Checkpoint::BeforeCommit {
                            ack.send(()).expect("preference hold receiver");
                            release
                                .lock()
                                .unwrap()
                                .recv_timeout(std::time::Duration::from_secs(10))
                                .expect("preference hold must be released within ten seconds");
                        }
                    },
                    save,
                )
            }
        }
    }
}
