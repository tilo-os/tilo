//! Another way in, with a way back to signing in.
//!
//! After "Can't sign in", the volume opens the way every encrypted install
//! opens: systemd-cryptsetup tries its tokens and asks for the passphrase or
//! recovery key (or a security key's PIN). Its questions reach the screen
//! through a password agent, and Plymouth's own agent asks them again after
//! an empty answer, so from there nobody could get back to the sign-in
//! screen without restarting the computer.
//!
//! While the volume is attached that way, this agent stands in for
//! Plymouth's: it asks systemd-cryptsetup's questions on the boot screen as
//! they come, word for word, and passes the answers on. An empty answer
//! means "back": `tilo-root-discover` stops the attempt and shows the
//! sign-in screen again. Every other agent (another of the owner's devices,
//! through `tilo-sibling-unlock`) answers exactly as before.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tilo_ask_password::{Ask, answer, is_for, live, requests};
use zeroize::Zeroizing;

use crate::screen::Plymouth;

/// How the attempt ended, as far as this agent saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relayed {
    /// The attaching program is gone: the volume opened, or it gave up.
    Done,
    /// The person asked to go back to signing in.
    Back,
    /// Plymouth could not ask; its own agent should take over again.
    SteppedAside,
}

impl Relayed {
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::SteppedAside => 2,
            Self::Back => 3,
        }
    }
}

/// The message the theme shows the way back for, while it applies.
pub const OTHER_WAYS_ON: &str = "tilo-otherways:1";
pub const OTHER_WAYS_OFF: &str = "tilo-otherways:0";

pub struct Relay<'a> {
    pub plymouth: &'a Plymouth,
    /// Where systemd's password requests appear.
    pub ask_dir: PathBuf,
    /// The volume's device, as systemd-cryptsetup names it in its passphrase
    /// request.
    pub device: PathBuf,
    /// The systemd-cryptsetup attaching the volume. Its security-key PIN
    /// requests carry no device, only this process.
    pub pid: u32,
    /// How often a waiting request, and the program asking it, are looked at.
    pub poll: Duration,
}

enum Carried {
    Answer(Zeroizing<String>),
    Back,
    /// The request went away first (answered by another agent, or its
    /// program ended).
    Withdrawn,
    Failed,
}

impl Relay<'_> {
    #[must_use]
    pub fn run(&self) -> Relayed {
        self.plymouth.message(OTHER_WAYS_ON);
        // A request is asked once: after an answer systemd-cryptsetup asks
        // again, if it must, with a new request.
        let mut asked = Vec::new();
        eprintln!(
            "tilo-boot-signin: asking the questions of systemd-cryptsetup {} on the boot screen",
            self.pid
        );
        let outcome = loop {
            if !running(self.pid) {
                eprintln!("tilo-boot-signin: systemd-cryptsetup {} is done", self.pid);
                break Relayed::Done;
            }
            let Some(ask) = self.next_request(&asked) else {
                std::thread::sleep(self.poll);
                continue;
            };
            asked.push(ask.socket.clone());
            eprintln!("tilo-boot-signin: asking \"{}\"", ask.message);
            match self.carry(&ask) {
                Carried::Answer(reply) => match answer(&ask, &reply) {
                    Ok(()) => eprintln!("tilo-boot-signin: answer passed on"),
                    Err(error) => eprintln!("tilo-boot-signin: {error:#}"),
                },
                Carried::Back => {
                    eprintln!("tilo-boot-signin: empty answer: back to signing in");
                    break Relayed::Back;
                }
                Carried::Withdrawn => {
                    eprintln!("tilo-boot-signin: the question was answered elsewhere");
                }
                Carried::Failed => {
                    eprintln!("tilo-boot-signin: Plymouth could not ask; its agent takes over");
                    break Relayed::SteppedAside;
                }
            }
        };
        self.plymouth.message(OTHER_WAYS_OFF);
        outcome
    }

    fn ours(&self, ask: &Ask) -> bool {
        ask.pid == Some(self.pid) || is_for(ask, &self.device)
    }

    fn next_request(&self, asked: &[PathBuf]) -> Option<Ask> {
        requests(&self.ask_dir)
            .into_iter()
            .find(|ask| self.ours(ask) && !asked.contains(&ask.socket))
    }

    fn still_waiting(&self, ask: &Ask) -> bool {
        live(ask)
            && requests(&self.ask_dir)
                .iter()
                .any(|pending| pending.socket == ask.socket)
    }

    /// Ask `ask`'s question on the boot screen until it is answered there
    /// or withdrawn.
    fn carry(&self, ask: &Ask) -> Carried {
        let Ok(mut child) = Command::new(&self.plymouth.program)
            .arg("ask-for-password")
            .arg(format!("--prompt={}", ask.message))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return Carried::Failed;
        };
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let reply = read_reply(&mut child);
                    return match (status.success(), reply) {
                        (true, Some(reply)) if reply.is_empty() => Carried::Back,
                        (true, Some(reply)) => Carried::Answer(reply),
                        _ => Carried::Failed,
                    };
                }
                Ok(None) => {}
                Err(_) => return Carried::Failed,
            }
            if !self.still_waiting(ask) {
                let _ = child.kill();
                let _ = child.wait();
                return Carried::Withdrawn;
            }
            std::thread::sleep(self.poll);
        }
    }
}

/// Whether `pid` still runs. A program that has ended stays in the process
/// table until its parent collects it, and that parent (tilo-root-discover)
/// is waiting for this agent: an ended one counts as gone.
fn running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // The state follows the command name, which is in parentheses and may
    // itself contain them.
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.chars().next())
        .is_some_and(|state| !matches!(state, 'Z' | 'X'))
}

fn read_reply(child: &mut Child) -> Option<Zeroizing<String>> {
    let mut bytes = Zeroizing::new(Vec::new());
    child.stdout.take()?.read_to_end(&mut bytes).ok()?;
    let mut text = Zeroizing::new(String::from_utf8(bytes.to_vec()).ok()?);
    if text.ends_with('\n') {
        text.pop();
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixDatagram;

    use std::path::Path;

    use super::*;

    /// Plymouth's stand-in: `--ping` answers, display-message is logged, and
    /// ask-for-password logs its prompt and prints the next scripted entry
    /// (or waits, for `wait`).
    const PLYMOUTH: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
case "$1" in
  display-message) echo "$2" >>"$dir/messages" ;;
  ask-for-password)
    echo "$2" >>"$dir/prompts"
    n=$(cat "$dir/asked" 2>/dev/null || echo 0)
    echo $((n + 1)) >"$dir/asked"
    entry="$(sed -n "$((n + 1))p" "$dir/entries")"
    if [ "$entry" = wait ]; then exec sleep 30; fi
    printf '%s' "$entry"
    ;;
esac
"#;

    struct Fixture {
        dir: tempfile::TempDir,
        plymouth: Plymouth,
        attach: Child,
    }

    impl Fixture {
        fn new(entries: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let program = dir.path().join("plymouth");
            fs::write(&program, PLYMOUTH).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(dir.path().join("entries"), entries).unwrap();
            fs::create_dir(dir.path().join("ask")).unwrap();
            let attach = Command::new("sleep").arg("30").spawn().unwrap();
            Self {
                dir,
                plymouth: Plymouth { program },
                attach,
            }
        }

        fn relay(&self) -> Relay<'_> {
            relay(&self.plymouth, self.dir.path(), self.attach.id())
        }

        /// A request from the attaching program; returns its socket.
        fn ask(&self, name: &str, id: &str, message: &str) -> UnixDatagram {
            let socket = self.dir.path().join(format!("sck.{name}"));
            let receiver = UnixDatagram::bind(&socket).unwrap();
            fs::write(
                self.dir.path().join("ask").join(format!("ask.{name}")),
                format!(
                    "[Ask]\nPID={}\nSocket={}\nNotAfter=0\nMessage={message}\nId={id}\n",
                    self.attach.id(),
                    socket.display()
                ),
            )
            .unwrap();
            receiver
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
        }
    }

    fn relay<'a>(plymouth: &'a Plymouth, dir: &Path, pid: u32) -> Relay<'a> {
        Relay {
            plymouth,
            ask_dir: dir.join("ask"),
            device: PathBuf::from("/dev/vda3"),
            pid,
            poll: Duration::from_millis(10),
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.attach.kill();
            let _ = self.attach.wait();
        }
    }

    fn received(socket: &UnixDatagram) -> Vec<u8> {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buffer = [0_u8; 64];
        let n = socket.recv(&mut buffer).unwrap();
        buffer[..n].to_vec()
    }

    #[test]
    fn an_empty_answer_goes_back_to_signing_in() {
        let _serial = crate::test_support::serial();
        let fixture = Fixture::new("\n");
        let _socket = fixture.ask(
            "a",
            "cryptsetup:/dev/vda3",
            "Please enter passphrase for disk root:",
        );
        assert_eq!(fixture.relay().run(), Relayed::Back);
        assert_eq!(
            fixture.read("prompts"),
            "--prompt=Please enter passphrase for disk root:\n",
            "the question is systemd-cryptsetup's, word for word"
        );
        assert_eq!(
            fixture.read("messages"),
            format!("--text={OTHER_WAYS_ON}\n--text={OTHER_WAYS_OFF}\n")
        );
    }

    #[test]
    fn answers_reach_the_attaching_program_until_it_is_done() {
        let _serial = crate::test_support::serial();
        let mut fixture = Fixture::new("not it\nPIN-1234\n");
        let passphrase = fixture.ask(
            "a",
            "cryptsetup:/dev/vda3",
            "Please enter passphrase for disk root:",
        );
        let pin_socket = std::thread::scope(|scope| {
            let relay = relay(&fixture.plymouth, fixture.dir.path(), fixture.attach.id());
            let running = scope.spawn(move || relay.run());
            assert_eq!(received(&passphrase), b"+not it");
            fs::remove_file(fixture.dir.path().join("ask/ask.a")).unwrap();
            // A security key's PIN request names no device, only the program.
            let pin = fixture.ask("b", "", "Please enter security token PIN:");
            assert_eq!(received(&pin), b"+PIN-1234");
            fs::remove_file(fixture.dir.path().join("ask/ask.b")).unwrap();
            let _ = fixture.attach.kill();
            let _ = fixture.attach.wait();
            assert_eq!(running.join().unwrap(), Relayed::Done);
            pin
        });
        drop(pin_socket);
    }

    #[test]
    fn a_question_answered_elsewhere_leaves_the_screen_and_others_are_ignored() {
        let _serial = crate::test_support::serial();
        let mut fixture = Fixture::new("wait\n");
        // Another volume's request, from another program, is not ours.
        let other = fixture.dir.path().join("sck.other");
        let _other = UnixDatagram::bind(&other).unwrap();
        fs::write(
            fixture.dir.path().join("ask/ask.0"),
            format!(
                "[Ask]\nPID=1\nSocket={}\nNotAfter=0\nMessage=Other disk\nId=cryptsetup:/dev/vdb1\n",
                other.display()
            ),
        )
        .unwrap();
        let _socket = fixture.ask("a", "cryptsetup:/dev/vda3", "Passphrase:");
        std::thread::scope(|scope| {
            let relay = relay(&fixture.plymouth, fixture.dir.path(), fixture.attach.id());
            let running = scope.spawn(move || relay.run());
            for _ in 0..500 {
                if fixture.read("asked").trim() == "1" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // Another of the owner's devices answered it.
            fs::remove_file(fixture.dir.path().join("ask/ask.a")).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            let _ = fixture.attach.kill();
            let _ = fixture.attach.wait();
            assert_eq!(running.join().unwrap(), Relayed::Done);
        });
        assert_eq!(fixture.read("prompts"), "--prompt=Passphrase:\n");
    }

    #[test]
    fn an_attach_that_ended_is_done_before_its_parent_collects_it() {
        let _serial = crate::test_support::serial();
        let mut fixture = Fixture::new("");
        fixture.attach.kill().unwrap();
        // Not waited for: it stays in the process table, ended.
        for _ in 0..500 {
            if !running(fixture.attach.id()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(Path::new(&format!("/proc/{}", fixture.attach.id())).exists());
        assert_eq!(fixture.relay().run(), Relayed::Done);
    }

    #[test]
    fn without_plymouth_answering_the_agent_steps_aside() {
        let _serial = crate::test_support::serial();
        let fixture = Fixture::new("");
        fs::write(&fixture.plymouth.program, "#!/bin/bash\nexit 1\n").unwrap();
        let _socket = fixture.ask("a", "cryptsetup:/dev/vda3", "Passphrase:");
        assert_eq!(fixture.relay().run(), Relayed::SteppedAside);
    }
}
