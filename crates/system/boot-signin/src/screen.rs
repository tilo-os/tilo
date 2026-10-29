//! The sign-in on the console, when the greeter can't start or goes away
//! (docs/security/sign-in.md §3.4, "If the greeter can't start"). The list
//! and the secret are asked through systemd's password agents, so they work
//! on a serial console too.
//!
//! Plymouth leaves first. While it runs, the console agent does not ask and
//! Plymouth's own agent would: it takes an empty answer as no answer and asks
//! again, so the empty Enter that means "Can't sign in" would never arrive.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use zeroize::Zeroizing;

use crate::flow::{Entry, Pick, Prompt, Screen};
use crate::people::Person;
use crate::plymouth::Plymouth;

pub struct ConsoleScreen {
    pub ask_password: PathBuf,
    /// Plymouth, while it still draws the boot: it ends before the first
    /// question.
    pub plymouth: Option<Plymouth>,
    pub volume: String,
}

impl ConsoleScreen {
    /// Give the console to the password agents.
    fn take_console(&mut self) {
        if let Some(plymouth) = self.plymouth.take() {
            eprintln!("tilo-boot-signin: Plymouth ends; signing in on the console");
            plymouth.quit();
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
}

impl Screen for ConsoleScreen {
    /// The list as one question, since nothing draws a list.
    fn pick(&mut self, people: &[Person], _highlighted: usize) -> Pick {
        self.take_console();
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
            let Some(answer) = self.ask_password("tilo-signin-person", "yes", &question) else {
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

    fn ask(&mut self, prompt: &Prompt<'_>) -> Entry {
        self.take_console();
        let person = prompt.person;
        let empty = if prompt.several {
            "leave it empty to choose someone else"
        } else {
            "leave it empty for other ways to sign in"
        };
        let mut text = format!(
            "{} for {}, or {empty}",
            person.secret.label(),
            person.display_name
        );
        let note = prompt.note.text();
        if !note.is_empty() {
            text = format!("{note} {text}");
        }
        let entry = match self.ask_password("tilo-signin", "masked", &text) {
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

    fn checking(&mut self, _person: &Person) {}

    fn finish(&mut self, _unlocked: bool) {}
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    use super::*;
    use crate::flow::Note;
    use crate::people::SecretKind;

    /// An ask-password agent that answers from a script, one line each,
    /// and logs what it was asked.
    const ASK: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
printf '%s\n' "$*" >>"$dir/asked"
answer="$(head -n1 "$dir/answers")"; sed -i 1d "$dir/answers"
[ "$answer" = "<cancel>" ] && exit 1
printf '%s' "$answer"
"#;

    /// A Plymouth that answers its ping and logs every other command, each
    /// where the ask-password agent logs its questions.
    const PLYMOUTH: &str = r#"#!/bin/bash
[ "$1" = --ping ] && exit 0
printf 'plymouth %s\n' "$*" >>"$(dirname "$0")/asked"
"#;

    fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn screen(dir: &Path, with_plymouth: bool) -> ConsoleScreen {
        ConsoleScreen {
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
    fn the_list_is_a_typed_question() {
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
    fn the_field_is_asked_through_the_password_agents() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), false);
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
                "-n --echo=masked --timeout=0 --id=tilo-signin:root Incorrect PIN. 4 tries left. PIN for Anna, or leave it empty for other ways to sign in"
            )
        );
    }

    #[test]
    fn plymouth_ends_before_the_first_question() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut screen = screen(dir.path(), true);
        fs::write(dir.path().join("answers"), "2\n123456\n").unwrap();
        assert_eq!(screen.pick(&people(), 0), Pick::Person(1));
        let ben = people().remove(1);
        let prompt = Prompt {
            person: &ben,
            note: Note::None,
            several: true,
        };
        assert!(matches!(screen.ask(&prompt), Entry::Secret(_)));
        let asked = read(dir.path(), "asked");
        let asked: Vec<&str> = asked.lines().collect();
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert_eq!(asked.first(), Some(&"plymouth quit"), "{asked:?}");
        assert!(
            asked
                .iter()
                .skip(1)
                .all(|line| !line.starts_with("plymouth")),
            "Plymouth is told to quit once: {asked:?}"
        );
    }
}
