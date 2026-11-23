//! Owner-key provisioning and its refusals on an offline deployment.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_install_helpers::dev_ssh::{Host, apply};

/// Records labels instead of calling `matchpathcon`/`chcon`; the keys are still
/// checked by the real `ssh-keygen`.
#[derive(Default)]
struct Labels(RefCell<Vec<String>>);

impl Host for Labels {
    fn label(&self, _path: &Path, installed: &str) -> tilo_install_helpers::Result<()> {
        self.0.borrow_mut().push(installed.to_owned());
        Ok(())
    }
}

fn keygen(path: &Path) {
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .status_locked()
        .expect("ssh-keygen must be installed to check the keys as the installer does");
    assert!(status.success());
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().take(2).map(str::to_owned).collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through one command reads as one flow"
)]
fn dev_ssh_owner_key_provisioning() {
    {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let etc = root.join("etc");
        fs::create_dir(&etc).unwrap();
        keygen(&root.join("key"));
        let labels = Labels::default();
        apply(&labels, &etc, "bob", &root.join("key.pub")).unwrap();
        assert_eq!(
            words(&fs::read_to_string(etc.join("ssh/authorized_keys/bob")).unwrap()),
            words(&fs::read_to_string(root.join("key.pub")).unwrap())
        );
        let config =
            fs::read_to_string(etc.join("ssh/sshd_config.d/00-tilo-bringup.conf")).unwrap();
        for directive in [
            "AllowUsers bob",
            "PasswordAuthentication no",
            "PermitRootLogin no",
            "AuthorizedKeysFile /etc/ssh/authorized_keys/%u",
        ] {
            assert!(config.contains(directive), "{directive}");
        }
        assert_eq!(
            config,
            "AllowUsers bob\nPermitRootLogin no\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
         AuthorizedKeysFile /etc/ssh/authorized_keys/%u\n"
        );
        let enable = etc.join("systemd/system/multi-user.target.wants/sshd.service");
        assert_eq!(
            fs::read_link(&enable).unwrap(),
            Path::new("/usr/lib/systemd/system/sshd.service")
        );
        assert_eq!(
            *labels.0.borrow(),
            [
                "/etc/ssh/authorized_keys",
                "/etc/ssh/sshd_config.d",
                "/etc/systemd/system/multi-user.target.wants",
                "/etc/ssh/authorized_keys/bob",
                "/etc/ssh/sshd_config.d/00-tilo-bringup.conf",
                "/etc/systemd/system/multi-user.target.wants/sshd.service",
            ]
        );
        for written in [
            "ssh/authorized_keys/bob",
            "ssh/sshd_config.d/00-tilo-bringup.conf",
        ] {
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &fs::metadata(etc.join(written)).unwrap().permissions(),
            );
            assert_eq!(mode & 0o7777, 0o644, "{written}");
        }
        assert!(!etc.join("tilo/.firstboot-done").exists());
        assert!(!root.join("var/lib/tilo/device-private").exists());
        let again = apply(&Labels::default(), &etc, "bob", &root.join("key.pub")).unwrap_err();
        assert!(again.0.contains("already exists"), "{again}");
    }

    {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let etc = root.join("etc");
        fs::create_dir(&etc).unwrap();
        for name in ["a", "b"] {
            keygen(&root.join(name));
        }
        let request = root.join("keys.pub");
        fs::write(
            &request,
            fs::read_to_string(root.join("a.pub")).unwrap()
                + "\n"
                + &fs::read_to_string(root.join("b.pub")).unwrap(),
        )
        .unwrap();
        apply(&Labels::default(), &etc, "bob", &request).unwrap();
        let installed = fs::read_to_string(etc.join("ssh/authorized_keys/bob")).unwrap();
        let installed: Vec<Vec<String>> = installed.lines().map(words).collect();
        let expected: Vec<Vec<String>> = ["a", "b"]
            .iter()
            .map(|name| words(&fs::read_to_string(root.join(format!("{name}.pub"))).unwrap()))
            .collect();
        assert_eq!(installed, expected);
    }

    {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let etc = root.join("etc");
        fs::create_dir(&etc).unwrap();
        let key = root.join("key");
        fs::write(&key, "command=\"id\" ssh-ed25519 AAAA\n").unwrap();
        assert!(apply(&Labels::default(), &etc, "bob", &key).is_err());
        keygen(&root.join("real"));
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, etc.join("ssh")).unwrap();
        let error = apply(&Labels::default(), &etc, "bob", &root.join("real.pub")).unwrap_err();
        assert!(error.0.contains("symlink"), "{error}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let etc = root.join("etc");
        fs::create_dir(&etc).unwrap();
        keygen(&root.join("real"));
        let real = fs::read_to_string(root.join("real.pub")).unwrap();
        let real_words = words(&real);
        let refuse = |name: &str, content: &str| {
            let path = root.join(name);
            fs::write(&path, content).unwrap();
            apply(&Labels::default(), &etc, "bob", &path).unwrap_err().0
        };
        assert_eq!(
            refuse("many", &real.repeat(17)),
            "expected 1 to 16 SSH public keys"
        );
        assert_eq!(
            refuse("bare", "ssh-ed25519\n"),
            "expected SSH public keys without authorized_keys options"
        );
        assert_eq!(
            refuse("dss", "ssh-dss AAAA\n"),
            "unsupported SSH public key type"
        );
        let mislabelled = format!("ssh-rsa {}\n", real_words[1]);
        assert_eq!(
            refuse("mislabelled", &mislabelled),
            "SSH public key type differs from its wire encoding"
        );
        // A carriage return ends a line, as Python's universal newlines read it.
        let crlf = format!("{} {}\r\n", real_words[0], real_words[1]);
        let path = root.join("crlf");
        fs::write(&path, crlf).unwrap();
        apply(&Labels::default(), &etc, "bob", &path).unwrap();

        let invalid = apply(&Labels::default(), &etc, "Bob", &path).unwrap_err().0;
        assert_eq!(invalid, "invalid installed /etc or account");

        let linked = root.join("linked.pub");
        fs::hard_link(root.join("real.pub"), &linked).unwrap();
        assert_eq!(
            apply(&Labels::default(), &root.join("etc2-missing"), "x", &linked)
                .unwrap_err()
                .0,
            "invalid installed /etc or account"
        );
        let fresh = root.join("etc3");
        fs::create_dir(&fresh).unwrap();
        assert_eq!(
            apply(&Labels::default(), &fresh, "bob", &linked)
                .unwrap_err()
                .0,
            "SSH key must be a bounded, non-linked regular file"
        );
    }

    {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let etc = root.join("etc");
        fs::create_dir_all(etc.join("systemd/system/multi-user.target.wants")).unwrap();
        std::os::unix::fs::symlink(
            "/dev/null",
            etc.join("systemd/system/multi-user.target.wants/sshd.service"),
        )
        .unwrap();
        keygen(&root.join("key"));
        assert_eq!(
            apply(&Labels::default(), &etc, "bob", &root.join("key.pub"))
                .unwrap_err()
                .0,
            "SSH enablement must first be cleared by the installer"
        );
        assert!(!etc.join("ssh/authorized_keys/bob").exists());
    }
}
