//! Account creation and private first-login migration metadata delivery.
//!
//! The root seed is intent, not actor authority. Production account facts come
//! from NSS; fixture paths are accepted only for the unprivileged caller itself.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rustix::fs::{self, AtFlags, Mode, OFlags};
use rustix::process::{Gid, Uid};
use serde_json::Value;

use super::{Fail, HANDOFF_MAX_BYTES, Host, Result, Strict, canonical, refuse};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Default)]
struct Options {
    username: Option<String>,
    home: Option<PathBuf>,
    uid: Option<u32>,
    gid: Option<u32>,
    seed: Option<PathBuf>,
}

fn options(command: &str, args: &[OsString]) -> Result<Options> {
    if !matches!(command, "required-user-id" | "deliver-user-plan") {
        return refuse("unknown user migration command");
    }
    let mut parsed = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut parts = arg.as_bytes().splitn(2, |byte| *byte == b'=');
        let flag = parts.next().unwrap_or_default();
        let value = if let Some(value) = parts.next() {
            OsStr::from_bytes(value)
        } else {
            args.next().map(OsString::as_os_str).ok_or_else(|| {
                Fail::Refusal("user migration command option requires a value".into())
            })?
        };
        match flag {
            b"--username" => {
                parsed.username = Some(
                    value
                        .to_str()
                        .ok_or_else(|| Fail::Refusal("target username is not valid UTF-8".into()))?
                        .to_owned(),
                );
            }
            b"--test-home" if command == "deliver-user-plan" => parsed.home = Some(value.into()),
            b"--test-seed-dir" if command == "deliver-user-plan" => {
                parsed.seed = Some(value.into());
            }
            b"--test-uid" | b"--test-gid" if command == "deliver-user-plan" => {
                let id = value
                    .to_str()
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|id| *id != u32::MAX)
                    .ok_or_else(|| Fail::Refusal("fixture account ID is invalid".into()))?;
                if flag == b"--test-uid" {
                    parsed.uid = Some(id);
                } else {
                    parsed.gid = Some(id);
                }
            }
            _ => return refuse("unknown user migration command option"),
        }
    }
    if parsed.username.is_none() {
        return refuse("user migration command requires --username");
    }
    Ok(parsed)
}

fn valid_username(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    (1..=32).contains(&bytes.len())
        && (first.is_ascii_lowercase() || *first == b'_')
        && rest.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'_' | b'-')
        })
}

struct Account {
    uid: u32,
    gid: u32,
    home: PathBuf,
}

/// getent uses the installed NSS configuration, including directory services.
/// Never parse /etc/passwd as a substitute or resolve an executable via PATH.
fn account(host: &dyn Host, key: &str) -> Result<Option<Account>> {
    let output = host.run(&["/usr/bin/getent", "--", "passwd", key])?;
    if output.code == Some(2) && output.stdout.is_empty() {
        return Ok(None);
    }
    if output.code != Some(0) {
        return refuse("installed account lookup failed");
    }
    let mut lines = output.stdout.lines();
    let fields: Vec<_> = lines.next().unwrap_or_default().split(':').collect();
    let [_, _, uid, gid, _, home, _] = fields.as_slice() else {
        return refuse("installed account lookup returned malformed facts");
    };
    if lines.next().is_some() {
        return refuse("installed account lookup returned malformed facts");
    }
    let id = |value: &str| -> Result<u32> {
        value
            .parse::<u32>()
            .ok()
            .filter(|id| *id != u32::MAX)
            .ok_or_else(|| Fail::Refusal("installed account ID is invalid".into()))
    };
    Ok(Some(Account {
        uid: id(uid)?,
        gid: id(gid)?,
        home: (*home).into(),
    }))
}

fn account_by_name(host: &dyn Host, name: &str) -> Result<Option<Account>> {
    if !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()) {
        // getent treats numeric keys as UIDs, whereas getpwnam accepts numeric
        // usernames. id's explicit operand retains the name lookup semantics.
        let output = host.run(&["/usr/bin/id", "--user", "--", name])?;
        if output.code == Some(1) && output.stdout.is_empty() {
            return Ok(None);
        }
        if output.code != Some(0) {
            return refuse("installed account lookup failed");
        }
        let uid = output
            .stdout
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|uid| *uid != u32::MAX)
            .ok_or_else(|| Fail::Refusal("installed account ID is invalid".into()))?;
        return account(host, &uid.to_string());
    }
    account(host, name)
}

fn open_directory_chain(path: &Path) -> Result<OwnedFd> {
    let bytes = path.as_os_str().as_bytes();
    let Some(relative) = bytes.strip_prefix(b"/") else {
        return refuse("metadata delivery directory is not absolute");
    };
    let parts: Vec<_> = relative.split(|byte| *byte == b'/').collect();
    if parts.iter().any(|part| matches!(*part, b"" | b"." | b"..")) {
        return refuse("metadata delivery directory is not normalized");
    }
    let mut directory = fs::open("/", DIRECTORY_FLAGS, Mode::empty())?;
    for part in parts {
        directory = fs::openat(
            &directory,
            OsStr::from_bytes(part),
            DIRECTORY_FLAGS,
            Mode::empty(),
        )?;
    }
    Ok(directory)
}

fn read_plan_at(directory: &OwnedFd, name: &str, owner: u32) -> Result<Option<Value>> {
    let fd = match fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let file = File::from(fd);
    let facts = file.metadata()?;
    if !facts.is_file()
        || facts.uid() != owner
        || facts.mode() & 0o077 != 0
        || facts.len() > HANDOFF_MAX_BYTES as u64
    {
        return refuse("migration metadata is not a bounded private owner-controlled file");
    }
    let mut bytes = Vec::new();
    file.take((HANDOFF_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > HANDOFF_MAX_BYTES {
        return refuse("migration metadata exceeds its size bound");
    }
    let Strict(value) = serde_json::from_slice(&bytes)
        .map_err(|_| Fail::Refusal("migration metadata is not valid JSON".into()))?;
    super::validate_migration(&value).map(Some)
}

fn required_plan(directory: &OwnedFd, name: &str, owner: u32) -> Result<Value> {
    read_plan_at(directory, name, owner)?
        .ok_or_else(|| Fail::Refusal("migration metadata is missing".into()))
}

fn parse_user_range(contents: &str) -> Result<(u32, u32)> {
    let mut minimum = (1000_u32, false);
    let mut maximum = (60000_u32, false);
    for line in contents.lines() {
        let fields: Vec<_> = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        let (value, seen) = match fields.first().copied() {
            Some("UID_MIN") => &mut minimum,
            Some("UID_MAX") => &mut maximum,
            _ => continue,
        };
        let [_, number] = fields.as_slice() else {
            return refuse("installed regular-account range is malformed");
        };
        if *seen || number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return refuse("installed regular-account range is malformed");
        }
        *value = number
            .parse()
            .map_err(|_| Fail::Refusal("installed regular-account range is malformed".into()))?;
        *seen = true;
    }
    if minimum.0 < 1000 || minimum.0 > maximum.0 || maximum.0 == u32::MAX {
        return refuse("installed regular-account range is invalid");
    }
    Ok((minimum.0, maximum.0))
}

fn regular_user_range() -> Result<(u32, u32)> {
    let file = File::from(fs::open(
        "/etc/login.defs",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let facts = file.metadata()?;
    if !facts.is_file() || facts.uid() != 0 || facts.mode() & 0o022 != 0 || facts.len() > 65536 {
        return refuse("installed regular-account policy is unsafe");
    }
    let mut contents = String::new();
    file.take(65537).read_to_string(&mut contents)?;
    if contents.len() > 65536 {
        return refuse("installed regular-account policy exceeds its size bound");
    }
    parse_user_range(&contents)
}

fn required_user_id(host: &dyn Host, username: &str) -> Result<String> {
    if !rustix::process::geteuid().is_root() || !valid_username(username) {
        return refuse("user-ID query requires root and a valid target username");
    }
    let seed = open_directory_chain(Path::new("/var/lib/tilo"))?;
    let facts = fs::fstat(&seed)?;
    if facts.st_uid != 0 || facts.st_mode & 0o022 != 0 {
        return refuse("root migration seed directory has unsafe ownership or permissions");
    }
    let raw = required_plan(&seed, "migration-plan.json", 0)?;
    if raw
        .get("home_policy")
        .is_none_or(|policy| policy != "access-read-write")
    {
        return Ok(String::new());
    }
    let uid = raw
        .get("home")
        .and_then(|home| home.get("original_uid"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Fail::Refusal("original source UID is invalid".into()))?;
    let (minimum, maximum) = regular_user_range()?;
    if !(minimum..=maximum).contains(&uid) {
        return refuse("original source UID is outside the installed regular-account range");
    }
    if account(host, username)?.is_some() || account(host, &uid.to_string())?.is_some() {
        return refuse("target username or original source UID is already assigned");
    }
    Ok(format!("{uid}\n"))
}

struct Link {
    parent: OwnedFd,
    component: &'static str,
    device: u64,
    inode: u64,
}

fn validate_chain(chain: &[Link]) -> Result<()> {
    for link in chain {
        let current = fs::statat(&link.parent, link.component, AtFlags::SYMLINK_NOFOLLOW)?;
        if fs::FileType::from_raw_mode(current.st_mode) != fs::FileType::Directory
            || current.st_dev != link.device
            || current.st_ino != link.inode
        {
            return refuse("migration metadata directory was replaced during delivery");
        }
    }
    Ok(())
}

fn target(host: &dyn Host, options: &Options) -> Result<(Account, PathBuf)> {
    let euid = rustix::process::geteuid();
    let effective_group = rustix::process::getegid();
    let fixture = options.home.is_some()
        || options.seed.is_some()
        || options.uid.is_some()
        || options.gid.is_some();
    if fixture {
        if euid.is_root()
            || options.uid != Some(euid.as_raw())
            || options.gid != Some(effective_group.as_raw())
            || options.home.is_none()
            || options.seed.is_none()
        {
            return refuse("fixture delivery is limited to the unprivileged caller own UID/GID");
        }
        let home = options
            .home
            .clone()
            .ok_or_else(|| Fail::Refusal("fixture home is missing".into()))?;
        let seed = options
            .seed
            .clone()
            .ok_or_else(|| Fail::Refusal("fixture seed directory is missing".into()))?;
        return Ok((
            Account {
                uid: euid.as_raw(),
                gid: effective_group.as_raw(),
                home,
            },
            seed,
        ));
    }
    if !euid.is_root() {
        return refuse("firstboot delivery requires root and a non-root target account");
    }
    let username = options.username.as_deref().unwrap_or_default();
    let account = account_by_name(host, username)?
        .ok_or_else(|| Fail::Refusal("target account does not exist".into()))?;
    if account.uid == 0 {
        return refuse("firstboot delivery requires root and a non-root target account");
    }
    Ok((account, PathBuf::from("/var/lib/tilo")))
}

fn publish(directory: &OwnedFd, name: &str, raw: &Value, account: &Account) -> Result<()> {
    let data = canonical(raw);
    if let Some(existing) = read_plan_at(directory, name, account.uid)? {
        if canonical(&existing) != data {
            return refuse("existing user migration consent differs; refusing replacement");
        }
        return Ok(());
    }
    // The proc descriptor pins the destination, even if an ancestor is renamed.
    // NamedTempFile provides unpredictable O_EXCL mode-0600 creation and cleanup.
    let pinned = format!("/proc/self/fd/{}", directory.as_raw_fd());
    let mut temporary = tempfile::Builder::new()
        .prefix(".migration-")
        .tempfile_in(pinned)?;
    fs::fchown(
        temporary.as_file(),
        Some(Uid::from_raw(account.uid)),
        Some(Gid::from_raw(account.gid)),
    )?;
    temporary.write_all(&data)?;
    temporary.as_file().sync_all()?;
    let temporary_name = temporary
        .path()
        .file_name()
        .ok_or_else(|| Fail::Refusal("migration temporary name is missing".into()))?;
    fs::linkat(directory, temporary_name, directory, name, AtFlags::empty())?;
    fs::fsync(directory)?;
    if canonical(&required_plan(directory, name, account.uid)?) != data {
        return refuse("published user migration plan failed verification");
    }
    temporary.close()?;
    fs::fsync(directory)?;
    Ok(())
}

fn deliver_user_plan(host: &dyn Host, options: &Options) -> Result<String> {
    let (account, seed_path) = target(host, options)?;
    let seed = open_directory_chain(&seed_path)?;
    let raw = required_plan(
        &seed,
        "migration-plan.json",
        rustix::process::geteuid().as_raw(),
    )?;
    let mut destination = open_directory_chain(&account.home)?;
    if fs::fstat(&destination)?.st_uid != account.uid {
        return refuse("migration target home is not owned by its actual account");
    }
    let mut chain = Vec::new();
    for component in [".local", "state", "tilo", "migration-handoff"] {
        let created = match fs::mkdirat(&destination, component, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(rustix::io::Errno::EXIST) => false,
            Err(error) => return Err(error.into()),
        };
        let child = fs::openat(&destination, component, DIRECTORY_FLAGS, Mode::empty())?;
        if created {
            fs::fchown(
                &child,
                Some(Uid::from_raw(account.uid)),
                Some(Gid::from_raw(account.gid)),
            )?;
            fs::fchmod(&child, Mode::from_raw_mode(0o700))?;
            fs::fsync(&destination)?;
        }
        let facts = fs::fstat(&child)?;
        if facts.st_uid != account.uid
            || facts.st_mode & 0o022 != 0
            || (component == "migration-handoff" && facts.st_mode & 0o077 != 0)
        {
            return refuse("migration metadata ancestor has unsafe ownership or permissions");
        }
        chain.push(Link {
            parent: destination,
            component,
            device: facts.st_dev,
            inode: facts.st_ino,
        });
        destination = child;
    }
    validate_chain(&chain)?;
    let operation = raw
        .get("operation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::Refusal("migration operation ID is missing".into()))?;
    // Validation above uses the shared Migration model; retain a local path guard
    // so future model extensions can never turn an operation ID into a path.
    if operation.is_empty() || operation.contains('/') || operation.contains('\0') {
        return refuse("migration operation ID is not a filename");
    }
    publish(
        &destination,
        &format!("{operation}.migration.json"),
        &raw,
        &account,
    )?;
    validate_chain(&chain)?;
    // Keep the authoritative seed: first-login receipts establish completion.
    Ok("delivered migration metadata to the target account\n".into())
}

/// Run an account handoff command. The returned string is its complete stdout;
/// errors never include seed contents, source paths, or child diagnostics.
pub fn run(host: &dyn Host, command: &str, args: &[OsString]) -> Result<String> {
    let options = options(command, args)?;
    match command {
        "required-user-id" => {
            required_user_id(host, options.username.as_deref().unwrap_or_default())
        }
        "deliver-user-plan" => deliver_user_plan(host, &options),
        _ => refuse("unknown user migration command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_uid_range_is_strict_and_defaults_are_preserved() {
        assert_eq!(parse_user_range("# UID_MIN 4\n").unwrap(), (1000, 60000));
        assert_eq!(
            parse_user_range("UID_MIN 2000 # local policy\nUID_MAX 3000\n").unwrap(),
            (2000, 3000)
        );
        for policy in [
            "UID_MIN 999",
            "UID_MAX 999",
            "UID_MIN 2000\nUID_MAX 1000",
            "UID_MIN 1000\nUID_MIN 1000",
            "UID_MIN +1000",
            "UID_MIN ١٠٠٠",
            "UID_MAX 4294967295",
            "UID_MIN 1000 extra",
            "UID_MIN",
        ] {
            assert!(parse_user_range(policy).is_err(), "{policy}");
        }
    }

    #[test]
    fn normalized_directories_never_follow_symlinks() {
        for path in [
            "relative",
            "/",
            "//var",
            "/var/",
            "/var/../var",
            "/var/./lib",
        ] {
            assert!(open_directory_chain(Path::new(path)).is_err(), "{path}");
        }
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), root.path().join("link")).unwrap();
        assert!(open_directory_chain(&root.path().join("link")).is_err());
    }

    #[test]
    fn username_query_rejects_numeric_and_option_ambiguity() {
        for name in ["", "0", "1234", "-root", "Root", "user/name", "a:b", "a\n"] {
            assert!(!valid_username(name), "{name:?}");
        }
        assert!(valid_username("_account-2"));
        assert!(!valid_username(&"a".repeat(33)));
    }

    fn plan() -> Value {
        let cases: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/staging_handoff_authenticated_parity.json"
        ))
        .unwrap();
        cases
            .as_array()
            .unwrap()
            .iter()
            .find_map(|case| case.get("plan").filter(|plan| plan.is_object()).cloned())
            .expect("authenticated corpus must contain a valid migration")
    }

    #[test]
    fn publication_is_private_idempotent_and_preserves_conflicting_consent() {
        let root = tempfile::tempdir().unwrap();
        let directory = open_directory_chain(root.path()).unwrap();
        let account = Account {
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
            home: root.path().into(),
        };
        let raw = plan();
        let name = format!("{}.migration.json", raw["operation_id"].as_str().unwrap());
        publish(&directory, &name, &raw, &account).unwrap();
        publish(&directory, &name, &raw, &account).unwrap();
        let path = root.path().join(&name);
        assert_eq!(std::fs::read(&path).unwrap(), canonical(&raw));
        let facts = std::fs::metadata(&path).unwrap();
        assert_eq!(facts.mode() & 0o777, 0o600);
        assert_eq!(facts.uid(), account.uid);
        assert_eq!(facts.gid(), account.gid);
        let mut conflicting = raw.clone();
        conflicting["conflicting_consent"] = Value::Bool(true);
        assert!(publish(&directory, &name, &conflicting, &account).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), canonical(&raw));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn fixture_delivery_uses_only_the_callers_actual_account_and_keeps_seed() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let seed = tempfile::tempdir().unwrap();
        let raw = plan();
        let seed_file = seed.path().join("migration-plan.json");
        std::fs::write(&seed_file, canonical(&raw)).unwrap();
        std::fs::set_permissions(&seed_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let uid = rustix::process::geteuid();
        let gid = rustix::process::getegid();
        let args = vec![
            OsString::from("--username"),
            OsString::from("fixture-account"),
            OsString::from("--test-home"),
            home.path().into(),
            OsString::from("--test-seed-dir"),
            seed.path().into(),
            OsString::from("--test-uid"),
            uid.as_raw().to_string().into(),
            OsString::from("--test-gid"),
            gid.as_raw().to_string().into(),
        ];
        let result = run(&super::super::System, "deliver-user-plan", &args);
        if uid.is_root() {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("unprivileged caller")
            );
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        } else {
            assert_eq!(
                result.unwrap(),
                "delivered migration metadata to the target account\n"
            );
            run(&super::super::System, "deliver-user-plan", &args).unwrap();
            let destination = home.path().join(".local/state/tilo/migration-handoff");
            let name = format!("{}.migration.json", raw["operation_id"].as_str().unwrap());
            assert_eq!(
                std::fs::read(destination.join(name)).unwrap(),
                canonical(&raw)
            );
            assert_eq!(
                std::fs::metadata(&destination).unwrap().mode() & 0o777,
                0o700
            );
            assert_eq!(std::fs::read_dir(destination).unwrap().count(), 1);
        }
        assert_eq!(std::fs::read(seed_file).unwrap(), canonical(&raw));
        let mut wrong_actor = args;
        wrong_actor[7] = if uid.as_raw() == 1000 { "1001" } else { "1000" }.into();
        assert!(run(&super::super::System, "deliver-user-plan", &wrong_actor).is_err());
    }

    #[test]
    fn pinned_chain_detects_replaced_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("child")).unwrap();
        let directory = open_directory_chain(root.path()).unwrap();
        let child = fs::statat(&directory, "child", AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let chain = [Link {
            parent: directory,
            component: "child",
            device: child.st_dev,
            inode: child.st_ino,
        }];
        validate_chain(&chain).unwrap();
        std::fs::rename(root.path().join("child"), root.path().join("old")).unwrap();
        std::fs::create_dir(root.path().join("child")).unwrap();
        assert!(validate_chain(&chain).is_err());
    }

    #[test]
    fn private_seed_refuses_public_files_symlinks_and_duplicate_json() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = open_directory_chain(root.path()).unwrap();
        let owner = rustix::process::geteuid().as_raw();
        let path = root.path().join("migration-plan.json");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_plan_at(&directory, "migration-plan.json", owner).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, br#"{"schema":1,"schema":1}"#).unwrap();
        assert!(read_plan_at(&directory, "migration-plan.json", owner).is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing", &path).unwrap();
        assert!(read_plan_at(&directory, "migration-plan.json", owner).is_err());
    }
}
