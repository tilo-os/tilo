//! `tilo-default-start`: after an alongside install, make the running Tilo the
//! firmware's default start, and undo it.
//!
//! An alongside install leaves `BootOrder` exactly as the retained OS had it
//! and starts Tilo once through `BootNext`. The next restart would boot the
//! retained OS. This moves Tilo's own `Boot####` (the one the firmware started:
//! `BootCurrent`) into the place the retained OS held, so every network-boot
//! entry before that place keeps its position and its rescue still starts.
//!
//! Asking it again restores the order it wrote: the note
//! ([`CHANGE_PATH`]) holds what was changed and when, so the same action undoes
//! itself. It never writes a boot option, only `BootOrder`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{EFI_GLOBAL, Machine, Refusal, ids, option, refuse, var};

/// Where the change is noted, beside the other machine notes.
pub const CHANGE_PATH: &str = "/var/lib/tilo/boot-order.json";
const SCHEMA: u32 = 1;

/// One change of the default start, durable so the same action can return the
/// firmware to what it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// [`SCHEMA`].
    pub schema: u32,
    /// Tilo's own `Boot####` when it was moved: the entry the firmware started.
    pub boot_current: String,
    /// `BootOrder` before the change, the retained OS's place included.
    pub original_order: Vec<String>,
    /// `BootOrder` the change wrote.
    pub changed_order: Vec<String>,
    /// When it was written, nanoseconds since the epoch.
    pub changed_at_ns: u64,
    /// When the change was undone, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_at_ns: Option<u64>,
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    /// `BootOrder` differs from what it was when asked.
    pub changed: bool,
    /// The recorded change was undone, rather than a new one made.
    pub restored: bool,
    /// One sentence for the person who asked.
    pub message: String,
}

/// Make the running Tilo the default start, or restore the recorded order when
/// a change this tool recorded is still in force.
///
/// # Errors
/// The firmware facts do not allow a safe change: no root, no `BootOrder` or
/// `BootCurrent`, a pending `BootNext`, an entry that is unreadable, or a
/// note that no longer matches the firmware.
pub fn default_start(machine: &dyn Machine, change_path: &Path) -> Result<Done, Refusal> {
    if machine.euid() != 0 {
        return refuse("as root only: BootOrder is firmware state");
    }
    let order = ids(machine, "BootOrder")?
        .filter(|order| !order.is_empty())
        .ok_or_else(|| Refusal("BootOrder is absent or empty".into()))?;
    let mut unique = order.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != order.len() {
        return refuse("BootOrder lists an entry twice");
    }
    let current = match ids(machine, "BootCurrent")?.as_deref() {
        Some([current]) => current.clone(),
        Some(_) => return refuse("BootCurrent does not name one entry"),
        None => {
            return refuse(
                "BootCurrent is absent; this firmware did not keep the entry it started",
            );
        }
    };
    if let Some(next) = ids(machine, "BootNext")?.filter(|next| !next.is_empty()) {
        return refuse(format!(
            "BootNext is set to {}; a one-shot start is pending",
            next.join(",")
        ));
    }
    // Every entry named anywhere must read as one device path, or nothing is
    // touched: an unreadable entry could be the rescue's network boot.
    let mut network = Vec::new();
    for id in &order {
        network.push(network_entry(machine, id)?);
    }
    option(machine, &current)?;

    if let Some(change) = load(change_path)?.filter(|change| change.restored_at_ns.is_none()) {
        return restore(machine, change_path, &change, &current, &order);
    }

    // The retained OS holds the first place in BootOrder that is neither Tilo
    // nor a network boot; Tilo takes exactly that place. With no such place,
    // Tilo follows the entries that are there.
    let (wanted, replaced) = planned_order(machine, &order, &current, &network)?;
    if wanted == order {
        return Ok(Done {
            changed: false,
            restored: false,
            message: format!("Tilo (Boot{current}) already starts by default"),
        });
    }
    // Every entry `wanted` names was read above: the order's own entries as
    // device paths, Tilo's as the OS entry it boots from.
    write_order(machine, &wanted)?;
    let change = Change {
        schema: SCHEMA,
        boot_current: current.clone(),
        original_order: order,
        changed_order: wanted.clone(),
        changed_at_ns: tilo_clock::unix_now_ns(),
        restored_at_ns: None,
    };
    save(change_path, &change)?;
    let ahead = wanted
        .iter()
        .position(|id| *id == current)
        .unwrap_or_default();
    let message = match replaced {
        Some(old) if ahead == 1 => format!(
            "Tilo (Boot{current}) now starts by default where Boot{old} did; the boot entry ahead kept its place"
        ),
        Some(old) => format!(
            "Tilo (Boot{current}) now starts by default where Boot{old} did; {ahead} boot entries ahead kept their place"
        ),
        None => {
            format!("Tilo (Boot{current}) now starts by default after the network boot entries")
        }
    };
    Ok(Done {
        changed: true,
        restored: false,
        message,
    })
}

/// `BootOrder` with Tilo in the retained OS's place, and the entry it
/// replaced. Every entry stays otherwise in order; Tilo's own earlier place
/// goes.
fn planned_order(
    machine: &dyn Machine,
    order: &[String],
    current: &str,
    network: &[bool],
) -> Result<(Vec<String>, Option<String>), Refusal> {
    let mut place = None;
    let mut replaced = None;
    for (at, id) in order.iter().enumerate() {
        if id.as_str() == current || network.get(at).copied().unwrap_or(false) {
            continue;
        }
        option(machine, id)?;
        place = Some(at);
        replaced = Some(id.clone());
        break;
    }
    let at = place.unwrap_or(order.len());
    let keep = |id: &String| id != current && Some(id) != replaced.as_ref();
    let mut wanted: Vec<String> = order.iter().filter(|id| keep(id)).cloned().collect();
    let before = order.iter().take(at).filter(|id| keep(id)).count();
    wanted.insert(before, current.to_owned());
    Ok((wanted, replaced))
}

fn restore(
    machine: &dyn Machine,
    change_path: &Path,
    change: &Change,
    current: &str,
    order: &[String],
) -> Result<Done, Refusal> {
    if change.boot_current != current {
        return refuse(format!(
            "Boot{current} starts now but Boot{} was made the default; BootCurrent names another system",
            change.boot_current
        ));
    }
    if change.changed_order != order {
        return refuse(format!(
            "BootOrder is {} but {} was written; leaving firmware this tool did not change",
            order.join(","),
            change.changed_order.join(",")
        ));
    }
    for id in &change.original_order {
        verify_entry(machine, id)?;
    }
    write_order(machine, &change.original_order)?;
    let mut restored = change.clone();
    restored.restored_at_ns = Some(tilo_clock::unix_now_ns());
    save(change_path, &restored)?;
    let mut starts = change.original_order.first().map_or("", String::as_str);
    for id in &change.original_order {
        if !network_entry(machine, id)? {
            starts = id;
            break;
        }
    }
    Ok(Done {
        changed: true,
        restored: true,
        message: format!("restored the earlier boot order; Boot{starts} starts by default again"),
    })
}

/// Whether `Boot####` boots over the network: its device path holds a
/// messaging network node (MAC, IPv4/IPv6, DNS, UDP, TCP or URI), or is a BBS
/// entry. Anything else is an operating system's own entry.
fn network_entry(machine: &dyn Machine, boot_id: &str) -> Result<bool, Refusal> {
    let nodes = nodes(machine, boot_id)?;
    Ok(nodes.iter().any(|(kind, subtype)| {
        (*kind == 0x03 && ((0x0b..=0x10).contains(subtype) || *subtype == 0x18)) || *kind == 0x05
    }))
}

/// An entry is readable: a network boot's device path, or an operating
/// system's GPT partition and EFI file.
fn verify_entry(machine: &dyn Machine, boot_id: &str) -> Result<(), Refusal> {
    if network_entry(machine, boot_id)? {
        Ok(())
    } else {
        option(machine, boot_id).map(|_| ())
    }
}

/// The `(type, subtype)` nodes of `Boot####`'s device path.
fn nodes(machine: &dyn Machine, boot_id: &str) -> Result<Vec<(u8, u8)>, Refusal> {
    let name = format!("Boot{boot_id}");
    let raw = match var(machine, &name, EFI_GLOBAL)? {
        Some(raw) if raw.len() >= 10 => raw,
        _ => return refuse(format!("{name} is absent or truncated")),
    };
    let attributes = raw
        .get(..4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map_or(0, u32::from_le_bytes);
    if attributes & 1 == 0 {
        return refuse(format!("{name} is inactive"));
    }
    let length = raw
        .get(4..6)
        .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
        .map_or(0, u16::from_le_bytes);
    let length = usize::from(length);
    let mut cursor = 6usize;
    while cursor.saturating_add(1) < raw.len()
        && raw.get(cursor..cursor.saturating_add(2)) != Some(&[0, 0][..])
    {
        cursor = cursor.saturating_add(2);
    }
    if cursor.saturating_add(2) > raw.len() {
        return refuse(format!("{name} has no description terminator"));
    }
    cursor = cursor.saturating_add(2);
    let path = raw
        .get(cursor..)
        .and_then(|path| path.get(..length.min(path.len())))
        .unwrap_or_default();
    if path.len() != length {
        return refuse(format!("{name} device path is truncated"));
    }
    let mut nodes = Vec::new();
    let mut at = 0usize;
    while at.saturating_add(4) <= path.len() {
        let kind = path.get(at).copied().unwrap_or(0);
        let subtype = path.get(at.saturating_add(1)).copied().unwrap_or(0);
        let size = path
            .get(at.saturating_add(2)..at.saturating_add(4))
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map_or(0, u16::from_le_bytes);
        let size = usize::from(size);
        if size < 4 || at.saturating_add(size) > path.len() {
            return refuse(format!("{name} has a malformed device path"));
        }
        nodes.push((kind, subtype));
        at = at.saturating_add(size);
    }
    if at != path.len() {
        return refuse(format!("{name} has a malformed device path"));
    }
    Ok(nodes)
}

fn write_order(machine: &dyn Machine, order: &[String]) -> Result<(), Refusal> {
    let joined = order.join(",");
    let result = machine.efibootmgr(&["-q", "-o", &joined])?;
    if !result.status.success() {
        return refuse(format!(
            "efibootmgr failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    if ids(machine, "BootOrder")?.as_deref() != Some(order) {
        return refuse(
            "BootOrder did not read back as written; inspect efibootmgr -v before further changes",
        );
    }
    Ok(())
}

fn change_path() -> PathBuf {
    PathBuf::from(CHANGE_PATH)
}

/// The note written by the last change, if any.
fn load(path: &Path) -> Result<Option<Change>, Refusal> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return refuse(format!("{}: {error}", path.display())),
    };
    let change: Change = serde_json::from_slice(&bytes)
        .map_err(|error| Refusal(format!("{}: {error}", path.display())))?;
    if change.schema != SCHEMA {
        return refuse(format!(
            "{}: schema {} is not {SCHEMA}",
            path.display(),
            change.schema
        ));
    }
    Ok(Some(change))
}

fn save(path: &Path, change: &Change) -> Result<(), Refusal> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| Refusal(format!("{}: {error}", parent.display())))?;
    }
    let bytes = serde_json::to_vec_pretty(change)
        .map_err(|error| Refusal(format!("{}: {error}", path.display())))?;
    match tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600) {
        Ok(()) => Ok(()),
        // The firmware boot order is already changed by now; once the change
        // note is renamed into place, a refused directory sync is best effort.
        #[cfg(any(unix, windows))]
        Err(error)
            if tilo_files::native::atomic::publication_state(&error)
                == Some(
                    tilo_files::native::atomic::PublicationState::PublishedDurabilityUnknown,
                ) =>
        {
            Ok(())
        }
        Err(error) => Err(Refusal(format!("{}: {error}", path.display()))),
    }
}

/// The change note path, for the command line.
pub fn path() -> PathBuf {
    change_path()
}

const USAGE: &str = "usage: tilo-default-start [-h]\n";
const HELP: &str = "

Make the running Tilo the firmware's default start after an alongside install.
Every network-boot entry before the retained OS's place keeps its position, so
a network rescue still starts. Ask again to restore the order it wrote.

options:
  -h, --help  show this help message and exit
";

/// The command line: the exit status and what it printed.
pub fn main(
    machine: &dyn Machine,
    args: &[std::ffi::OsString],
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        let _ = write!(out, "{USAGE}{HELP}");
        return 0;
    }
    if !args.is_empty() {
        let _ = writeln!(
            err,
            "{USAGE}tilo-default-start: error: unrecognized arguments: {}",
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );
        return 2;
    }
    match default_start(machine, &path()) {
        Ok(done) => {
            let _ = writeln!(out, "{}", done.message);
            0
        }
        Err(refusal) => {
            let _ = writeln!(err, "tilo-default-start: {refusal}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};

    fn sample_change(current: &str, changed_at_ns: u64) -> Change {
        Change {
            schema: SCHEMA,
            boot_current: current.into(),
            original_order: vec!["0001".into(), "0002".into()],
            changed_order: vec!["0002".into(), "0001".into()],
            changed_at_ns,
            restored_at_ns: None,
        }
    }

    #[test]
    fn save_keeps_old_note_on_failed_replace_and_succeeds_when_post_rename_dir_sync_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join("boot-order.json");
        let first = sample_change("0002", 1_000);
        let second = sample_change("0003", 2_000);

        save(&path, &first).unwrap();
        assert_eq!(load(&path).unwrap(), Some(first.clone()));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let failed = with_failure(Checkpoint::BeforePublish, || save(&path, &second));
        assert!(failed.is_err());
        assert_eq!(load(&path).unwrap(), Some(first));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        // Once the change note is renamed into place, a refused directory sync
        // must not fail save (the firmware BootOrder is already updated).
        with_failure(Checkpoint::BeforeCommit, || save(&path, &second)).unwrap();
        assert_eq!(load(&path).unwrap(), Some(second));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
