//! The device's sign-in comforts, which this screen starts with and changes
//! when someone turns one on or off here (docs/security/sign-in.md §3.4).
//! `tilo-authd` keeps them and mirrors them into the volume header, so the
//! boot screen starts with them too.
//!
//! No daemon, no answer or anything unexpected reads as the defaults: the
//! comforts are preferences, never a reason to keep the screen back.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::{Map, Value};
use settings_store::sign_in_comforts::SignInComforts;

use crate::handoff::{EXCHANGE_BOUND, exchange};

/// The comforts `tilo-authd` keeps, clamped; `None` when none were chosen
/// or the daemon does not answer.
#[must_use]
pub fn load(socket: &Path) -> Option<SignInComforts> {
    let line = exchange(socket, b"{\"op\":\"sign-in-comforts\"}\n", EXCHANGE_BOUND)?;
    let reply: Value = serde_json::from_str(line.trim()).ok()?;
    reply
        .is_object()
        .then(|| SignInComforts::from_value(&reply))
}

/// Change the fields `changes` names; whether the daemon took them.
fn change(socket: &Path, changes: &Map<String, Value>) -> bool {
    let Ok(mut request) = serde_json::to_vec(&serde_json::json!({
        "op": "set-sign-in-comforts",
        "comforts": changes,
    })) else {
        return false;
    };
    request.push(b'\n');
    exchange(socket, &request, EXCHANGE_BOUND).is_some_and(|line| {
        serde_json::from_str::<Value>(line.trim())
            .is_ok_and(|reply| reply.get("ok") == Some(&Value::Bool(true)))
    })
}

/// Sends each change to the daemon in order, off the screen's thread.
pub struct Changes {
    sender: Option<mpsc::Sender<Map<String, Value>>>,
}

impl Changes {
    pub fn to(socket: PathBuf) -> Self {
        let (sender, changes) = mpsc::channel::<Map<String, Value>>();
        let started = std::thread::Builder::new()
            .name("sign-in-comforts".into())
            .spawn(move || {
                for comforts in changes {
                    if !change(&socket, &comforts) {
                        tracing::warn!("tilo-authd did not keep the login screen's comforts");
                    }
                }
            });
        Self {
            sender: started.is_ok().then_some(sender),
        }
    }

    /// Keep `field` at `value` for the login screens from now on.
    pub fn set(&self, field: &str, value: impl Into<Value>) {
        let mut changes = Map::new();
        changes.insert(field.to_owned(), value.into());
        if let Some(sender) = &self.sender {
            let _ = sender.send(changes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    fn daemon(
        replies: &'static [&'static str],
    ) -> (tempfile::TempDir, std::thread::JoinHandle<Vec<String>>) {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("socket")).unwrap();
        let served = std::thread::spawn(move || {
            replies
                .iter()
                .map(|reply| {
                    let (stream, _) = listener.accept().unwrap();
                    let mut request = String::new();
                    BufReader::new(stream.try_clone().unwrap())
                        .read_line(&mut request)
                        .unwrap();
                    (&stream).write_all(reply.as_bytes()).unwrap();
                    request
                })
                .collect()
        });
        (dir, served)
    }

    #[test]
    fn the_screen_starts_with_the_comforts_the_daemon_keeps_clamped() {
        let (dir, served) = daemon(&[
            "{\"screen_reader\":true,\"screen_reader_rate\":900,\"text_scale\":1.5}\n",
            "null\n",
        ]);
        let socket = dir.path().join("socket");
        assert_eq!(
            load(&socket),
            Some(SignInComforts {
                screen_reader: true,
                screen_reader_rate: 200,
                text_scale: 1.5,
                ..SignInComforts::default()
            })
        );
        assert_eq!(load(&socket), None);
        assert_eq!(
            served.join().unwrap(),
            ["{\"op\":\"sign-in-comforts\"}\n"; 2]
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(load(&empty.path().join("socket")), None);
    }

    #[test]
    fn a_toggle_here_is_kept_for_the_login_screens() {
        let (dir, served) = daemon(&["{\"ok\":true}\n", "{\"ok\":true}\n"]);
        let changes = Changes::to(dir.path().join("socket"));
        changes.set("screen_reader", true);
        changes.set("text_scale", 1.5);
        let requests: Vec<Value> = served
            .join()
            .unwrap()
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests,
            [
                serde_json::json!({"op": "set-sign-in-comforts", "comforts": {"screen_reader": true}}),
                serde_json::json!({"op": "set-sign-in-comforts", "comforts": {"text_scale": 1.5}}),
            ]
        );
    }
}
