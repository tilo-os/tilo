//! What a write finishes on a disk that already carries its installed system
//! and account, after the staged files are copied onto it.
//!
//! An installer medium hands its owner SSH keys to `tilo-install`, which
//! knows the account it creates. A full-disk image (a board card, a VM disk)
//! never runs the installer: its account exists already, so the write gives
//! that account the same key-only SSH login, places the operator's files in
//! its home, and labels everything it placed with the image's own `SELinux`
//! policy. An enforcing boot ignores a Wi-Fi profile, an SSH key or a
//! hostname that kept the setup machine's labels.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The app chooses its staged request paths and validation policy.
#[derive(Clone, Copy)]
pub struct Requests {
    pub ssh_key: &'static str,
    pub home_files: &'static str,
    pub validate_keys: fn(&str) -> io::Result<()>,
    pub valid_home_name: fn(&str) -> bool,
}

/// sshd's unit, as the image's enablement link names it.
const SSHD_UNIT: &str = "/usr/lib/systemd/system/sshd.service";

/// The key-only login `tilo-install-dev-ssh` configures on a self-install.
fn bringup_sshd_config(username: &str) -> String {
    format!(
        "AllowUsers {username}\nPermitRootLogin no\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
         AuthorizedKeysFile /etc/ssh/authorized_keys/%u\n"
    )
}

/// Paths the copy is about to create, recorded before it runs so the
/// labelling touches only what the write placed: files, and directories
/// that did not exist before.
#[derive(Debug, Default)]
pub struct Placed {
    etc: Vec<PathBuf>,
    var: Vec<PathBuf>,
}

impl Placed {
    /// Record where `staged`'s `etc/` and `var/` trees land under `etc` and
    /// `var`.
    ///
    /// # Errors
    /// Returns source inventory or destination metadata errors.
    pub fn before_copy(staged: &Path, etc: &Path, var: &Path) -> io::Result<Self> {
        Ok(Self {
            etc: landing_paths(&staged.join("etc"), etc)?,
            var: landing_paths(&staged.join("var"), var)?,
        })
    }
}

fn landing_paths(from: &Path, to: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !from.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let relative = path.strip_prefix(from).map_err(io::Error::other)?;
            let dest = to.join(relative);
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                // A new directory is labelled whole; inside an existing one
                // only the files the copy adds are.
                if fs::symlink_metadata(&dest).is_err() {
                    out.push(dest);
                } else {
                    stack.push(path);
                }
            } else if meta.is_file() {
                out.push(dest);
            }
        }
    }
    Ok(out)
}

/// Apply the owner SSH request and the home files to the disk's account and
/// label everything placed.
///
/// # Errors
///
/// Returns an error when the request needs an account the disk does not
/// have exactly one of, the image has no OpenSSH server, a path on the disk
/// is a link where a directory or file belongs, or labelling fails.
pub fn finish(
    etc: &Path,
    var: &Path,
    staged: &Path,
    mut placed: Placed,
    requests: Requests,
) -> io::Result<()> {
    let request = etc.join(
        requests
            .ssh_key
            .strip_prefix("etc/")
            .unwrap_or(requests.ssh_key),
    );
    let keys = match fs::symlink_metadata(&request) {
        Ok(meta) if meta.is_file() => Some(fs::read_to_string(&request)?),
        Ok(_) => {
            return Err(io::Error::other(
                "the owner SSH key request on the disk is not a regular file",
            ));
        }
        Err(_) => None,
    };
    let home_files = staged.join(requests.home_files);
    let has_home_files = home_files.is_dir();
    if keys.is_some() || has_home_files {
        let account = login_account(etc)?;
        if let Some(keys) = keys {
            (requests.validate_keys)(&keys)?;
            placed
                .etc
                .extend(apply_owner_ssh(etc, &account.name, &keys)?);
            fs::remove_file(&request)?;
        }
        if has_home_files {
            placed.var.extend(place_home_files(
                var,
                &account,
                &home_files,
                requests.valid_home_name,
            )?);
        }
    }
    placed.etc.retain(|path| fs::symlink_metadata(path).is_ok());
    label_with_image_policy(etc, var, &placed)
}

/// The account a full-disk image was built with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) name: String,
    uid: u32,
    gid: u32,
    home: String,
}

/// The one login account in the installed `/etc/passwd`: a regular UID with
/// a shell that logs in.
///
/// # Errors
///
/// Returns an error when the disk has none or several, since the owner's
/// keys and files would otherwise go to a guess.
pub(crate) fn login_account(etc: &Path) -> io::Result<Account> {
    let passwd = read_regular(&etc.join("passwd"))?;
    let accounts = passwd
        .lines()
        .filter_map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            let [name, _, uid, gid, _, home, shell] = fields.as_slice() else {
                return None;
            };
            let uid = uid.parse::<u32>().ok()?;
            let logs_in = !shell.ends_with("/nologin") && !shell.ends_with("/false");
            ((1000..60000).contains(&uid) && logs_in).then(|| Account {
                name: (*name).to_string(),
                uid,
                gid: gid.parse().unwrap_or(uid),
                home: (*home).to_string(),
            })
        })
        .collect::<Vec<_>>();
    match <[Account; 1]>::try_from(accounts) {
        Ok([account]) => {
            if account.name.is_empty()
                || account.name.len() > 32
                || account.name.starts_with('-')
                || !account
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(io::Error::other(
                    "the disk login account name is not a plain SSH user name",
                ));
            }
            Ok(account)
        }
        Err(accounts) => Err(io::Error::other(format!(
            "the disk has {} login accounts, so it is not clear whose SSH login and home files these are",
            accounts.len()
        ))),
    }
}

fn read_regular(path: &Path) -> io::Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::other(format!(
            "{} on the disk is not a regular file",
            path.display()
        )));
    }
    fs::read_to_string(path)
}

/// A directory under `root`, created as needed, that is never a link.
fn real_dir(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let mut current = root.to_path_buf();
    for part in Path::new(relative).components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} on the disk is not a directory",
                    current.display()
                )));
            }
            Err(_) => {
                fs::create_dir(&current)?;
                set_mode(&current, 0o755)?;
            }
        }
    }
    Ok(current)
}

/// Replace `path` with `contents`, never writing through a link.
fn write_file(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            return Err(io::Error::other(format!(
                "{} on the disk is a directory",
                path.display()
            )));
        }
        Ok(_) => fs::remove_file(path)?,
        Err(_) => {}
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    set_mode(path, mode)
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Key-only SSH login for `username` with the requested keys, and sshd
/// enabled. Returns the paths written.
fn apply_owner_ssh(etc: &Path, username: &str, keys: &str) -> io::Result<Vec<PathBuf>> {
    if !fs::symlink_metadata(etc.join("ssh/sshd_config")).is_ok_and(|meta| meta.is_file()) {
        return Err(io::Error::other(
            "the image has no OpenSSH server, so the owner SSH keys cannot be applied",
        ));
    }
    let mut written = Vec::new();
    let keys_dir = etc.join("ssh/authorized_keys");
    let keys_dir_is_new = fs::symlink_metadata(&keys_dir).is_err();
    let keys_dir = real_dir(etc, "ssh/authorized_keys")?;
    if keys_dir_is_new {
        written.push(keys_dir.clone());
    }
    let keys_file = keys_dir.join(username);
    write_file(&keys_file, keys.as_bytes(), 0o644)?;
    written.push(keys_file);
    let config = real_dir(etc, "ssh/sshd_config.d")?.join("00-tilo-bringup.conf");
    write_file(&config, bringup_sshd_config(username).as_bytes(), 0o644)?;
    written.push(config);
    let enable = real_dir(etc, "systemd/system/multi-user.target.wants")?.join("sshd.service");
    match fs::symlink_metadata(&enable) {
        Ok(meta)
            if meta.file_type().is_symlink()
                && fs::read_link(&enable).is_ok_and(|dest| dest == Path::new(SSHD_UNIT)) => {}
        Ok(_) => {
            return Err(io::Error::other(
                "sshd's enablement on the disk is not the image's unit link",
            ));
        }
        Err(_) => {
            std::os::unix::fs::symlink(SSHD_UNIT, &enable)?;
            written.push(enable);
        }
    }
    Ok(written)
}

/// The installed `/var` path of the account's home, which `/etc/passwd`
/// names as `/var/home/account` or through the `/home` link.
fn installed_home(var: &Path, account: &Account) -> io::Result<PathBuf> {
    let relative = account
        .home
        .strip_prefix("/var/")
        .or_else(|| {
            account
                .home
                .strip_prefix('/')
                .filter(|rest| rest.starts_with("home/"))
        })
        .filter(|rest| {
            Path::new(rest)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        })
        .ok_or_else(|| {
            io::Error::other(format!(
                "{}'s home {} is not under /var/home",
                account.name, account.home
            ))
        })?;
    let mut home = var.to_path_buf();
    for component in std::iter::once(None).chain(Path::new(relative).components().map(Some)) {
        if let Some(component) = component {
            home.push(component);
        }
        if !fs::symlink_metadata(&home).is_ok_and(|meta| meta.is_dir()) {
            return Err(io::Error::other(format!(
                "{}'s home is not a real directory on the disk",
                account.name
            )));
        }
    }
    Ok(home)
}

/// Copy the staged home files into the account's home, owned by it.
fn place_home_files(
    var: &Path,
    account: &Account,
    staged: &Path,
    valid_name: fn(&str) -> bool,
) -> io::Result<Vec<PathBuf>> {
    let home = installed_home(var, account)?;
    let mut placed = Vec::new();
    for entry in fs::read_dir(staged)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().filter(|name| valid_name(name));
        let (Some(name), true) = (name, fs::symlink_metadata(entry.path())?.is_file()) else {
            return Err(io::Error::other(
                "a staged home file is not a plain, regular file",
            ));
        };
        let dest = home.join(name);
        write_file(&dest, &fs::read(entry.path())?, 0o644)?;
        std::os::unix::fs::lchown(&dest, Some(account.uid), Some(account.gid))?;
        placed.push(dest);
    }
    Ok(placed)
}

/// The newest compiled policy the image ships, which defines the types its
/// file contexts name.
fn image_binary_policy(etc: &Path) -> Option<PathBuf> {
    fs::read_dir(etc.join("selinux/targeted/policy"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let version = entry
                .file_name()
                .to_str()?
                .strip_prefix("policy.")?
                .parse::<u32>()
                .ok()?;
            Some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

/// `setfiles` runs for the deployment's `/etc` and the stateroot's `/var`
/// separately: `-r` labels each tree as if it were mounted at `/`, which is
/// what the image's `file_contexts` describes.
fn setfiles_commands(etc: &Path, var: &Path, placed: &Placed) -> Vec<Command> {
    let policy = etc.join("selinux/targeted/contexts/files/file_contexts");
    let binary = image_binary_policy(etc);
    [(etc, &placed.etc), (var, &placed.var)]
        .into_iter()
        .filter(|(_, paths)| !paths.is_empty())
        .filter_map(|(tree, paths)| {
            let root = tree.parent()?;
            let mut command = Command::new(crate::system_tool("setfiles"));
            command.arg("-F");
            if let Some(binary) = &binary {
                command.arg("-c").arg(binary);
            }
            command.arg("-r").arg(root).arg(&policy).args(paths);
            Some(command)
        })
        .collect()
}

fn label_with_image_policy(etc: &Path, var: &Path, placed: &Placed) -> io::Result<()> {
    let policy = etc.join("selinux/targeted/contexts/files/file_contexts");
    if !fs::symlink_metadata(&policy).is_ok_and(|meta| meta.is_file()) {
        return Ok(());
    }
    for command in setfiles_commands(etc, var, placed) {
        match bounded_command::run_prepared(
            command,
            None,
            1024 * 1024,
            std::time::Duration::from_secs(30),
        ) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let status = output.status;
                return Err(io::Error::other(format!(
                    "setfiles could not label the customization with the image's policy ({status})"
                )));
            }
            Err(bounded_command::Error::Process(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                return Err(io::Error::other(
                    "setfiles (policycoreutils) is needed to label the customization with the image's `SELinux` policy",
                ));
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const BRINGUP_SSH_KEY: &str = "etc/tilo/bringup-ssh.pub";
    fn requests() -> Requests {
        Requests {
            ssh_key: BRINGUP_SSH_KEY,
            home_files: "account-home",
            validate_keys: |keys| {
                if keys.lines().all(|line| line == KEY_A || line == KEY_B) {
                    Ok(())
                } else {
                    Err(io::Error::other("invalid fixture key"))
                }
            },
            valid_home_name: |name| name == "SETUP.md",
        }
    }

    const KEY_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const KEY_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB";

    /// A mounted composefs root with one login account and no policy. The
    /// account is this test's own user when that is a regular one, so its
    /// home files can be given to it without root.
    fn disk() -> (tempfile::TempDir, PathBuf, PathBuf) {
        use std::fmt::Write as _;
        use std::os::unix::fs::MetadataExt;
        let mnt = tempfile::tempdir().unwrap();
        let me = fs::metadata(mnt.path()).unwrap();
        let (uid, gid) = if (1000..60000).contains(&me.uid()) {
            (me.uid(), me.gid())
        } else {
            (1000, 1000)
        };
        let etc = mnt.path().join("state/deploy/abcd/etc");
        let var = mnt.path().join("state/os/default/var");
        fs::create_dir_all(etc.join("ssh/sshd_config.d")).unwrap();
        fs::write(
            etc.join("ssh/sshd_config"),
            "Include /etc/ssh/sshd_config.d/*.conf\n",
        )
        .unwrap();
        fs::write(
            etc.join("passwd"),
            "root:x:0:0:root:/root:/bin/bash\n\
             tilo-relay:x:980:980::/var/lib/tilo-relay:/usr/sbin/nologin\n\
             nobody:x:65534:65534:Kernel Overflow User:/:/usr/sbin/nologin\n",
        )
        .unwrap();
        let mut passwd = fs::read_to_string(etc.join("passwd")).unwrap();
        writeln!(passwd, "alice:x:{uid}:{gid}::/var/home/alice:/bin/bash").unwrap();
        fs::write(etc.join("passwd"), passwd).unwrap();
        fs::create_dir_all(var.join("home/alice")).unwrap();
        (mnt, etc, var)
    }

    #[test]
    fn the_login_account_is_the_one_regular_user() {
        let (_mnt, etc, _var) = disk();
        let account = login_account(&etc).unwrap();
        assert_eq!(account.name, "alice");
        assert_eq!(account.home, "/var/home/alice");

        let mut passwd = fs::read_to_string(etc.join("passwd")).unwrap();
        passwd.push_str("bob:x:59999:59999::/var/home/bob:/bin/bash\n");
        fs::write(etc.join("passwd"), passwd).unwrap();
        let error = login_account(&etc).unwrap_err();
        assert!(error.to_string().contains("2 login accounts"), "{error}");
    }

    #[test]
    fn owner_keys_become_key_only_login_for_the_disk_account() {
        let (_mnt, etc, var) = disk();
        let staged = tempfile::tempdir().unwrap();
        fs::create_dir_all(staged.path().join("etc/tilo")).unwrap();
        fs::write(
            staged.path().join(BRINGUP_SSH_KEY),
            format!("{KEY_A}\n{KEY_B}\n"),
        )
        .unwrap();
        fs::create_dir_all(etc.join("tilo")).unwrap();
        fs::copy(
            staged.path().join(BRINGUP_SSH_KEY),
            etc.join("tilo/bringup-ssh.pub"),
        )
        .unwrap();
        finish(&etc, &var, staged.path(), Placed::default(), requests()).unwrap();

        assert_eq!(
            fs::read_to_string(etc.join("ssh/authorized_keys/alice")).unwrap(),
            format!("{KEY_A}\n{KEY_B}\n")
        );
        let config =
            fs::read_to_string(etc.join("ssh/sshd_config.d/00-tilo-bringup.conf")).unwrap();
        for directive in [
            "AllowUsers alice",
            "PasswordAuthentication no",
            "AuthorizedKeysFile /etc/ssh/authorized_keys/%u",
        ] {
            assert!(config.contains(directive), "{config}");
        }
        assert_eq!(
            fs::read_link(etc.join("systemd/system/multi-user.target.wants/sshd.service")).unwrap(),
            Path::new(SSHD_UNIT)
        );
        assert!(
            !etc.join("tilo/bringup-ssh.pub").exists(),
            "the request is consumed"
        );

        // Customizing again replaces the keys instead of failing.
        fs::write(etc.join("tilo/bringup-ssh.pub"), format!("{KEY_B}\n")).unwrap();
        finish(&etc, &var, staged.path(), Placed::default(), requests()).unwrap();
        assert_eq!(
            fs::read_to_string(etc.join("ssh/authorized_keys/alice")).unwrap(),
            format!("{KEY_B}\n")
        );
    }

    #[test]
    fn owner_keys_need_an_openssh_server_and_no_planted_links() {
        let (_mnt, etc, var) = disk();
        fs::create_dir_all(etc.join("tilo")).unwrap();
        fs::write(etc.join("tilo/bringup-ssh.pub"), format!("{KEY_A}\n")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), etc.join("ssh/authorized_keys")).unwrap();
        let staged = tempfile::tempdir().unwrap();
        let error = finish(&etc, &var, staged.path(), Placed::default(), requests()).unwrap_err();
        assert!(error.to_string().contains("not a directory"), "{error}");
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);

        fs::remove_file(etc.join("ssh/authorized_keys")).unwrap();
        fs::remove_file(etc.join("ssh/sshd_config")).unwrap();
        let error = finish(&etc, &var, staged.path(), Placed::default(), requests()).unwrap_err();
        assert!(error.to_string().contains("no OpenSSH server"), "{error}");
    }

    #[test]
    fn home_files_land_in_the_account_home() {
        use std::os::unix::fs::MetadataExt;
        let (_mnt, etc, var) = disk();
        // The files are handed to the login account: a no-op when the test
        // runs as that account, and root's to do otherwise.
        let account = login_account(&etc).unwrap();
        if account.uid != fs::metadata(&var).unwrap().uid() {
            tilo_test_env::require_capability!(
                Chown,
                tilo_test_env::chown_to(account.uid),
                "the login account owns its home files"
            );
        }
        let staged = tempfile::tempdir().unwrap();
        fs::create_dir_all(staged.path().join("account-home")).unwrap();
        fs::write(staged.path().join("account-home/SETUP.md"), b"# Setup\n").unwrap();
        finish(&etc, &var, staged.path(), Placed::default(), requests()).unwrap();
        let placed = var.join("home/alice/SETUP.md");
        assert_eq!(fs::read(&placed).unwrap(), b"# Setup\n");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&placed).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        assert!(
            !etc.join("ssh/authorized_keys").exists(),
            "no keys were asked for"
        );
    }

    #[test]
    fn a_home_outside_var_home_is_refused() {
        let (_mnt, etc, var) = disk();
        let account = Account {
            name: "alice".into(),
            uid: 1000,
            gid: 1000,
            home: "/root/../var/home/alice".into(),
        };
        assert!(installed_home(&var, &account).is_err());
        let account = Account {
            home: "/home/alice".into(),
            ..account
        };
        assert_eq!(
            installed_home(&var, &account).unwrap(),
            var.join("home/alice")
        );
        let _ = etc;
    }

    #[test]
    fn account_names_and_home_parent_links_cannot_escape_the_image() {
        let (_mnt, etc, var) = disk();
        fs::write(
            etc.join("passwd"),
            "../outside:x:1000:1000::/var/home/alice:/bin/bash\n",
        )
        .unwrap();
        let error =
            login_account(&etc).expect_err("an account name cannot be used as an outside path");
        assert!(error.to_string().contains("plain SSH user name"), "{error}");
        fs::write(
            etc.join("passwd"),
            "alice:x:1000:1000::/var/home/alice:/bin/bash\n",
        )
        .unwrap();
        fs::remove_dir_all(var.join("home")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("alice")).unwrap();
        std::os::unix::fs::symlink(outside.path(), var.join("home")).unwrap();
        let staged = tempfile::tempdir().unwrap();
        fs::create_dir(staged.path().join("account-home")).unwrap();
        fs::write(
            staged.path().join("account-home/SETUP.md"),
            b"private setup",
        )
        .unwrap();
        let error = finish(&etc, &var, staged.path(), Placed::default(), requests())
            .expect_err("a home parent link cannot escape the image");
        assert!(
            error.to_string().contains("not a real directory"),
            "{error}"
        );
        assert!(!outside.path().join("alice/SETUP.md").exists());
    }

    #[test]
    fn labels_follow_each_tree_mounted_at_root() {
        let (_mnt, etc, var) = disk();
        let staged = tempfile::tempdir().unwrap();
        fs::create_dir_all(staged.path().join("etc/NetworkManager/system-connections")).unwrap();
        fs::write(
            staged
                .path()
                .join("etc/NetworkManager/system-connections/tilo-setup.nmconnection"),
            "x",
        )
        .unwrap();
        fs::write(staged.path().join("etc/hostname"), "board\n").unwrap();
        fs::create_dir_all(etc.join("NetworkManager")).unwrap();
        fs::create_dir_all(etc.join("selinux/targeted/policy")).unwrap();
        fs::write(etc.join("selinux/targeted/policy/policy.33"), "").unwrap();
        fs::write(etc.join("selinux/targeted/policy/policy.34"), "").unwrap();
        let placed = Placed::before_copy(staged.path(), &etc, &var).unwrap();
        let mut etc_paths = placed.etc.clone();
        etc_paths.sort();
        assert_eq!(
            etc_paths,
            vec![
                etc.join("NetworkManager/system-connections"),
                etc.join("hostname")
            ],
            "a new directory is labelled whole, an existing one is not"
        );
        let commands = setfiles_commands(&etc, &var, &placed);
        assert_eq!(commands.len(), 1, "nothing was placed under /var");
        let args = commands[0]
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let deploy = etc.parent().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            args[..5],
            [
                "-F".to_string(),
                "-c".into(),
                etc.join("selinux/targeted/policy/policy.34")
                    .to_string_lossy()
                    .into_owned(),
                "-r".into(),
                deploy
            ]
        );
    }
}
