//! A device's daemon as a test's machine runs it.
//!
//! A machine with an identity enrolled on it runs one process that makes and takes the device key
//! ([`crate::persist::take_device_key`]), and every program of the machine reads the device
//! through it ([`DeviceActions::here`]): nothing else opens the key. A test whose machine has no
//! real daemon starts this one: it takes the machine's key, and answers the device actions that
//! need nothing but the key, at the endpoint the machine's programs resolve.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::Value;

use crate::IdentityPaths;
use crate::login::daemon::DeviceActions;
use crate::persist::{self, DeviceKeyLock};
use tilo_files::native::socket::{self, Listener};

/// The action [`DeviceActions::device_key`] asks: what this device is.
const INFO: &str = "info";

/// The device's daemon of a test's machine, for as long as it lives: it holds the machine's
/// device key, and answers its programs' device actions at the endpoint they resolve.
#[must_use = "the daemon answers for as long as it lives"]
pub struct DeviceDaemon {
    /// The key the daemon took, and signs with while it answers.
    key: DeviceKeyLock,
    /// The endpoint the machine's programs reach this daemon at.
    native: PathBuf,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DeviceDaemon {
    /// The daemon of the machine whose device key and certificates are `paths`: it makes the
    /// machine's key when it has none (as `tilo-device-key-init` does at boot), takes it, and
    /// answers at [`DeviceActions::here`]'s endpoint for `paths`.
    ///
    /// # Errors
    /// The key cannot be made or is taken by another process, or the endpoint cannot be bound.
    pub fn start(paths: &IdentityPaths) -> std::io::Result<Self> {
        let refused = |error: persist::PersistError| std::io::Error::other(error.to_string());
        persist::load_or_create_device_key(paths).map_err(refused)?;
        let native = DeviceActions::here(paths.clone())
            .map_err(std::io::Error::other)?
            .native()
            .to_path_buf();
        // Bound before the key is taken: a machine whose endpoint a live daemon holds refuses
        // here, rather than this process holding the key of a daemon that is not here.
        let listener = socket::bind(&native)?;
        let key = persist::take_device_key(paths).map_err(refused)?;
        let signing = key.key().clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stopping);
        let thread = std::thread::spawn(move || serve(&listener, &signing, &stopped));
        Ok(Self {
            key,
            native,
            stopping,
            thread: Some(thread),
        })
    }

    /// This device's public key: the one its daemon signs with.
    #[must_use]
    pub fn device_key(&self) -> [u8; 32] {
        self.key.key().verifying_key().to_bytes()
    }

    /// The device actions of this daemon, as the machine's programs ask them: at its endpoint.
    #[must_use]
    pub fn actions(&self) -> DeviceActions {
        DeviceActions::at(self.native.clone())
    }
}

impl Drop for DeviceDaemon {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        // The accept that waits is woken by a connection, which the loop then sees is the stop.
        let _ = socket::connect(&self.native);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // A socket's name stays behind after it closes on Unix; the next daemon there takes it.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.native);
    }
}

/// Answer this daemon's questions with `key` until `stopping` says the test is done.
fn serve(listener: &Listener, key: &SigningKey, stopping: &AtomicBool) {
    for stream in listener.incoming() {
        if stopping.load(Ordering::Relaxed) {
            return;
        }
        let Ok(stream) = stream else { return };
        let mut request = String::new();
        if std::io::BufReader::new(&stream)
            .read_line(&mut request)
            .is_err()
        {
            continue;
        }
        let _ = writeln!(&stream, "{}", answer(&request, key));
    }
}

/// The daemon's answer, as the device action service writes it ([`crate::login::daemon`]), to the
/// request line `request`.
fn answer(request: &str, key: &SigningKey) -> Value {
    let asked: Value = serde_json::from_str(request).unwrap_or_default();
    let path = asked
        .pointer("/params/arguments/path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = asked
        .pointer("/params/arguments/args")
        .cloned()
        .unwrap_or_default();
    // `devices#info`: the resource is the person's devices, the action the part after the `#`.
    let action = path.rsplit('#').next().unwrap_or_default();
    match act(action, &args, key) {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{"type": "text", "text": ""}],
                "structuredContent": {"status": "ok", "result": result},
            },
        }),
        Err(message) => {
            let message = message.to_string();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": message}],
                    "isError": true,
                    "structuredContent": {
                        "status": "error",
                        "error": {"code": crate::login::daemon::REFUSED, "message": message},
                    },
                },
            })
        }
    }
}

/// What the device action `action` answers with `key`, or why this daemon does not answer it.
fn act(action: &str, args: &Value, key: &SigningKey) -> Result<Value, &'static str> {
    match action {
        INFO => Ok(serde_json::json!({
            "device": hex::encode(key.verifying_key().to_bytes()),
        })),
        crate::login::SIGN => {
            let message = args
                .get("message")
                .and_then(Value::as_str)
                .ok_or("the sign action asks no message")?;
            let message = hex::decode(message).map_err(|_| "the message is not hex")?;
            Ok(serde_json::json!({
                "signature": hex::encode(key.sign(&message).to_bytes()),
            }))
        }
        _ => Err("this machine's daemon answers no such device action"),
    }
}

/// What this daemon answers, as the daemon's clients read its answers.
#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon of a machine answers what the device is and signs with the key it took, and
    /// refuses an action it does not serve, as the daemon's clients read those answers.
    #[test]
    fn a_machines_daemon_answers_what_the_device_is_and_signs_with_its_key() {
        // This machine's own home and runtime directory: the endpoint is not another test's.
        let machine = tilo_test_env::TestHome::new().unwrap();
        let paths = IdentityPaths::under(machine.root().join("device"));
        let daemon = DeviceDaemon::start(&paths).unwrap();
        let actions = daemon.actions();
        assert_eq!(actions.device_key().unwrap(), daemon.device_key());
        let signature = actions.sign(b"a message").unwrap();
        ed25519_dalek::Verifier::verify(
            &ed25519_dalek::VerifyingKey::from_bytes(&daemon.device_key()).unwrap(),
            b"a message",
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .unwrap();
        assert!(actions.ask("sealVault", serde_json::json!({})).is_err());
    }
}
