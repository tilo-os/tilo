//! The `host-info` resource: what [`tilo_system_resources::host_info`] says it is, read from the
//! files the kernel and the OS image answer with.
//!
//! The facts come from the same files everywhere: `/etc/os-release`, the kernel release, the
//! uptime, the hostname, the processor's model (or the board's system on a chip) and the memory.
//! A fact that cannot be read is "unknown", which the About page leaves out rather than guess.

use std::path::Path;

use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::host_info::{HOST_INFO, HostInfo, HostInfoAction};

use crate::i18n::name;

/// The `host-info` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.host", label = "About this device")]
pub struct HostInfoReading;

/// The `host-info` resource reading `host_info`.
#[must_use]
pub fn resource(_info: &HostInfo) -> ResourceSpec {
    offer::<HostInfoAction>(tilo_resource::typed_resource::<HostInfoReading>(
        HOST_INFO,
        name("so.tilo.system.host", "About this device"),
    ))
}

/// The facts of this device now.
#[must_use]
pub fn facts() -> HostInfo {
    facts_from(
        Path::new("/etc/os-release"),
        Path::new("/proc/sys/kernel/osrelease"),
        Path::new("/proc/uptime"),
        Path::new("/proc/sys/kernel/hostname"),
        &[Path::new("/proc/cpuinfo"), Path::new("/sys/devices/soc0")],
        Path::new("/proc/meminfo"),
    )
}

/// The facts `paths` answer with; `unknown` marks what could not be read.
#[must_use]
pub fn facts_from(
    os_release: &Path,
    kernel_path: &Path,
    uptime_path: &Path,
    hostname_path: &Path,
    [cpuinfo_path, soc_dir]: &[&Path; 2],
    meminfo_path: &Path,
) -> HostInfo {
    let pretty_name = std::fs::read_to_string(os_release)
        .ok()
        .and_then(|contents| tilo_os_release::parse(&contents).ok())
        .and_then(|mut fields| fields.remove("PRETTY_NAME"))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(unknown);
    let kernel = std::fs::read_to_string(kernel_path)
        .ok()
        .map(|contents| contents.trim().to_owned())
        .filter(|kernel| !kernel.is_empty())
        .unwrap_or_else(unknown);
    let uptime = std::fs::read_to_string(uptime_path)
        .ok()
        .and_then(|contents| parse_uptime_seconds(&contents))
        .map_or_else(unknown, format_uptime);
    let hostname = std::fs::read_to_string(hostname_path)
        .ok()
        .map(|contents| contents.trim().to_owned())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| "this-device".to_owned());
    let cpu = std::fs::read_to_string(cpuinfo_path)
        .ok()
        .and_then(|contents| parse_cpu_model(&contents))
        .or_else(|| soc_name(soc_dir))
        .unwrap_or_else(unknown);
    let memory = std::fs::read_to_string(meminfo_path)
        .ok()
        .and_then(|contents| parse_mem_total_kb(&contents))
        .map_or_else(unknown, format_memory);
    let arch = std::env::consts::ARCH.to_owned();
    let unknown = [&pretty_name, &kernel, &uptime, &cpu, &memory]
        .iter()
        .any(|fact| fact.as_str() == "unknown");
    HostInfo {
        pretty_name,
        kernel,
        uptime,
        hostname,
        arch,
        cpu,
        memory,
        unknown,
        pending: Vec::new(),
        problem: None,
    }
}

fn unknown() -> String {
    "unknown".to_owned()
}

fn parse_cpu_model(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(':')
            && key.trim() == "model name"
        {
            let model = value.trim();
            if !model.is_empty() {
                return Some(model.to_owned());
            }
        }
    }
    None
}

fn soc_name(soc_dir: &Path) -> Option<String> {
    let read = |name: &str| {
        std::fs::read_to_string(soc_dir.join(name))
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let family = read("family").filter(|family| !family.starts_with("jep106:"))?;
    Some(match read("machine") {
        Some(machine) => format!("{family} {machine}"),
        None => family,
    })
}

fn parse_mem_total_kb(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|number| number.parse::<u64>().ok());
        }
    }
    None
}

/// The memory in words, `33.6 GB` or `512 MB`, the way sizes read everywhere else.
fn format_memory(kilobytes: u64) -> String {
    tilo_formats::ByteFormat::decimal()
        .min_unit(tilo_formats::ByteUnit::Mega)
        .decimal_from(tilo_formats::ByteUnit::Giga)
        .rounding(tilo_formats::ByteRounding::Truncate)
        .format(kilobytes.saturating_mul(1024))
}

fn parse_uptime_seconds(contents: &str) -> Option<f64> {
    contents
        .split_whitespace()
        .next()
        .and_then(|seconds| seconds.parse::<f64>().ok())
}

fn format_uptime(seconds: f64) -> String {
    let total = std::time::Duration::try_from_secs_f64(seconds).map_or(0, |up| up.as_secs());
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}
