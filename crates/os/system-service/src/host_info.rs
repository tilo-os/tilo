//! The `host-info` resource: what [`tilo_system_resources::host_info`] says it is, read from the
//! files the kernel and the OS image answer with, and from the hardware report.
//!
//! The report is the one the installers read: its model, firmware, kernel, architecture and each
//! component's support come from [`hardware_report::SupportReport::probe`], so About answers with
//! what an installation checked, not a second reading of the same machine. What the report does
//! not carry — the OS image's name, the uptime, the hostname, the processor's model and the
//! memory — is read from `/etc/os-release`, `/proc` and the board's system on a chip. A fact
//! that cannot be read is "unknown", which the About page leaves out rather than guess.

use std::path::Path;

use hardware_report::SupportReport;
use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::host_info::{HOST_INFO, HostComponent, HostInfo, HostInfoAction};

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
    let report = SupportReport::probe();
    let mut info = facts_from(
        Path::new("/etc/os-release"),
        Path::new("/proc/uptime"),
        Path::new("/proc/sys/kernel/hostname"),
        &[Path::new("/proc/cpuinfo"), Path::new("/sys/devices/soc0")],
        Path::new("/proc/meminfo"),
    );
    read_report(&mut info, &report);
    info
}

/// Take what the hardware report establishes, into `info`: its model and firmware, its kernel
/// and architecture, and each component class's support. A report that cannot identify this
/// computer, or found nothing, leaves the hardware facts empty and says so.
fn read_report(info: &mut HostInfo, report: &SupportReport) {
    if let Some(kernel) = report.kernel.as_deref().and_then(usable) {
        kernel.clone_into(&mut info.kernel);
    }
    if let Some(architecture) = report.architecture.as_deref().and_then(usable) {
        architecture.clone_into(&mut info.arch);
    }
    if report.validate().is_err() || report.components.is_empty() {
        info.unknown = true;
        return;
    }
    info.model = report.identity.name().unwrap_or_default();
    info.firmware = report.identity.firmware.clone().unwrap_or_default();
    info.blocked = report.blocks_install();
    info.components = report
        .components
        .iter()
        .map(|component| HostComponent {
            kind: component.kind.key().to_owned(),
            name: component.name.clone(),
            state: component.state.key().to_owned(),
            present: component.present,
        })
        .collect();
}

/// The facts `paths` answer with; `unknown` marks what could not be read. The hardware report
/// fills the kernel and architecture, which no path here answers.
#[must_use]
pub fn facts_from(
    os_release: &Path,
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
    let missing = [&pretty_name, &uptime, &cpu, &memory]
        .iter()
        .any(|fact| fact.as_str() == "unknown");
    HostInfo {
        pretty_name,
        kernel: unknown(),
        uptime,
        hostname,
        arch: unknown(),
        cpu,
        memory,
        unknown: missing,
        ..HostInfo::default()
    }
}

/// The value, when it says something.
fn usable(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
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

#[cfg(test)]
mod tests {
    use super::*;
    use hardware_report::{
        Applicability, ComponentKind, ComponentSupport, DestinationIdentity, REPORT_SCHEMA,
        ReportSource, SupportReason, SupportState,
    };

    /// This computer, as its probe answers: a model it names and one component it observed.
    fn report() -> SupportReport {
        SupportReport {
            schema: REPORT_SCHEMA.into(),
            identity: DestinationIdentity {
                source: ReportSource::LocalScan,
                manufacturer: Some("Example".into()),
                model: Some("Laptop 14".into()),
                variant: Some("BOARD-1".into()),
                board: None,
                sku: None,
                firmware: Some("1.2.3".into()),
            },
            kernel: Some("6.18.58".into()),
            architecture: Some("x86_64".into()),
            components: vec![ComponentSupport {
                kind: ComponentKind::Graphics,
                name: "Radeon 780M".into(),
                present: true,
                state: SupportState::Unverified,
                applicability: Applicability::ObservationOnly,
                reason: SupportReason::NotDetected,
                evidence: Vec::new(),
                remedy: None,
            }],
        }
    }

    #[test]
    fn the_report_supplies_the_model_the_kernel_and_the_components() {
        let mut info = HostInfo::default();
        read_report(&mut info, &report());
        assert_eq!(info.kernel, "6.18.58");
        assert_eq!(info.arch, "x86_64");
        assert_eq!(info.model, "Example Laptop 14 (BOARD-1)");
        assert_eq!(info.firmware, "1.2.3");
        assert_eq!(
            info.components,
            vec![HostComponent {
                kind: "graphics".into(),
                name: "Radeon 780M".into(),
                state: "unverified".into(),
                present: true,
            }]
        );
        assert!(!info.blocked);
        assert!(!info.unknown);
    }

    #[test]
    fn a_report_that_does_not_identify_this_computer_says_so_and_names_no_hardware() {
        let mut info = HostInfo::default();
        let mut unreadable = report();
        unreadable.schema = "tilo.hardware-report.v1".into();
        read_report(&mut info, &unreadable);
        assert_eq!(info.kernel, "6.18.58", "the running kernel is still known");
        assert!(info.components.is_empty(), "no component is claimed");
        assert!(info.model.is_empty(), "no model is claimed");
        assert!(info.unknown, "the reading says what it could not establish");
    }

    #[test]
    fn a_component_the_desktop_needs_is_reported_as_blocking() {
        let mut info = HostInfo::default();
        let mut report = report();
        report.components[0].kind = ComponentKind::Storage;
        report.components[0].state = SupportState::Unsupported;
        read_report(&mut info, &report);
        assert!(info.blocked, "storage is essential and unsupported");
    }
}
