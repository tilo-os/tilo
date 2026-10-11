//! Capacity of the supervisor's fixed user slice, not the calling process.
//! This is a limit observation, not resident headroom or a performance claim.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path};
use std::process::Command;
use std::time::Duration;

use local_model_service::supervision::MODEL_RUNTIME_SLICE;

pub(super) fn runtime_memory_mib() -> Result<u64, String> {
    let meminfo = bounded_text(Path::new("/proc/meminfo"))?;
    let mut query = Command::new("systemctl");
    query.args([
        "--user",
        "show",
        MODEL_RUNTIME_SLICE,
        "--property=LoadState,ControlGroup",
    ]);
    let output = bounded_command::run_prepared(query, None, 65_536, Duration::from_secs(5))
        .map_err(|error| format!("runtime memory capacity unavailable: {error}"))?;
    if !output.status.success() {
        return Err("runtime memory capacity unavailable: cannot observe the user slice".into());
    }
    let properties = String::from_utf8(output.stdout)
        .map_err(|_| "runtime memory capacity unavailable: invalid slice properties")?;
    capacity_mib(&meminfo, &properties, Path::new("/sys/fs/cgroup"))
}

pub(super) fn capacity_mib(meminfo: &str, properties: &str, mount: &Path) -> Result<u64, String> {
    let physical = physical_bytes(meminfo)?;
    let group = slice_group(properties)?;
    let root = fs::canonicalize(mount)
        .map_err(|error| format!("runtime memory capacity unavailable: {error}"))?;
    if !bounded_text(&root.join("cgroup.controllers"))?
        .split_whitespace()
        .any(|controller| controller == "memory")
    {
        return Err("runtime memory capacity unavailable: no cgroup v2 memory controller".into());
    }
    let relative = group.strip_prefix('/').ok_or("invalid slice cgroup path")?;
    if relative.is_empty()
        || relative.len() > 4096
        || Path::new(relative)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("runtime memory capacity unavailable: invalid slice cgroup path".into());
    }
    let selected = root.join(relative);
    let canonical = fs::canonicalize(&selected)
        .map_err(|error| format!("runtime memory capacity unavailable: {error}"))?;
    if canonical != selected {
        return Err("runtime memory capacity unavailable: redirected slice cgroup path".into());
    }
    let mut capacity = physical;
    for (depth, ancestor) in selected.ancestors().enumerate() {
        if depth > 128 || !ancestor.starts_with(&root) {
            return Err("runtime memory capacity unavailable: invalid cgroup ancestry".into());
        }
        let path = ancestor.join("memory.max");
        // A cgroup v2 mount root has no memory.max; its ceiling is physical RAM.
        // Every non-root ancestor must expose its limit, including "max".
        match fs::symlink_metadata(&path) {
            Err(error) if ancestor == root && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("runtime memory capacity unavailable: {error}"));
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("runtime memory capacity unavailable: redirected memory limit".into());
            }
            Ok(_) => {
                capacity = capacity.min(limit_bytes(&bounded_text(&path)?)?.unwrap_or(physical));
            }
        }
        if ancestor == root {
            let mib = capacity / (1024 * 1024);
            return (mib > 0)
                .then_some(mib)
                .ok_or_else(|| "runtime memory capacity unavailable: zero usable capacity".into());
        }
    }
    Err("runtime memory capacity unavailable: cgroup mount root not reached".into())
}

fn physical_bytes(meminfo: &str) -> Result<u64, String> {
    let mut totals = meminfo
        .lines()
        .filter_map(|line| line.strip_prefix("MemTotal:"));
    let total = totals
        .next()
        .ok_or("physical memory capacity unavailable")?;
    if totals.next().is_some() {
        return Err("physical memory capacity unavailable: duplicate total".into());
    }
    let fields: Vec<_> = total.split_whitespace().collect();
    let [value, "kB"] = fields.as_slice() else {
        return Err("physical memory capacity unavailable: invalid total".into());
    };
    decimal(value)?
        .checked_mul(1024)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "physical memory capacity unavailable: invalid total".into())
}

fn slice_group(properties: &str) -> Result<&str, String> {
    let mut state = None;
    let mut group = None;
    for line in properties.lines() {
        let (key, value) = line.split_once('=').ok_or("invalid slice properties")?;
        let field = match key {
            "LoadState" => &mut state,
            "ControlGroup" => &mut group,
            _ => {
                return Err(
                    "runtime memory capacity unavailable: unexpected slice property".into(),
                );
            }
        };
        if field.replace(value).is_some() {
            return Err("runtime memory capacity unavailable: duplicate slice property".into());
        }
    }
    if state != Some("loaded") {
        return Err("runtime memory capacity unavailable: user slice is not loaded".into());
    }
    group.ok_or_else(|| "runtime memory capacity unavailable: slice cgroup is missing".into())
}

fn limit_bytes(value: &str) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value == "max" {
        Ok(None)
    } else {
        decimal(value).map(Some)
    }
}

fn decimal(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("runtime memory capacity unavailable: malformed memory capacity".into());
    }
    value
        .parse()
        .map_err(|_| "runtime memory capacity unavailable: memory capacity overflow".into())
}

fn bounded_text(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(65_537).read_to_end(&mut bytes))
        .map_err(|error| format!("runtime memory capacity unavailable: {error}"))?;
    if bytes.len() > 65_536 {
        return Err("runtime memory capacity unavailable: oversized observation".into());
    }
    String::from_utf8(bytes)
        .map_err(|_| "runtime memory capacity unavailable: invalid observation encoding".into())
}

#[cfg(test)]
mod tests;
