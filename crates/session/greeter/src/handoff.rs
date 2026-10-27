//! The boot sign-in handoff: who, if anyone, signed in before the system
//! volume opened.
//!
//! A person who signs in on the boot screen should not be asked again here.
//! `tilo-authd` remembers that sign-in once; the greeter asks it for the
//! person's name when it starts and, given one, starts their session
//! straight away (see [`crate::auth::spawn_handoff`]). `pam_tilo` is what
//! accepts the sign-in, so the name alone grants nothing: it only says whose
//! session to try.
//!
//! Anything unexpected (no daemon, no answer, an answer that is not a plain
//! account name) reads as "nobody", and the ordinary sign-in screen shows.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

/// `tilo-authd`'s socket.
pub const DEFAULT_SOCKET: &str = "/run/tilo-auth/socket";

/// How long the greeter waits for the daemon before showing the ordinary
/// screen. The daemon answers from memory; this only keeps a wedged daemon
/// from holding the sign-in screen back.
const EXCHANGE_BOUND: Duration = Duration::from_secs(2);

/// The longest reply line read. A name is at most 32 bytes.
const REPLY_LIMIT: u64 = 4096;

/// Present for the rest of a boot on which the installer's one-boot
/// autologin started the new account's session
/// (`tilo-firstboot-autologin-cleanup.service` creates it).
pub const FIRST_BOOT_AUTOLOGIN: &str = "/run/tilo-firstboot-autologin";

/// The person who signed in at boot and has not started a session yet.
///
/// Nobody, on a boot whose first session was the installer's autologin:
/// one boot starts at most one session without a question. The greeter only
/// runs then once that session has ended, and signing out must lead to a
/// sign-in, not straight back in.
#[must_use]
pub fn pending_sign_in(socket: &Path, first_boot_autologin: &Path) -> Option<String> {
    if first_boot_autologin.exists() {
        return None;
    }
    let line = exchange(socket, b"{\"op\":\"pending-sign-in\"}\n", EXCHANGE_BOUND)?;
    parse_reply(&line)
}

/// Whether `tilo-authd` holds `user`'s key for a session of theirs running
/// now: the only time this screen may offer their fingerprint (they are
/// switching back to a session), because a fingerprint unwraps nothing.
/// No daemon, no answer or anything unexpected reads as "no".
#[must_use]
pub fn key_held(socket: &Path, user: &str) -> bool {
    let Ok(mut request) = serde_json::to_vec(&serde_json::json!({"op": "key-held", "user": user}))
    else {
        return false;
    };
    request.push(b'\n');
    exchange(socket, &request, EXCHANGE_BOUND).is_some_and(|line| {
        serde_json::from_str::<Value>(line.trim()).is_ok_and(|reply| {
            reply.get("ok") == Some(&Value::Bool(true))
                && reply.get("key_held") == Some(&Value::Bool(true))
        })
    })
}

fn exchange(socket: &Path, request: &[u8], bound: Duration) -> Option<String> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(bound)).ok()?;
    stream.set_write_timeout(Some(bound)).ok()?;
    stream.write_all(request).ok()?;
    let mut line = String::new();
    BufReader::new(stream.take(REPLY_LIMIT))
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

/// `null`, or `{"name": "..."}` naming a local account.
fn parse_reply(line: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(line.trim()).ok()?;
    let name = reply.get("name")?.as_str()?;
    plain_account_name(name).then(|| name.to_owned())
}

/// The names `useradd` accepts by default: a letter or underscore, then
/// letters, digits, `_`, `-` or `.`, at most 32 bytes.
fn plain_account_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && name.len() <= 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn daemon(reply: Option<&'static str>) -> (tempfile::TempDir, std::thread::JoinHandle<String>) {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("socket")).unwrap();
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            match reply {
                // A caller that hung up without asking gets nothing.
                Some(reply) => {
                    if !request.is_empty() {
                        (&stream).write_all(reply.as_bytes()).unwrap();
                    }
                }
                // Hold the connection open without answering.
                None => std::thread::sleep(Duration::from_millis(400)),
            }
            request
        });
        (dir, served)
    }

    #[test]
    fn the_daemon_names_the_person_who_signed_in_at_boot() {
        let (dir, served) = daemon(Some("{\"name\":\"ada\"}\n"));
        assert_eq!(
            pending_sign_in(&dir.path().join("socket"), &dir.path().join("autologin")).as_deref(),
            Some("ada")
        );
        assert_eq!(served.join().unwrap(), "{\"op\":\"pending-sign-in\"}\n");
    }

    #[test]
    fn nobody_pending_no_daemon_or_no_answer_shows_the_ordinary_screen() {
        let (dir, served) = daemon(Some("null\n"));
        assert_eq!(
            pending_sign_in(&dir.path().join("socket"), &dir.path().join("autologin")),
            None
        );
        served.join().unwrap();

        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            pending_sign_in(
                &empty.path().join("socket"),
                &empty.path().join("autologin")
            ),
            None
        );

        let (dir, served) = daemon(None);
        assert_eq!(
            exchange(
                &dir.path().join("socket"),
                b"{\"op\":\"pending-sign-in\"}\n",
                Duration::from_millis(50)
            ),
            None
        );
        served.join().unwrap();
    }

    #[test]
    fn a_fingerprint_is_offered_only_when_the_daemon_says_the_key_is_held() {
        let (dir, served) = daemon(Some("{\"ok\":true,\"key_held\":true}\n"));
        assert!(key_held(&dir.path().join("socket"), "ada"));
        assert_eq!(
            served.join().unwrap(),
            "{\"op\":\"key-held\",\"user\":\"ada\"}\n"
        );

        for reply in [
            "{\"ok\":true,\"key_held\":false}\n",
            "{\"ok\":false,\"error\":\"not-permitted\"}\n",
            "{\"ok\":true}\n",
        ] {
            let (dir, served) = daemon(Some(reply));
            assert!(!key_held(&dir.path().join("socket"), "ada"), "{reply}");
            served.join().unwrap();
        }
        let empty = tempfile::tempdir().unwrap();
        assert!(!key_held(&empty.path().join("socket"), "ada"));
    }

    #[test]
    fn a_boot_whose_first_session_was_the_installer_autologin_hands_nobody_over() {
        let (dir, served) = daemon(Some("{\"name\":\"ada\"}\n"));
        let autologin = dir.path().join("autologin");
        std::fs::create_dir(&autologin).unwrap();
        assert_eq!(
            pending_sign_in(&dir.path().join("socket"), &autologin),
            None
        );
        // The daemon was not even asked: its first connection is this one.
        drop(UnixStream::connect(dir.path().join("socket")).unwrap());
        assert_eq!(served.join().unwrap(), "");
    }

    /// greetd's framing: a little-endian length, then JSON.
    fn read_request(stream: &mut UnixStream) -> Value {
        let mut len = [0; 4];
        stream.read_exact(&mut len).unwrap();
        let mut bytes = vec![0; usize::try_from(u32::from_le_bytes(len)).unwrap()];
        stream.read_exact(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn respond(stream: &mut UnixStream, value: &Value) {
        let bytes = serde_json::to_vec(value).unwrap();
        stream
            .write_all(&u32::try_from(bytes.len()).unwrap().to_le_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
    }

    fn greetd(
        serve: impl FnOnce(&mut UnixStream) + Send + 'static,
    ) -> (tempfile::TempDir, std::thread::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("greetd.sock")).unwrap();
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            serve(&mut stream);
        });
        (dir, served)
    }

    fn session_argv() -> Vec<String> {
        vec!["/usr/libexec/tilo-session".to_owned()]
    }

    #[test]
    fn a_question_from_pam_cancels_the_handoff_unanswered() {
        let (dir, served) = greetd(|stream| {
            assert_eq!(
                read_request(stream),
                serde_json::json!({"type":"create_session","username":"ada"})
            );
            respond(
                stream,
                &serde_json::json!({"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}),
            );
            assert_eq!(
                read_request(stream),
                serde_json::json!({"type":"cancel_session"})
            );
            respond(stream, &serde_json::json!({"type":"success"}));
        });
        let (sender, updates) = std::sync::mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let _session = crate::auth::spawn_handoff_updates(
            dir.path().join("greetd.sock").to_str().unwrap().to_owned(),
            "ada".to_owned(),
            session_argv(),
            Vec::new(),
            move |update| sender.lock().unwrap().send(update).unwrap(),
        );
        assert_eq!(
            updates.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
            crate::auth::AuthUpdate::Failed(crate::auth::Failure::Cancelled)
        );
        served.join().unwrap();
    }

    #[test]
    fn an_accepted_boot_sign_in_starts_the_session_without_a_question() {
        // A started session exits the greeter, as greetd requires; run that
        // in a child so the exit does not end this test binary.
        let (dir, served) = greetd(|stream| {
            assert_eq!(
                read_request(stream),
                serde_json::json!({"type":"create_session","username":"ada"})
            );
            respond(
                stream,
                &serde_json::json!({"type":"auth_message","auth_message_type":"info","auth_message":"Welcome back"}),
            );
            let answer = read_request(stream);
            assert_eq!(answer["type"], "post_auth_message_response");
            assert!(answer["response"].is_null(), "{answer}");
            respond(stream, &serde_json::json!({"type":"success"}));
            assert_eq!(
                read_request(stream),
                serde_json::json!({"type":"start_session","cmd":["/usr/libexec/tilo-session"],"env":[]})
            );
            respond(stream, &serde_json::json!({"type":"success"}));
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handoff::tests::handoff_start_child",
                "--nocapture",
            ])
            .env(
                "TILO_GREETER_HANDOFF_FIXTURE",
                dir.path().join("greetd.sock"),
            )
            .output()
            .unwrap();
        served.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("accepted"),
            "the session started through the acceptance hook"
        );
    }

    #[test]
    fn handoff_start_child() {
        let Some(socket) = std::env::var_os("TILO_GREETER_HANDOFF_FIXTURE") else {
            return;
        };
        let (sender, updates) = std::sync::mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let _session = crate::auth::spawn_handoff_updates(
            socket.into_string().unwrap(),
            "ada".to_owned(),
            session_argv(),
            Vec::new(),
            move |update| {
                // Report acceptance from the hook itself: the worker goes on
                // to start the session and exit the process, which can happen
                // before another thread prints what it was sent.
                if update == crate::auth::AuthUpdate::Accepted {
                    println!("accepted");
                }
                sender.lock().unwrap().send(update).unwrap();
            },
        );
        assert_eq!(
            updates.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
            crate::auth::AuthUpdate::Accepted
        );
        // The worker exits the process once greetd starts the session.
        let after = updates.recv_timeout(tilo_test_env::hang_bound());
        panic!("the handoff returned instead of starting the session: {after:?}");
    }

    #[test]
    fn only_a_plain_account_name_is_taken() {
        for (reply, expected) in [
            ("{\"name\":\"ada\"}", Some("ada")),
            ("{\"name\":\"_svc.ben-2\"}", Some("_svc.ben-2")),
            ("{\"name\":\"ada\",\"since\":12}", Some("ada")),
            ("null", None),
            ("{}", None),
            ("{\"name\":null}", None),
            ("{\"name\":\"\"}", None),
            ("{\"name\":\"-rf\"}", None),
            ("{\"name\":\"ada lovelace\"}", None),
            ("{\"name\":\"../root\"}", None),
            ("{\"name\":\"a\\nb\"}", None),
            ("{\"name\":\"abcdefghijklmnopqrstuvwxyz0123456\"}", None),
            ("not json", None),
        ] {
            assert_eq!(parse_reply(reply).as_deref(), expected, "{reply}");
        }
    }
}
