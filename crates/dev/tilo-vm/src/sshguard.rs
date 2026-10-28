//! What `vm ssh` refuses without `--raw`: commands that write into the VM's
//! `/usr`, `/etc`, `/boot` or tilo-vm's state behind tilo-vm's back. Reading
//! anything, anywhere, is always fine.
//!
//! This is a guard against mistakes, not a sandbox: it reads the command line
//! as words, splits it at shell separators (inside quotes too, so
//! `sh -c '...'` is looked into), and asks of each simple command whether it
//! writes one of those places.

/// Places only tilo-vm (swap, vm upgrade, gc) may change.
const GUARDED: &[&str] = &[
    "/usr",
    "/etc",
    "/boot",
    "/var/lib/tilo-vm",
    "/var/lib/extensions",
    "/run/extensions",
];

/// Programs whose last operand is where they write.
const COPIERS: &[&str] = &["cp", "install", "rsync", "scp", "ln"];
/// Programs that change every operand they are given.
const CHANGERS: &[&str] = &[
    "mv", "rm", "rmdir", "mkdir", "touch", "chmod", "chown", "chgrp", "chattr", "truncate", "tee",
    "unlink", "setfacl", "chcon",
];
/// Words that run the rest of the line as a command.
const WRAPPERS: &[&str] = &["sudo", "doas", "env", "nice", "nohup", "time", "exec"];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash"];
/// `systemctl` verbs that change what runs or what is enabled.
const SYSTEMCTL_CHANGES: &[&str] = &[
    "enable",
    "disable",
    "reenable",
    "start",
    "stop",
    "restart",
    "try-restart",
    "reload",
    "reload-or-restart",
    "try-reload-or-restart",
    "kill",
    "mask",
    "unmask",
    "edit",
    "revert",
    "link",
    "preset",
];
/// tilo-vmd subcommands, which all act on the VM's state.
const DAEMON_COMMANDS: &[&str] = &["rpc", "build", "ci-layer", "restore", "input-serve"];

/// What `command` would change behind tilo-vm's back, as the end of the
/// sentence "it ...", or `None` for anything that only reads.
pub fn guarded_write(command: &[String]) -> Option<String> {
    let line = command.join(" ");
    segments(&line)
        .iter()
        .find_map(|words| simple_command(words))
}

fn is_guarded(path: &str) -> bool {
    let path = path.trim_matches(|c| c == '"' || c == '\'');
    GUARDED.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The path part of `host:/path` (scp, rsync) or `key=/path` (dd of=).
fn path_of(word: &str) -> &str {
    match word.split_once(['=', ':']) {
        Some((_, path)) if !word.starts_with('/') => path,
        _ => word,
    }
}

/// Split at `;`, `&&`, `||`, `|`, `&`, newlines, backticks and parentheses;
/// quotes are dropped so the inside of `sh -c '...'` is read as commands.
fn segments(line: &str) -> Vec<Vec<String>> {
    let mut spaced = String::with_capacity(line.len().saturating_mul(2));
    for c in line.chars() {
        match c {
            ';' | '|' | '&' | '\n' | '`' | '(' | ')' => {
                // `2>&1` and `&>` are redirections, not separators.
                if c == '&' && (spaced.ends_with('>') || spaced.ends_with("> ")) {
                    spaced.push(c);
                } else {
                    spaced.push_str(" ; ");
                }
            }
            '"' | '\'' | '$' | '{' | '}' => spaced.push(' '),
            _ => spaced.push(c),
        }
    }
    spaced
        .split(" ; ")
        .map(|segment| segment.split_whitespace().map(str::to_owned).collect())
        .filter(|words: &Vec<String>| !words.is_empty())
        .collect()
}

/// A redirection into a guarded place: `> /etc/x`, `>>/usr/y`, `2>/boot/z`.
fn redirect(words: &[String]) -> Option<String> {
    let following = words.iter().skip(1).map(Some).chain([None]);
    words.iter().zip(following).find_map(|(word, next)| {
        let (_, after) = word.split_once('>')?;
        let target = after.trim_start_matches(['>', '&', '|']);
        let target = if target.is_empty() {
            next.map_or("", String::as_str)
        } else {
            target
        };
        is_guarded(target).then(|| format!("writes {target} (a redirection)"))
    })
}

fn operands(words: &[String]) -> Vec<&str> {
    words
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|w| !w.starts_with('-') && !w.contains('>'))
        .collect()
}

#[expect(clippy::too_many_lines, reason = "one arm per kind of writer")]
fn simple_command(words: &[String]) -> Option<String> {
    if let Some(write) = redirect(words) {
        return Some(write);
    }
    // Skip wrappers (and their options), VAR=value and `sh -c`.
    let mut start = 0;
    while let Some(word) = words.get(start) {
        let name = word.rsplit('/').next().unwrap_or(word);
        let after = start.saturating_add(1);
        if WRAPPERS.contains(&name) || (word.contains('=') && !word.starts_with('-')) {
            start = after;
            while words.get(start).is_some_and(|w| w.starts_with('-')) {
                start = start.saturating_add(1);
            }
        } else if SHELLS.contains(&name) && words.get(after).is_some_and(|w| w == "-c") {
            start = after.saturating_add(1);
        } else {
            break;
        }
    }
    let words = words.get(start..)?;
    let program = words.first()?;
    let name = program.rsplit('/').next().unwrap_or(program).to_owned();
    let args: Vec<&str> = words.iter().skip(1).map(String::as_str).collect();
    let has = |verb: &str| args.contains(&verb);
    let first = args.iter().find(|a| !a.starts_with('-')).copied();
    let name = name.as_str();
    if COPIERS.contains(&name) {
        let target = args
            .windows(2)
            .find_map(|pair| match pair {
                [flag, dir] if *flag == "-t" || *flag == "--target-directory" => Some(*dir),
                _ => None,
            })
            .or_else(|| {
                args.iter()
                    .find_map(|a| a.strip_prefix("--target-directory="))
            })
            .or_else(|| operands(words).last().copied())?;
        return is_guarded(path_of(target)).then(|| format!("writes {}", path_of(target)));
    }
    let in_place = name == "sed"
        && args.iter().any(|a| {
            a.starts_with("--in-place")
                || (a.starts_with('-') && !a.starts_with("--") && a.contains('i'))
        });
    if CHANGERS.contains(&name) || in_place {
        return operands(words)
            .into_iter()
            .find(|w| is_guarded(w))
            .map(|w| format!("changes {w}"));
    }
    match name {
        "dd" => args
            .iter()
            .find_map(|a| a.strip_prefix("of="))
            .filter(|t| is_guarded(t))
            .map(|t| format!("writes {t}")),
        "dnf" | "dnf5" | "yum" | "microdnf" | "apt" | "apt-get" | "zypper" => first
            .filter(|verb| {
                [
                    "install",
                    "reinstall",
                    "remove",
                    "erase",
                    "purge",
                    "upgrade",
                    "update",
                    "downgrade",
                    "swap",
                    "distro-sync",
                    "dist-upgrade",
                    "full-upgrade",
                    "autoremove",
                    "in",
                    "rm",
                    "up",
                    "dup",
                ]
                .contains(verb)
            })
            .map(|verb| format!("changes packages ({name} {verb})")),
        "rpm" => args
            .iter()
            .find(|a| {
                [
                    "--install",
                    "--upgrade",
                    "--freshen",
                    "--erase",
                    "--reinstall",
                ]
                .contains(a)
                    || (a.starts_with('-')
                        && !a.starts_with("--")
                        && a.contains(['i', 'U', 'F', 'e'])
                        && !a.contains('q'))
            })
            .map(|flag| format!("changes packages (rpm {flag})")),
        "rpm-ostree" => first
            .filter(|verb| {
                [
                    "install",
                    "uninstall",
                    "override",
                    "rebase",
                    "upgrade",
                    "deploy",
                    "usroverlay",
                    "apply-live",
                    "reset",
                    "rollback",
                ]
                .contains(verb)
            })
            .map(|verb| format!("changes the image (rpm-ostree {verb})")),
        "pacman" => args
            .iter()
            .find(|a| {
                (a.starts_with("-S") || a.starts_with("-U") || a.starts_with("-R"))
                    && !a.contains(['s', 'i', 'l', 'g', 'p'])
            })
            .map(|flag| format!("changes packages (pacman {flag})")),
        "systemctl" => {
            let verb = first.filter(|v| SYSTEMCTL_CHANGES.contains(v))?;
            let unit = args
                .iter()
                .find(|a| !a.starts_with('-') && *a != &verb && a.contains("tilo"))?;
            Some(format!("{verb}s the tilo unit {unit}"))
        }
        "systemd-sysext" => first
            .filter(|v| ["merge", "refresh", "unmerge"].contains(v))
            .map(|v| format!("changes the merged system extensions (systemd-sysext {v})")),
        "bootc" => first
            .filter(|v| {
                [
                    "switch",
                    "upgrade",
                    "rollback",
                    "usr-overlay",
                    "install",
                    "edit",
                ]
                .contains(v)
            })
            .map(|v| format!("changes the VM's image (bootc {v})")),
        "ostree" => (has("admin") && (has("unlock") || has("deploy") || has("undeploy")))
            .then(|| "changes the VM's deployments (ostree admin)".to_owned()),
        "mount" | "umount" => operands(words)
            .into_iter()
            .find(|w| is_guarded(w))
            .map(|w| format!("remounts {w}")),
        "tilo-vmd" => first
            .filter(|v| DAEMON_COMMANDS.contains(v))
            .map(|v| format!("runs tilo-vmd {v}, which acts on the VM's slots behind tilo-vm")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(line: &str) -> Option<String> {
        guarded_write(&[line.to_owned()])
    }

    /// Reads pass anywhere and writes into guarded places are caught.
    #[test]
    fn the_ssh_guard_passes_reads_and_catches_guarded_writes() {
        // Phase: reads_pass_anywhere.
        {
            for line in [
                "systemctl status tilo-vmd --no-pager",
                "systemctl is-active tilo-vmd",
                "systemctl is-enabled tilo-vm-session@a.service",
                "command -v tilo-vmd",
                "tilo-vmd --version",
                "tilo-vmd --help",
                "ls /usr/bin",
                "cat /etc/os-release",
                "ls -la /var/lib/tilo-vm/slots",
                "journalctl -b -p warning",
                "journalctl -u tilo-vmd --no-pager | tail -n 50",
                "systemctl status tilo-vmd --no-pager | head -20; systemctl is-active sshd && echo ok",
                "rpm -qa | grep tilo",
                "rpm -qi bootc",
                "dnf list installed",
                "bootc status --json",
                "systemd-sysext status",
                "cp /usr/share/tilo/components.toml /tmp/components.toml",
                "find /usr/lib/systemd -name 'tilo*' 2>/dev/null",
                "cat /etc/os-release > /tmp/os",
                "ls /usr 2>&1 | head",
                "sed -n 1,5p /etc/fstab",
                "systemctl restart sshd",
                "pacman -Ss foo",
            ] {
                assert_eq!(check(line), None, "{line}");
            }
            assert_eq!(guarded_write(&[]), None);
            let words = ["systemctl", "status", "tilo-vmd", "--no-pager"].map(str::to_owned);
            assert_eq!(guarded_write(&words), None);
        }

        // Phase: writes_into_guarded_places_are_caught.
        {
            for (line, says) in [
                ("sudo cp shell /usr/bin/shell", "writes /usr/bin/shell"),
                (
                    "install -Dm755 target/shell /usr/bin/shell",
                    "writes /usr/bin/shell",
                ),
                ("cp -t /usr/lib/tilo a b", "writes /usr/lib/tilo"),
                (
                    "rsync -a out/ root@vm:/usr/lib/tilo/",
                    "writes /usr/lib/tilo/",
                ),
                ("mv /usr/bin/shell /tmp/shell", "changes /usr/bin/shell"),
                ("echo x | tee /etc/tilo.conf", "changes /etc/tilo.conf"),
                ("echo x > /etc/tilo.conf", "writes /etc/tilo.conf"),
                ("echo x >>/etc/tilo.conf", "writes /etc/tilo.conf"),
                ("dd if=img of=/boot/efi.img", "writes /boot/efi.img"),
                (
                    "rm -rf /var/lib/tilo-vm/layers/x",
                    "changes /var/lib/tilo-vm/layers/x",
                ),
                (
                    "sed -i s/a/b/ /etc/greetd/config.toml",
                    "changes /etc/greetd/config.toml",
                ),
                ("dnf install -y strace", "changes packages (dnf install)"),
                ("sudo rpm-ostree install strace", "changes the image"),
                ("rpm -Uvh x.rpm", "changes packages (rpm -Uvh)"),
                (
                    "systemctl restart tilo-vmd",
                    "restarts the tilo unit tilo-vmd",
                ),
                (
                    "systemctl enable --now tilo-vmd.socket",
                    "enables the tilo unit",
                ),
                ("systemd-sysext refresh", "system extensions"),
                ("bootc usr-overlay", "bootc usr-overlay"),
                ("ostree admin unlock --hotfix", "ostree admin"),
                ("mount -o remount,rw /usr", "remounts /usr"),
                ("tilo-vmd restore", "tilo-vmd restore"),
                ("sh -c 'cp shell /usr/bin/shell'", "writes /usr/bin/shell"),
                (
                    "ls /usr/bin && cp shell /usr/bin/shell",
                    "writes /usr/bin/shell",
                ),
            ] {
                let found = check(line).unwrap_or_else(|| panic!("{line} passed"));
                assert!(found.contains(says), "{line}: {found}");
            }
        }
    }
}
