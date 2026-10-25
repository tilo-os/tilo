//! The `tilo-auth` command line, as the initramfs runs it: no daemon, one
//! process per question.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::flow::{Auth, Limit, Unlock};
use crate::people::{self, Person};

pub struct CliAuth {
    pub program: PathBuf,
    pub device: PathBuf,
    pub volume: String,
    /// A private directory on the initramfs tmpfs for the one secret file.
    pub runtime: PathBuf,
}

impl Auth for CliAuth {
    fn people(&mut self) -> Vec<Person> {
        let output = Command::new(&self.program)
            .arg("boot-people")
            .arg("--device")
            .arg(&self.device)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                people::parse(&String::from_utf8_lossy(&output.stdout))
            }
            _ => Vec::new(),
        }
    }

    fn unlock(&mut self, person: &Person, secret: &str) -> Unlock {
        let Ok(file) = SecretFile::write(&self.runtime, secret) else {
            eprintln!("tilo-boot-signin: could not hold the entry for checking");
            return Unlock::CouldNotTry;
        };
        let output = Command::new(&self.program)
            .arg("boot-unlock")
            .arg("--device")
            .arg(&self.device)
            .arg("--volume")
            .arg(&self.volume)
            .arg("--secret-file")
            .arg(file.path())
            .arg("--uid")
            .arg(person.uid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .output();
        drop(file);
        let Ok(output) = output else {
            return Unlock::CouldNotTry;
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr.lines().filter(|line| limit(line).is_none()) {
            eprintln!("{line}");
        }
        match output.status.code() {
            Some(0) => Unlock::Opened,
            Some(1) => Unlock::Wrong(stderr.lines().rev().find_map(limit).unwrap_or_default()),
            _ => Unlock::CouldNotTry,
        }
    }
}

#[derive(Deserialize)]
struct Reported {
    /// `wrong`: the entry was checked and refused. `locked-out`: the TPM
    /// refused to check it.
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tries_left: Option<u32>,
    /// The TPM's time to win back one try. It is a wait only when no try is
    /// left.
    #[serde(default)]
    retry_in_s: Option<u64>,
}

/// The one JSON line `boot-unlock` prints after a refused secret.
fn limit(line: &str) -> Option<Limit> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let reported: Reported = serde_json::from_str(line).ok()?;
    let locked_out =
        reported.error.as_deref() == Some("locked-out") || reported.tries_left == Some(0);
    Some(Limit {
        tries_left: reported.tries_left,
        wait_seconds: locked_out.then_some(reported.retry_in_s).flatten(),
    })
}

/// The entry, readable by root alone for as long as one check takes, and
/// overwritten before it is removed.
struct SecretFile {
    path: PathBuf,
    len: usize,
}

impl SecretFile {
    fn write(runtime: &Path, secret: &str) -> std::io::Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(runtime)?;
        let path = runtime.join("entry");
        let _ = fs::remove_file(&path);
        let file = Self {
            path,
            len: secret.len(),
        };
        let mut handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file.path)?;
        handle.write_all(secret.as_bytes())?;
        Ok(file)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        if let Ok(mut handle) = OpenOptions::new().write(true).open(&self.path) {
            let _ = handle.write_all(&vec![0; self.len]);
            let _ = handle.sync_all();
        }
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::people::SecretKind;

    /// A `tilo-auth` that follows the contract: Anna's PIN is 123456, and a
    /// wrong one reports the tries left.
    const STUB: &str = r#"#!/bin/bash
set -u
log="$(dirname "$0")/calls"
case "$1" in
  boot-people)
    printf '%s\n' "$*" >>"$log"
    printf '[{"uid":1000,"name":"anna","display_name":"Anna Example","factors":["tpm","recovery"],"secret":"pin"}]\n'
    ;;
  boot-unlock)
    while [ $# -gt 0 ]; do
      case "$1" in --secret-file) file="$2"; shift ;; --volume) volume="$2"; shift ;; esac
      shift
    done
    printf 'boot-unlock %s %s %s\n' "$volume" "$(stat -c %a "$file")" "$(cat "$file")" >>"$log"
    case "$(cat "$file")" in
      123456) exit 0 ;;
      no-tpm) echo 'no TPM here' >&2; exit 2 ;;
      *) echo 'checking'; echo 'the TPM refused the entry' >&2; echo '{"error":"wrong","tries_left":7,"retry_in_s":7200}' >&2; exit 1 ;;
    esac
    ;;
esac
"#;

    fn stub(dir: &Path) -> CliAuth {
        let program = dir.join("tilo-auth");
        fs::write(&program, STUB).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        CliAuth {
            program,
            device: PathBuf::from("/dev/vda3"),
            volume: "root".into(),
            runtime: dir.join("run"),
        }
    }

    fn anna() -> Person {
        Person {
            uid: 1000,
            name: "anna".into(),
            display_name: "Anna Example".into(),
            secret: SecretKind::Pin,
        }
    }

    #[test]
    fn people_come_from_boot_people() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut auth = stub(dir.path());
        assert_eq!(auth.people(), vec![anna()]);
        assert_eq!(
            fs::read_to_string(dir.path().join("calls")).unwrap(),
            "boot-people --device /dev/vda3\n"
        );
    }

    #[test]
    fn a_missing_tilo_auth_lists_nobody() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut auth = CliAuth {
            program: dir.path().join("absent"),
            device: PathBuf::from("/dev/vda3"),
            volume: "root".into(),
            runtime: dir.path().join("run"),
        };
        assert!(auth.people().is_empty());
        assert!(matches!(
            auth.unlock(&anna(), "123456"),
            Unlock::CouldNotTry
        ));
    }

    #[test]
    fn the_entry_reaches_boot_unlock_privately_and_is_gone_after() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut auth = stub(dir.path());
        assert!(matches!(auth.unlock(&anna(), "123456"), Unlock::Opened));
        assert_eq!(
            fs::read_to_string(dir.path().join("calls")).unwrap(),
            "boot-unlock root 600 123456\n"
        );
        assert!(!dir.path().join("run/entry").exists());
        assert_eq!(
            fs::metadata(dir.path().join("run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn exit_statuses_map_to_outcomes() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut auth = stub(dir.path());
        match auth.unlock(&anna(), "654321") {
            Unlock::Wrong(limit) => assert_eq!(
                limit,
                Limit {
                    tries_left: Some(7),
                    wait_seconds: None
                }
            ),
            _ => panic!("a wrong PIN must read as wrong"),
        }
        assert!(matches!(
            auth.unlock(&anna(), "no-tpm"),
            Unlock::CouldNotTry
        ));
        assert!(!dir.path().join("run/entry").exists());
    }

    #[test]
    fn limits_parse_from_the_reported_line() {
        assert_eq!(
            limit(r#"{"error":"locked-out","tries_left":0,"retry_in_s":120}"#),
            Some(Limit {
                tries_left: Some(0),
                wait_seconds: Some(120)
            })
        );
        // The time to win back a try is no wait while tries are left.
        assert_eq!(
            limit(r#"{"error":"wrong","tries_left":9,"retry_in_s":7200}"#),
            Some(Limit {
                tries_left: Some(9),
                wait_seconds: None
            })
        );
        assert_eq!(limit("{}"), Some(Limit::default()));
        assert_eq!(limit("the TPM is in lockout"), None);
        assert_eq!(limit("{not json"), None);
    }

    /// Types the scripted entries, then leaves the field empty.
    struct Typist {
        entries: Vec<&'static str>,
        notes: Vec<String>,
    }

    impl crate::flow::Screen for Typist {
        fn pick(&mut self, _people: &[Person], _highlighted: usize) -> crate::flow::Pick {
            crate::flow::Pick::Person(0)
        }
        fn ask(&mut self, prompt: &crate::flow::Prompt<'_>) -> crate::flow::Entry {
            self.notes.push(prompt.note.text());
            if self.entries.is_empty() {
                return crate::flow::Entry::Empty;
            }
            let entry = self.entries.remove(0);
            crate::flow::Entry::Secret(zeroize::Zeroizing::new(entry.to_owned()))
        }
        fn checking(&mut self, _person: &Person) {}
        fn finish(&mut self, _unlocked: bool) {}
    }

    #[test]
    fn a_repeated_wrong_entry_does_not_reach_tilo_auth_again() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let mut auth = stub(dir.path());
        let mut screen = Typist {
            entries: vec!["654321", "654321", "123456"],
            notes: Vec::new(),
        };
        assert_eq!(
            crate::flow::run(&mut auth, &mut screen),
            crate::flow::Exit::Unlocked
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("calls")).unwrap(),
            "boot-people --device /dev/vda3\n\
             boot-unlock root 600 654321\n\
             boot-unlock root 600 123456\n",
            "the second 654321 cost no TPM try"
        );
        assert_eq!(
            screen.notes,
            [
                "",
                "Incorrect PIN. 7 tries left.",
                "You already tried that PIN. Try another.",
            ]
        );
    }
}
