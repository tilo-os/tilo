use super::*;
use std::path::Component;

#[derive(Clone, Debug)]
pub(super) struct Mount {
    pub(super) id: u64,
    pub(super) filesystem: String,
    pub(super) private: bool,
    pub(super) number: String,
    pub(super) root: PathBuf,
    pub(super) point: PathBuf,
    pub(super) source: PathBuf,
    pub(super) read_only: bool,
    pub(super) source_eligible: bool,
}

fn unescape(bytes: &[u8]) -> io::Result<PathBuf> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            let escape = bytes
                .get(
                    cursor
                        ..cursor
                            .checked_add(4)
                            .ok_or_else(|| refusal("Kernel mount path overflow."))?,
                )
                .ok_or_else(|| refusal("Truncated kernel mount path escape."))?;
            out.push(match escape {
                b"\\040" => b' ',
                b"\\011" => b'\t',
                b"\\012" => b'\n',
                b"\\134" => b'\\',
                _ => return Err(refusal("Unknown kernel mount path escape.")),
            });
            cursor = cursor
                .checked_add(4)
                .ok_or_else(|| refusal("Kernel mount path overflow."))?;
        } else {
            out.push(bytes[cursor]);
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| refusal("Kernel mount path overflow."))?;
        }
    }
    if out.contains(&0) {
        return Err(refusal("Kernel mount path contains a NUL."));
    }
    Ok(PathBuf::from(OsString::from_vec(out)))
}
fn flags(bytes: &[u8], wanted: &[u8]) -> bool {
    bytes.split(|byte| *byte == b',').any(|flag| flag == wanted)
}

pub(super) fn parse(bytes: &[u8]) -> io::Result<Vec<Mount>> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(refusal("Kernel mount table exceeds bounds."));
    }
    let mut mounts = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let fields: Vec<_> = line.split(|byte| *byte == b' ').collect();
        let dash = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(|| refusal("Malformed kernel mount table."))?;
        if dash < 6 || fields.len() != dash.saturating_add(4) {
            return Err(refusal("Malformed kernel mount table fields."));
        }
        let number = std::str::from_utf8(fields[2]).map_err(io::Error::other)?;
        let (major, minor) = number
            .split_once(':')
            .ok_or_else(|| refusal("Malformed kernel mount device number."))?;
        if major.parse::<u32>().is_err() || minor.parse::<u32>().is_err() {
            return Err(refusal("Malformed kernel mount device number."));
        }
        let root = unescape(fields[3])?;
        let point = unescape(fields[4])?;
        let cgroup_namespace = fields[dash.saturating_add(1)] == b"cgroup2";
        // Cgroup namespaces legitimately expose roots containing parents.
        // Retain the covering mount so selecting it refuses rather than
        // falling back to a containing storage mount.
        for (path, namespace_root) in [(&root, cgroup_namespace), (&point, false)] {
            if !path.is_absolute()
                || (!namespace_root
                    && path
                        .components()
                        .any(|part| matches!(part, Component::CurDir | Component::ParentDir)))
            {
                return Err(refusal(
                    "Kernel mount path is not a normalized absolute path.",
                ));
            }
        }
        let id = std::str::from_utf8(fields[0])
            .map_err(io::Error::other)?
            .parse::<u64>()
            .map_err(io::Error::other)?;
        if id == 0 || mounts.iter().any(|mount: &Mount| mount.id == id) {
            return Err(refusal("Kernel mount identity is invalid or repeated"));
        }
        mounts.push(Mount {
            private: !fields[6..dash]
                .iter()
                .any(|field| field.starts_with(b"shared:") || field.starts_with(b"master:")),
            id,
            filesystem: std::str::from_utf8(fields[dash.saturating_add(1)])
                .map_err(io::Error::other)?
                .to_owned(),
            number: number.into(),
            root,
            point,
            source: unescape(fields[dash.saturating_add(2)])?,
            source_eligible: !cgroup_namespace,
            read_only: flags(fields[5], b"ro") && flags(fields[dash.saturating_add(3)], b"ro"),
        });
    }
    Ok(mounts)
}
