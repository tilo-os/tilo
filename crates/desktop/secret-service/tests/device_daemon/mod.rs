//! A stand-in for the device's daemon, for the tests that run the built
//! `tilo-secret-service`: it keeps a device key, as the daemon alone does, and
//! answers the device actions the Secret Service asks of it (seal and open a
//! person's vault, check a caller's proof) at a native endpoint, in the
//! daemon's own wire form. The service reaches it the way it reaches the real
//! daemon, by the profile `TILO_RUNTIME_PROFILE` names.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tilo_device_identity::login::{
    CHECK_CALLER_PROOF, OPEN_VAULT, SEAL_VAULT, SEALED_BY_ANOTHER_KEY,
};
use tilo_device_identity::persist::{IdentityPaths, load_or_create_device_key};
use tilo_seal::secret_service::{CallerKey, open_vault, seal_vault};

/// The variable a program finds a daemon's profile, and its sockets, by.
pub const PROFILE: &str = tilo_paths::machine::DAEMON_PROFILE_VAR;

/// A running stand-in, answering until it is dropped.
pub struct DeviceDaemon {
    profile: PathBuf,
    native: PathBuf,
    stopped: Arc<AtomicBool>,
}

impl DeviceDaemon {
    /// The daemon of the device whose key is under the identity root
    /// `identity` (made there when it has none), serving the profile
    /// `profile` for programs whose runtime directory is `runtime`: it
    /// listens where those programs look for a daemon of that profile.
    pub fn start(identity: &Path, profile: &Path, runtime: &Path) -> Self {
        let (key, _) = load_or_create_device_key(&IdentityPaths::under(identity)).unwrap();
        let seed = key.to_bytes();
        // Reading the key turned this process non-dumpable, as it does the
        // daemon. This process is also the tests' ordinary client, which the
        // service names by its executable: it is made dumpable again, as no
        // process that keeps a device key would be.
        // SAFETY: PR_SET_DUMPABLE takes scalar arguments only.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0) }, 0);
        std::fs::create_dir_all(profile).unwrap();
        let sockets = {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", runtime);
            tilo_device_identity::login::daemon::profile_sockets(identity, profile).unwrap()
        };
        let native = tilo_device_identity::login::daemon::native_in(&sockets);
        let listener = UnixListener::bind(&native).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stopped);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(stream) = stream {
                    answer(&seed, stream);
                }
            }
        });
        Self {
            profile: profile.to_owned(),
            native,
            stopped,
        }
    }

    /// The profile a program names (`PROFILE`) to reach this daemon.
    pub fn profile(&self) -> &Path {
        &self.profile
    }
}

impl Drop for DeviceDaemon {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wake the listener so it sees it stopped.
        let _ = UnixStream::connect(&self.native);
    }
}

/// Answer the one call on `stream` with the device key `seed`.
fn answer(seed: &[u8; 32], stream: UnixStream) {
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let call: Value = serde_json::from_str(line.trim()).unwrap();
    let arguments = &call["params"]["arguments"];
    let path = arguments["path"].as_str().unwrap_or_default();
    let action = path.rsplit_once('#').map_or(path, |(_, action)| action);
    let typed = match act(seed, action, &arguments["args"]) {
        Ok(result) => json!({ "status": "ok", "result": result }),
        Err((code, message)) => {
            json!({ "status": "failed", "error": { "code": code, "message": message } })
        }
    };
    let failed = typed["status"] != "ok";
    let reply = json!({
        "jsonrpc": "2.0",
        "id": call["id"],
        "result": {
            "content": [{ "type": "text", "text": "" }],
            "structuredContent": typed,
            "isError": failed,
        },
    });
    let _ = writeln!(&stream, "{reply}");
}

fn hex_arg(args: &Value, name: &str) -> Vec<u8> {
    hex::decode(args[name].as_str().unwrap()).unwrap()
}

fn act(seed: &[u8; 32], action: &str, args: &Value) -> Result<Value, (&'static str, String)> {
    match action {
        SEAL_VAULT | OPEN_VAULT => {
            let salt: [u8; 32] = hex_arg(args, "salt").try_into().unwrap();
            let account = args["account"].as_str().unwrap().as_bytes();
            if action == SEAL_VAULT {
                let sealed = seal_vault(seed, &salt, account, &hex_arg(args, "plain")).unwrap();
                return Ok(json!({ "sealed": hex::encode(sealed) }));
            }
            match open_vault(seed, &salt, account, &hex_arg(args, "sealed")) {
                Ok(plain) => Ok(json!({ "plain": hex::encode(plain.as_slice()) })),
                Err(tilo_seal::Error::WrongKey) => Err((
                    SEALED_BY_ANOTHER_KEY,
                    "the vault was sealed under another device's key".to_owned(),
                )),
                Err(error) => Err((
                    tilo_device_identity::login::MALFORMED_ASK,
                    error.to_string(),
                )),
            }
        }
        CHECK_CALLER_PROOF => {
            let proven = CallerKey::derive(seed).unwrap().verify(
                &hex_arg(args, "challenge"),
                args["caller"].as_str().unwrap(),
                &hex_arg(args, "program"),
                &hex_arg(args, "proof"),
            );
            Ok(json!({ "proven": proven }))
        }
        other => Err(("refused", format!("no device action {other}"))),
    }
}
