//! Kernel-authenticated credential capture; serialized facts never grant authority.
use super::*;

impl OriginalCallerFacts {
    /// Capture only an already admitted original application peer. A request PID,
    /// service worker's NSS credentials, or source inode group is not authority.
    pub(in crate::capture) fn capture(
        peer: &runtime_portal::process_binding::ExecutingPeer,
    ) -> io::Result<Self> {
        if !peer.current() {
            return Err(refused("original peer exited"));
        }
        let before = crate::capture::process::Incarnation::capture(peer.pid(), peer.uid())?;
        let first = read_status(peer)?;
        let second = read_status(peer)?;
        let after = crate::capture::process::Incarnation::capture(peer.pid(), peer.uid())?;
        if before != after || !peer.current() || first != second {
            return Err(refused("original peer credentials or incarnation changed"));
        }
        first.validate(peer.uid())?;
        Ok(first)
    }
}

fn read_status(
    peer: &runtime_portal::process_binding::ExecutingPeer,
) -> io::Result<OriginalCallerFacts> {
    use std::io::Read;
    if !peer.current() {
        return Err(refused("original peer exited before credential read"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{}/status", peer.pid()))?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(refused("original credential status exceeds bound"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| refused("original credential status is malformed"))?;
    let mut uid = None;
    let mut gid = None;
    let mut groups = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Uid:") {
            if uid.replace(parse_ids(value, 4)?).is_some() {
                return Err(refused("duplicate original UID"));
            }
        } else if let Some(value) = line.strip_prefix("Gid:") {
            if gid.replace(parse_ids(value, 4)?).is_some() {
                return Err(refused("duplicate original GID"));
            }
        } else if let Some(value) = line.strip_prefix("Groups:") {
            if groups.replace(parse_ids(value, MAX_GROUPS)?).is_some() {
                return Err(refused("duplicate original groups"));
            }
        }
    }
    let uid = uid.ok_or_else(|| refused("original UID missing"))?;
    let gid = gid.ok_or_else(|| refused("original GID missing"))?;
    let mut groups = groups.ok_or_else(|| refused("original groups missing"))?;
    groups.sort_unstable();
    // Kernel setgroups permits duplicates. Multiplicity grants no authority;
    // canonicalize identically to the proof socket's kernel group snapshot.
    groups.dedup();
    // Privileged/changing real/effective/saved/filesystem IDs are not guessed.
    if uid.len() != 4
        || gid.len() != 4
        || uid.iter().any(|id| *id != peer.uid())
        || gid.iter().any(|id| *id != peer.gid())
        || groups != peer.supplementary_groups()?
        || !peer.current()
    {
        return Err(refused("original kernel credentials changed or differ"));
    }
    Ok(OriginalCallerFacts {
        schema: 1,
        uid: peer.uid(),
        primary_gid: peer.gid(),
        supplementary_gids: groups,
    })
}
fn parse_ids(value: &str, maximum: usize) -> io::Result<Vec<u32>> {
    let mut ids = Vec::new();
    for field in value.split_ascii_whitespace() {
        if ids.len() >= maximum {
            return Err(refused("original credentials exceed bound"));
        }
        let id = field
            .parse::<u32>()
            .map_err(|_| refused("original credential ID malformed"))?;
        if id == u32::MAX {
            return Err(refused("original credential ID invalid"));
        }
        ids.push(id);
    }
    Ok(ids)
}
