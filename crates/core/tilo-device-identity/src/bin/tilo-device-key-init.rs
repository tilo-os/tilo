//! Early-boot provisioning of a device identity's key: the machine's
//! (root-only, no arguments), or with `--root DIR --public-env-out FILE` a
//! node session's own tree, created by the session's account before the
//! node's install certifies it. The public half goes to `FILE` as
//! `TILO_NODE_SESSION_DEVICE=<hex>`, a systemd `EnvironmentFile` the node's
//! server names the session with (`--session-device`).
//!
//! The machine's key is the device daemon's, which runs as the `tilo` account
//! on every computer, never as root. This program makes it when the computer
//! has none and gives it and its directory that account, `0600` and `0700`,
//! every boot, so an install from any earlier state settles on the one state
//! the daemon reads.
//! Nothing here keeps a copy, and a device whose daemon has the key already is
//! refused rather than read beside it.
//!
//! `--branch` runs at every boot before anything uses the machine's key: it
//! records which machine the key belongs to and re-keys a machine whose disk
//! was copied from another (`tilo_device_identity::branch`), then gives a new
//! key the daemon's account as well.

use tilo_device_identity::persist::{
    DeviceKeyOutcome, IdentityPaths, device_daemon_owner, enforce_device_key_owner_and_mode,
    holds_device_key, load_or_create_device_key, settle_device_certificate, take_device_key,
};

fn main() {
    if let Err(error) = provision(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("tilo-device-key-init: {error}");
        std::process::exit(1);
    }
}

fn provision(args: &[String]) -> Result<(), String> {
    match args {
        [] => {
            if rustix::process::geteuid().as_raw() != 0 {
                return Err("must run as root".into());
            }
            let owner = device_daemon_owner().map_err(|error| error.to_string())?;
            maintain(&IdentityPaths::default_system(), owner)
        }
        #[cfg(unix)]
        [flag] if flag == "--branch" => {
            if rustix::process::geteuid().as_raw() != 0 {
                return Err("must run as root".into());
            }
            let owner = device_daemon_owner().map_err(|error| error.to_string())?;
            let paths = IdentityPaths::default_system();
            let branched = branch::run(&paths);
            // A re-key writes the new key as root, and a run that stops early can have written it
            // already: whatever is on disk now goes to the daemon.
            if holds_device_key(&paths).map_err(|error| error.to_string())? {
                enforce_device_key_owner_and_mode(&paths.device_privkey, owner)
                    .map_err(|error| error.to_string())?;
            }
            branched
        }
        [root_flag, root, out_flag, out]
            if root_flag == "--root" && out_flag == "--public-env-out" =>
        {
            let me = (
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw(),
            );
            let device = create(&IdentityPaths::under(root), "node session", me)?;
            let line = format!(
                "TILO_NODE_SESSION_DEVICE={}\n",
                hex::encode(device.to_bytes())
            );
            std::fs::write(out, line).map_err(|error| format!("write {out}: {error}"))
        }
        _ => {
            Err("usage: tilo-device-key-init [--branch | --root DIR --public-env-out FILE]".into())
        }
    }
}

/// The machine's key in the one state its daemon reads: `owner`'s, `0600`, in `owner`'s `0700`
/// directory. Every boot, since a re-key, an installer's copy or a build from before this rule can
/// have left another owner or mode; when the computer has no key yet it is made first.
fn maintain(paths: &IdentityPaths, owner: (u32, u32)) -> Result<(), String> {
    if holds_device_key(paths).map_err(|error| error.to_string())? {
        enforce_device_key_owner_and_mode(&paths.device_privkey, owner)
            .map_err(|error| error.to_string())?;
    } else {
        create(paths, "machine", owner)?;
    }
    // A machine imaged before one certificate per person kept the owner's at the old
    // `device/cert.bin`: keep it beside its person and remove the second copy. Idempotent, so
    // every boot settles on the one layout every reader expects.
    settle_device_certificate(paths).map_err(|error| error.to_string())?;
    Ok(())
}

/// Make the device key under `paths` when there is none, before the daemon that writes as the
/// device starts: it is handed over in the file, which the daemon takes. Nothing here keeps it,
/// and a device whose daemon took the key already is refused rather than read beside it.
fn create(
    paths: &IdentityPaths,
    what: &str,
    owner: (u32, u32),
) -> Result<ed25519_dalek::VerifyingKey, String> {
    let (_, outcome) = load_or_create_device_key(paths).map_err(|error| error.to_string())?;
    // Taken and let go at once, while it is still root's: a running daemon refuses it, naming
    // itself.
    let taken = take_device_key(paths).map_err(|error| error.to_string())?;
    let public = taken.key().verifying_key();
    drop(taken);
    enforce_device_key_owner_and_mode(&paths.device_privkey, owner)
        .map_err(|error| error.to_string())?;
    if outcome == DeviceKeyOutcome::Created {
        eprintln!("created {what} device identity");
    }
    Ok(public)
}

#[cfg(unix)]
mod branch {
    use std::io::{self, Write as _};
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use getrandom::rand_core::Rng as _;
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use tilo_device_identity::branch::{
        self, Branch, Certification, Machine, MachineIdentity, PRODUCT_UUID, Resealed, SealedStore,
        Settled, SigningKey,
    };
    use tilo_device_identity::persist::IdentityPaths;

    /// Where the server keeps its state, its workspaces' group state among it.
    const SERVER_STATE: &str = "/var/lib/tilo-server";
    /// Each person's Secret Service, which re-seals their vault as them.
    const SECRET_SERVICE: &str = "/usr/libexec/tilo/tilo-secret-service";
    const PASSWD: &str = "/etc/passwd";
    const MACHINE_ID: &str = "/etc/machine-id";
    const SSH_DIR: &str = "/etc/ssh";
    /// The login accounts' uid range (`login.defs`' `UID_MIN`..`UID_MAX`).
    const PEOPLE: std::ops::Range<u32> = 1000..60000;

    pub(super) fn run(paths: &IdentityPaths) -> Result<(), String> {
        let machine = Machine::read(Path::new(PRODUCT_UUID))
            .map_err(|error| format!("read {PRODUCT_UUID}: {error}"))?;
        let people = people(Path::new(PASSWD));
        let vaults: Vec<PersonVault<'_>> =
            people.iter().map(|person| PersonVault { person }).collect();
        let server = ServerGroups {
            root: PathBuf::from(SERVER_STATE),
        };
        let names = MachineNames {
            machine_id: PathBuf::from(MACHINE_ID),
            ssh: PathBuf::from(SSH_DIR),
        };
        let settled = branch::settle(
            paths,
            machine.as_ref(),
            &Branch {
                stores: vaults
                    .iter()
                    .map(|vault| vault as &dyn SealedStore)
                    .chain([&server as &dyn SealedStore])
                    .collect(),
                machine: Some(&names),
                now_ns: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| {
                        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
                    }),
                after: &|_| Ok(()),
            },
        )
        .map_err(|error| error.to_string())?;
        match settled {
            Settled::NoDevice | Settled::Same => Ok(()),
            Settled::NoMachine => {
                eprintln!("this machine reports no identity of its own: a copy is not re-keyed");
                Ok(())
            }
            Settled::Recorded => {
                eprintln!("recorded which machine the device identity belongs to");
                Ok(())
            }
            Settled::Branched(branched) => {
                eprintln!(
                    "this disk was copied from another machine: device {} is now {} ({})",
                    hex::encode(branched.previous),
                    hex::encode(branched.device),
                    branched.label
                );
                for (store, outcome) in &branched.stores {
                    eprintln!("  {store}: {}", outcome.as_str());
                }
                match &branched.certification {
                    Certification::Certified(_) => {
                        eprintln!("  certified by the person's root on this machine");
                    }
                    Certification::Pending(why) => eprintln!(
                        "  not certified yet ({why}): it is at the person's next sign-in when \
                         their identity is on this machine, otherwise `tilo identity pair` here \
                         and one approval on a device that holds it"
                    ),
                }
                if branched.finished() {
                    return Ok(());
                }
                let failed: Vec<String> = branched
                    .failed
                    .iter()
                    .map(|(store, why)| format!("{store}: {why}"))
                    .collect();
                Err(format!(
                    "kept the previous key to re-seal at the next boot: {}",
                    failed.join("; ")
                ))
            }
        }
    }

    struct Person {
        name: String,
        uid: u32,
        gid: u32,
        home: PathBuf,
    }

    /// The machine's login accounts, from the account database's file.
    fn people(passwd: &Path) -> Vec<Person> {
        let Ok(text) = std::fs::read_to_string(passwd) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split(':').collect();
                let [name, _, uid, gid, _, home, ..] = fields.as_slice() else {
                    return None;
                };
                let uid: u32 = uid.parse().ok()?;
                let gid: u32 = gid.parse().ok()?;
                (PEOPLE.contains(&uid) && home.starts_with('/')).then(|| Person {
                    name: (*name).to_owned(),
                    uid,
                    gid,
                    home: PathBuf::from(home),
                })
            })
            .collect()
    }

    /// A person's Secret Service vault, re-sealed by their own Secret
    /// Service running as them: root never opens a file in their home.
    struct PersonVault<'a> {
        person: &'a Person,
    }

    impl SealedStore for PersonVault<'_> {
        fn name(&self) -> String {
            format!("{}'s Secret Service vault", self.person.name)
        }

        fn reseal(&self, from: &[SigningKey], to: &SigningKey) -> Result<Resealed, String> {
            let spawned = Command::new(SECRET_SERVICE)
                .arg("reseal-device")
                .uid(self.person.uid)
                .gid(self.person.gid)
                .env_clear()
                .env("HOME", &self.person.home)
                .env("PATH", "/usr/bin:/bin")
                .current_dir("/")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                // No Secret Service on this machine: no vault it sealed.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(Resealed::Absent);
                }
                Err(error) => return Err(format!("start {SECRET_SERVICE}: {error}")),
            };
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(&branch::hand_over(from, to))
                    .map_err(|error| format!("hand the keys over: {error}"))?;
            }
            let output = child
                .wait_with_output()
                .map_err(|error| format!("wait for {SECRET_SERVICE}: {error}"))?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            }
            Resealed::parse(&String::from_utf8_lossy(&output.stdout))
                .ok_or_else(|| "the Secret Service answered something else".into())
        }
    }

    /// Each workspace's group state: sealed under a key the device key
    /// derives, and every group in it names the source's device as its
    /// member, so no re-seal makes it this device's. Its member files are
    /// dropped, and the server joins its workspaces' groups as this device,
    /// admitted by their key holders.
    struct ServerGroups {
        root: PathBuf,
    }

    impl ServerGroups {
        /// The directory the server keeps each workspace's group state in.
        fn groups(&self) -> PathBuf {
            self.root.join("groups")
        }
    }

    impl SealedStore for ServerGroups {
        fn name(&self) -> String {
            format!("the server's group state in {}", self.root.display())
        }

        fn reseal(&self, _: &[SigningKey], _: &SigningKey) -> Result<Resealed, String> {
            let mut dropped = false;
            let Ok(entries) = std::fs::read_dir(self.groups()) else {
                return Ok(Resealed::Absent);
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "peer") {
                    std::fs::remove_file(&path)
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                    dropped = true;
                }
            }
            Ok(if dropped {
                Resealed::Reset
            } else {
                Resealed::Absent
            })
        }
    }

    /// The machine-wide names a copied disk shares with its source.
    struct MachineNames {
        machine_id: PathBuf,
        ssh: PathBuf,
    }

    impl MachineIdentity for MachineNames {
        fn renew(&self) -> io::Result<()> {
            // A fresh machine id, written the way systemd writes one: the
            // DHCP client id, the journal and D-Bus take it from here.
            if self.machine_id.exists() {
                let mut id = [0_u8; 16];
                UnwrapErr(SysRng).fill_bytes(&mut id);
                id[6] = (id[6] & 0x0f) | 0x40;
                id[8] = (id[8] & 0x3f) | 0x80;
                let text = format!("{}\n", hex::encode(id));
                tilo_files::native::atomic::write_with_mode(
                    &self.machine_id,
                    text.as_bytes(),
                    0o444,
                )?;
            }
            // SSH host keys: sshd-keygen makes new ones before sshd starts.
            let entries = match std::fs::read_dir(&self.ssh) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            for entry in entries {
                let path = entry?.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let host_key = name.starts_with("ssh_host_")
                    && (name.ends_with("_key") || name.ends_with("_key.pub"));
                if host_key {
                    std::fs::remove_file(&path)?;
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        tilo_test_env::scenario! {
            fn a_branch_finds_its_people_resets_the_server_and_renames_the_machine;

            phase people_are_the_login_accounts;
            {
                let dir = tempfile::tempdir().unwrap();
                let passwd = dir.path().join("passwd");
                std::fs::write(
                    &passwd,
                    "root:x:0:0:root:/root:/bin/bash\n\
                 tilo-server:x:985:985::/var/lib/tilo-server:/sbin/nologin\n\
                 you:x:1000:1000:You:/var/home/you:/bin/bash\n\
                 broken:x:abc:1000::/home/broken:/bin/sh\n\
                 nobody:x:65534:65534:Kernel Overflow User:/:/sbin/nologin\n",
                )
                .unwrap();
                let people = people(&passwd);
                assert_eq!(people.len(), 1);
                assert_eq!(people[0].name, "you");
                assert_eq!((people[0].uid, people[0].gid), (1000, 1000));
                assert_eq!(people[0].home, Path::new("/var/home/you"));
                assert!(super::people(&dir.path().join("missing")).is_empty());
            }

            phase the_servers_group_members_are_dropped_and_its_other_files_kept;
            {
                let dir = tempfile::tempdir().unwrap();
                let server = ServerGroups {
                    root: dir.path().to_path_buf(),
                };
                let key = SigningKey::from_bytes(&[1; 32]);
                assert_eq!(server.reseal(&[], &key).unwrap(), Resealed::Absent);
                let groups = dir.path().join("groups");
                std::fs::create_dir(&groups).unwrap();
                assert_eq!(server.reseal(&[], &key).unwrap(), Resealed::Absent);
                let member = groups.join("ws-1.peer");
                let kept = groups.join("notes");
                std::fs::write(&member, "member").unwrap();
                std::fs::write(&kept, "notes").unwrap();
                assert_eq!(server.reseal(&[], &key).unwrap(), Resealed::Reset);
                assert!(!member.exists());
                assert!(kept.exists());
                assert_eq!(server.reseal(&[], &key).unwrap(), Resealed::Absent);
            }

            phase a_branch_gets_a_machine_id_and_ssh_host_keys_of_its_own;
            {
                let dir = tempfile::tempdir().unwrap();
                let machine_id = dir.path().join("machine-id");
                let ssh = dir.path().join("ssh");
                std::fs::create_dir(&ssh).unwrap();
                std::fs::write(&machine_id, "0123456789abcdef0123456789abcdef\n").unwrap();
                for name in [
                    "ssh_host_ed25519_key",
                    "ssh_host_ed25519_key.pub",
                    "ssh_host_rsa_key",
                    "sshd_config",
                    "moduli",
                ] {
                    std::fs::write(ssh.join(name), "x").unwrap();
                }
                MachineNames {
                    machine_id: machine_id.clone(),
                    ssh: ssh.clone(),
                }
                .renew()
                .unwrap();
                let id = std::fs::read_to_string(&machine_id).unwrap();
                assert_ne!(id, "0123456789abcdef0123456789abcdef\n");
                assert_eq!(id.len(), 33);
                assert!(id.trim_end().chars().all(|c| c.is_ascii_hexdigit()));
                let mut left: Vec<String> = std::fs::read_dir(&ssh)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect();
                left.sort();
                assert_eq!(left, vec!["moduli", "sshd_config"]);

                // A machine without either is left without either.
                let bare = tempfile::tempdir().unwrap();
                MachineNames {
                    machine_id: bare.path().join("machine-id"),
                    ssh: bare.path().join("ssh"),
                }
                .renew()
                .unwrap();
                assert!(!bare.path().join("machine-id").exists());
            }
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn canonical_paths_match_consumers_and_boot_unit() {
        let paths = IdentityPaths::default_system();
        assert_eq!(
            paths.device_privkey,
            Path::new("/var/lib/tilo/device-private/keypair.bin")
        );
        assert_eq!(paths.device_dir, Path::new("/var/lib/tilo/device"));
        assert_eq!(
            paths.device_cert_for_user(&[0; 32]).parent().unwrap(),
            Path::new("/var/lib/tilo/device/certs")
        );
        let unit = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../os/usr/lib/systemd/system/tilo-device-key-init.service"
        ));
        // Every boot, not only when the key is missing: the key is given its owner and mode
        // again, so an install from any earlier state settles on the one state the daemon reads.
        assert!(
            !unit
                .lines()
                .any(|line| line.starts_with("ConditionPathExists="))
        );
        assert!(
            unit.lines()
                .any(|line| line == "Before=tilo-firstboot.service greetd.service fs.service")
        );
        assert!(
            unit.lines()
                .any(|line| line == "CapabilityBoundingSet=CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER")
        );
        // `@privileged` stays denied; the chown family is added back on a later allow line,
        // after the deny line, which is the only order systemd's merged filter permits it in.
        let deny = unit
            .lines()
            .position(|line| line.starts_with("SystemCallFilter=~") && line.contains("@privileged"))
            .expect("the filter denies @privileged");
        let allow = unit
            .lines()
            .position(|line| line == "SystemCallFilter=chown fchownat")
            .expect("the chown call is allowed after the deny list");
        assert!(
            allow > deny,
            "the chown family is added after `@privileged` is denied"
        );
        assert!(
            !unit.lines().any(|line| line
                == "SystemCallFilter=~@mount @network-io @resources @obsolete @debug @swap"),
            "the deny list keeps `@privileged`"
        );
        // The branch check runs at every boot, before the services that act
        // as the device and the names a copy shares are first read.
        let unit = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../os/usr/lib/systemd/system/tilo-device-branch.service"
        ));
        assert!(
            unit.lines()
                .any(|line| line == "ExecStart=/usr/libexec/tilo/tilo-device-key-init --branch")
        );
        assert!(
            unit.lines()
                .any(|line| line == "ConditionPathExists=/var/lib/tilo/device-private/keypair.bin")
        );
        assert!(
            unit.lines()
                .any(|line| line == "ConditionVirtualization=!container")
        );
        for before in [
            "fs.service",
            "tilo-server.service",
            "greetd.service",
            "NetworkManager.service",
            "sshd-keygen.target",
        ] {
            assert!(
                unit.lines().any(|line| line.starts_with("Before=")
                    && line
                        .split_whitespace()
                        .any(|unit| unit.trim_start_matches("Before=") == before)),
                "{before}"
            );
        }
    }
}
