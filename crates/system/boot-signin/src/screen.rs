//! The boot screen. While Plymouth draws, the secret is asked on it directly
//! and the theme is told who is signing in and what to say, so it can lay the
//! screen out like the login screen (os/usr/share/plymouth/themes/tilo).
//! Without Plymouth the secret is asked through systemd's password agents, so
//! it works on a serial console too.
//!
//! On Plymouth the question cannot go through systemd-ask-password: its
//! Plymouth agent takes an empty answer as no answer and asks again, so the
//! empty Enter that means "Can't sign in" would never arrive.
//!
//! The theme reads `display-message` texts of the form `tilo-signin-<what>:…`.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use zeroize::Zeroizing;

use crate::flow::{Entry, Pick, Prompt, Screen};
use crate::people::Person;

pub struct BootScreen {
    pub ask_password: PathBuf,
    /// Plymouth, when it is drawing the boot.
    pub plymouth: Option<Plymouth>,
    pub volume: String,
}

impl BootScreen {
    fn tell(&self, message: &str) {
        if let Some(plymouth) = &self.plymouth {
            plymouth.message(message);
        }
    }

    fn ask_password(&self, id: &str, echo: &str, prompt: &str) -> Option<Zeroizing<String>> {
        let output = Command::new(&self.ask_password)
            .arg("-n")
            .arg(format!("--echo={echo}"))
            .arg("--timeout=0")
            .arg(format!("--id={id}:{}", self.volume))
            .arg(prompt)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .ok()?;
        let stdout = Zeroizing::new(output.stdout);
        if !output.status.success() {
            return None;
        }
        let text = std::str::from_utf8(&stdout).ok()?;
        Some(Zeroizing::new(text.to_owned()))
    }

    /// The list as one question, where nothing draws a list.
    fn pick_by_typing(&self, people: &[Person]) -> Pick {
        let choices: Vec<String> = people
            .iter()
            .enumerate()
            .map(|(index, person)| {
                format!("{} for {}", index.saturating_add(1), person.display_name)
            })
            .collect();
        let question = format!(
            "Who is signing in? Type {}, or leave it empty for other ways to sign in",
            choices.join(", ")
        );
        loop {
            let answer = match &self.plymouth {
                Some(plymouth) => plymouth.ask("ask-question", &question),
                None => self.ask_password("tilo-signin-person", "yes", &question),
            };
            let Some(answer) = answer else {
                return Pick::OtherWays;
            };
            let answer = answer.trim();
            if answer.is_empty() {
                return Pick::OtherWays;
            }
            if let Some(index) = answer
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1))
                .filter(|index| *index < people.len())
            {
                return Pick::Person(index);
            }
        }
    }
}

impl Screen for BootScreen {
    fn pick(&mut self, people: &[Person], highlighted: usize) -> Pick {
        let Some(plymouth) = &self.plymouth else {
            return self.pick_by_typing(people);
        };
        // Keys reach a watch only while the splash is up; a password prompt
        // raises it by itself, a list does not.
        plymouth.command(&["show-splash"]);
        plymouth.message(&format!("tilo-signin-people:{}", people.len()));
        for (index, person) in people.iter().enumerate() {
            plymouth.message(&format!(
                "tilo-signin-person:{index}:{}",
                person.display_name
            ));
        }
        // The last row is "Can't sign in".
        let last = people.len();
        let mut row = highlighted.min(last);
        loop {
            plymouth.message(&format!("tilo-signin-choose:{row}"));
            match plymouth.key() {
                Some(Key::Next) => {
                    row = if row >= last {
                        0
                    } else {
                        row.saturating_add(1)
                    }
                }
                Some(Key::Previous) => row = row.checked_sub(1).unwrap_or(last),
                Some(Key::Choose) if row < last => return Pick::Person(row),
                Some(Key::Choose) => return Pick::OtherWays,
                Some(Key::Other) => {}
                None => return self.pick_by_typing(people),
            }
        }
    }

    fn ask(&mut self, prompt: &Prompt<'_>) -> Entry {
        let person = prompt.person;
        let label = person.secret.label();
        self.tell(&format!("tilo-signin-name:{}", person.display_name));
        self.tell(&format!("tilo-signin-kind:{}", person.secret.wire()));
        self.tell(&format!("tilo-signin-back:{}", u8::from(prompt.several)));
        let note = prompt.note.text();
        self.tell(&format!("tilo-signin-note:{}:{note}", prompt.note.class()));
        let empty = if prompt.several {
            "leave it empty to choose someone else"
        } else {
            "leave it empty for other ways to sign in"
        };
        let mut text = format!("{label} for {}, or {empty}", person.display_name);
        if !note.is_empty() {
            text = format!("{note} {text}");
        }
        let answer = match &self.plymouth {
            Some(plymouth) => plymouth.ask("ask-for-password", &text),
            None => self.ask_password("tilo-signin", "masked", &text),
        };
        let entry = match answer {
            Some(secret) if secret.is_empty() => Entry::Empty,
            Some(secret) => Entry::Secret(secret),
            None => Entry::Unavailable,
        };
        let what = match &entry {
            Entry::Secret(_) => "an entry",
            Entry::Empty => "an empty entry",
            Entry::Unavailable => "no answer (the question could not be asked)",
        };
        eprintln!("tilo-boot-signin: {} gave {what}", person.name);
        entry
    }

    fn checking(&mut self, _person: &Person) {
        self.tell("tilo-signin-checking");
    }

    fn finish(&mut self, _unlocked: bool) {
        self.tell("tilo-signin-end");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Next,
    Previous,
    Choose,
    Other,
}

/// Keys that move through the list: Tab and the arrow keys, as Plymouth
/// passes them on whole.
const MOVE_KEYS: &str = "\t\u{1b}[A\u{1b}[B\u{1b}[C\u{1b}[D";
const ENTER_KEYS: &str = "\n";

pub struct Plymouth {
    pub program: PathBuf,
}

impl Plymouth {
    /// Plymouth, if it is running.
    pub fn running(program: PathBuf) -> Option<Self> {
        let pinged = Command::new(&program)
            .arg("--ping")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        pinged.then_some(Self { program })
    }

    /// Ask on the boot screen: `ask-for-password` (masked) or
    /// `ask-question`. An empty Enter answers empty. `None` when Plymouth
    /// could not ask.
    pub(crate) fn ask(&self, how: &str, prompt: &str) -> Option<Zeroizing<String>> {
        let output = Command::new(&self.program)
            .arg(how)
            .arg(format!("--prompt={prompt}"))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let stdout = Zeroizing::new(output.stdout);
        if !output.status.success() {
            return None;
        }
        let text = std::str::from_utf8(&stdout).ok()?;
        Some(Zeroizing::new(text.to_owned()))
    }

    pub(crate) fn message(&self, text: &str) {
        self.command(&["display-message", &format!("--text={text}")]);
    }

    fn command(&self, args: &[&str]) {
        let _ = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// The next key on the list screen; `None` when Plymouth cannot report
    /// keys.
    ///
    /// Two watches run at once: Plymouth reports Enter with whatever was
    /// typed since the last Enter, so a single watch could not tell a Tab
    /// from an Enter after a Tab. Once one answers, the other is withdrawn.
    pub fn key(&self) -> Option<Key> {
        let (sender, receiver) = mpsc::channel();
        let watches: Vec<_> = [(MOVE_KEYS, false), (ENTER_KEYS, true)]
            .into_iter()
            .map(|(keys, enter)| {
                let sender = sender.clone();
                let program = self.program.clone();
                std::thread::spawn(move || {
                    let output = Command::new(&program)
                        .arg("watch-keystroke")
                        .arg(format!("--keys={keys}"))
                        .stdin(Stdio::null())
                        .stderr(Stdio::null())
                        .output();
                    let answer = match output {
                        Ok(output) if output.status.success() => {
                            Some(String::from_utf8_lossy(&output.stdout).into_owned())
                        }
                        _ => None,
                    };
                    let _ = sender.send((enter, keys, answer));
                })
            })
            .collect();
        drop(sender);
        let first = receiver.recv().ok();
        if let Some((_, keys, _)) = &first {
            let other = if *keys == MOVE_KEYS {
                ENTER_KEYS
            } else {
                MOVE_KEYS
            };
            self.command(&["ignore-keystroke", &format!("--keys={other}")]);
        }
        for watch in watches {
            let _ = watch.join();
        }
        match first? {
            (_, _, None) => None,
            (true, _, Some(_)) => Some(Key::Choose),
            (false, _, Some(key)) => Some(match key.as_str() {
                "\t" | "\u{1b}[B" | "\u{1b}[C" => Key::Next,
                "\u{1b}[A" | "\u{1b}[D" => Key::Previous,
                _ => Key::Other,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    use super::*;
    use crate::flow::Note;
    use crate::people::SecretKind;

    /// A plymouth that logs messages and answers watches from a script of
    /// keys: `tab`, `up` or `enter`, one per line. The watch a key is not for
    /// waits until it is withdrawn, as Plymouth's does.
    const PLYMOUTH: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
case "$1" in
  --ping) exit 0 ;;
  ask-for-password|ask-question)
    printf '%s\n' "$*" >>"$dir/asked"
    answer="$(head -n1 "$dir/answers")"; sed -i 1d "$dir/answers"
    [ "$answer" = "<cancel>" ] && exit 1
    printf '%s' "$answer"
    ;;
  display-message) printf '%s\n' "${2#--text=}" >>"$dir/messages" ;;
  ignore-keystroke)
    if [ "$2" = $'--keys=\n' ]; then other=enter; else other=move; fi
    pid="$(cat "$dir/$other.pid")"; rm -f "$dir/$other.pid"; kill "$pid"
    ;;
  watch-keystroke)
    if [ "$2" = $'--keys=\n' ]; then mine=enter other=move; else mine=move other=enter; fi
    # The first of a round's two watches takes the next key and decides
    # which of them answers; the second reads that decision.
    exec 9>>"$dir/lock"; flock 9
    if [ -e "$dir/decided" ]; then
      read -r next for_me <"$dir/decided"; rm "$dir/decided"
    else
      next="$(head -n1 "$dir/keys")"; sed -i 1d "$dir/keys"
      case "$next" in enter) for_me=enter ;; *) for_me=move ;; esac
      echo "$next $for_me" >"$dir/decided"
    fi
    flock -u 9
    if [ "$mine" = "$for_me" ]; then
      while [ ! -s "$dir/$other.pid" ]; do sleep 0.01; done
      case "$next" in tab) printf '\t' ;; up) printf '\033[A' ;; enter) printf '' ;; esac
      exit 0
    fi
    sleep 30 9>&- & echo $! >"$dir/$mine.tmp"; mv "$dir/$mine.tmp" "$dir/$mine.pid"; wait $!; exit 1
    ;;
esac
"#;

    /// An ask-password agent that answers from a script, one line each,
    /// and logs what it was asked.
    const ASK: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
printf '%s\n' "$*" >>"$dir/asked"
answer="$(head -n1 "$dir/answers")"; sed -i 1d "$dir/answers"
[ "$answer" = "<cancel>" ] && exit 1
printf '%s' "$answer"
"#;

    fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn screen(dir: &Path, with_plymouth: bool) -> BootScreen {
        BootScreen {
            ask_password: program(dir, "ask", ASK),
            plymouth: with_plymouth.then(|| {
                Plymouth::running(program(dir, "plymouth", PLYMOUTH)).expect("the stub pings")
            }),
            volume: "root".into(),
        }
    }

    fn people() -> Vec<Person> {
        ["Anna", "Ben"]
            .iter()
            .zip(1000..)
            .map(|(name, uid)| Person {
                uid,
                name: name.to_lowercase(),
                display_name: (*name).to_owned(),
                secret: SecretKind::Pin,
            })
            .collect()
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn the_list_moves_with_tab_and_arrows_and_chooses_with_enter() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), true);
        fs::write(dir.path().join("keys"), "tab\ntab\nup\nenter\n").unwrap();
        assert_eq!(screen.pick(&people(), 0), Pick::Person(1));
        assert_eq!(
            read(dir.path(), "messages"),
            "tilo-signin-people:2\n\
             tilo-signin-person:0:Anna\n\
             tilo-signin-person:1:Ben\n\
             tilo-signin-choose:0\n\
             tilo-signin-choose:1\n\
             tilo-signin-choose:2\n\
             tilo-signin-choose:1\n"
        );
    }

    #[test]
    fn the_last_row_is_other_ways_in() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), true);
        fs::write(dir.path().join("keys"), "up\nenter\n").unwrap();
        assert_eq!(screen.pick(&people(), 0), Pick::OtherWays);
    }

    #[test]
    fn without_plymouth_the_list_is_a_typed_question() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), false);
        fs::write(dir.path().join("answers"), "7\n2\n").unwrap();
        assert_eq!(screen.pick(&people(), 0), Pick::Person(1));
        let asked = read(dir.path(), "asked");
        assert!(
            asked.contains("--echo=yes --timeout=0 --id=tilo-signin-person:root Who is signing in? Type 1 for Anna, 2 for Ben, or leave it empty for other ways to sign in"),
            "{asked}"
        );
        fs::write(dir.path().join("answers"), "\n").unwrap();
        assert_eq!(screen.pick(&people(), 0), Pick::OtherWays);
    }

    #[test]
    fn without_plymouth_the_field_is_asked_through_the_password_agents() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), false);
        fs::write(dir.path().join("answers"), "123456\n\n").unwrap();
        let anna = people().remove(0);
        let prompt = Prompt {
            person: &anna,
            note: Note::None,
            several: false,
        };
        assert!(
            matches!(screen.ask(&prompt), Entry::Secret(secret) if secret.as_str() == "123456")
        );
        assert!(matches!(screen.ask(&prompt), Entry::Empty));
        assert_eq!(
            read(dir.path(), "asked").lines().next(),
            Some(
                "-n --echo=masked --timeout=0 --id=tilo-signin:root PIN for Anna, or leave it empty for other ways to sign in"
            )
        );
    }

    #[test]
    fn the_field_is_labelled_for_the_person_and_their_secret() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), true);
        fs::write(dir.path().join("answers"), "123456\n\n<cancel>\n").unwrap();
        let anna = people().remove(0);
        let wrong = Note::Wrong {
            secret: SecretKind::Pin,
            tries_left: Some(4),
        };
        let prompt = Prompt {
            person: &anna,
            note: wrong,
            several: false,
        };
        assert!(
            matches!(screen.ask(&prompt), Entry::Secret(secret) if secret.as_str() == "123456")
        );
        assert!(matches!(screen.ask(&prompt), Entry::Empty));
        assert!(matches!(screen.ask(&prompt), Entry::Unavailable));
        assert_eq!(
            read(dir.path(), "asked").lines().next(),
            Some(
                "ask-for-password --prompt=Incorrect PIN. 4 tries left. PIN for Anna, or leave it empty for other ways to sign in"
            ),
            "asked on Plymouth itself, whose answer can be empty"
        );
        assert_eq!(
            read(dir.path(), "messages")
                .lines()
                .take(4)
                .collect::<Vec<_>>(),
            [
                "tilo-signin-name:Anna",
                "tilo-signin-kind:pin",
                "tilo-signin-back:0",
                "tilo-signin-note:wrong:Incorrect PIN. 4 tries left.",
            ]
        );
    }
}
