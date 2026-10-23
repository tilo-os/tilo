//! About: the facts of this device.
//!
//! Reads real system facts where they're cheap: `/etc/os-release`
//! (`PRETTY_NAME`), `/proc/sys/kernel/osrelease` (kernel),
//! `/proc/uptime` (uptime), the hostname, the processor (`/proc/cpuinfo`,
//! or the system-on-chip's own name in `/sys/devices/soc0` where the CPU
//! reports no model, as on Arm) and the memory. A fact that cannot be read
//! has no row.
//!
//! Who the person is lives in Users, which reads the identities from the
//! one place that keeps them; About shows no second copy that could
//! disagree with it. Backing up the identity is offered there too; its
//! action stays [`AboutEffect::BackUpIdentity`].

use std::fs;
use std::path::Path;
use std::time::Duration;

use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;
use ui::widget::Widget;

use crate::host::PaneCx;
use crate::i18n::t;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{block_section, hero, rows, section, value_row};

crate::effects::effect_family!(AboutEffect => answer);

/// What About offers to do with this device's identity.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AboutEffect {
    /// Open the identity backup, to carry the person's identity to a new
    /// device.
    #[effect(job, label = "Back up identity")]
    BackUpIdentity,
}

/// Answer `effect` by opening the tool it names.
#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AboutEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        AboutEffect::BackUpIdentity => {
            if !app.host().reaches_devices() {
                return Err("only a window someone uses opens the identity backup".into());
            }
            Ok(crate::device_read::job(
                cx,
                move || os_actions::open_tool(portable_identity_backup_intent(), None),
                |_: &mut SettingsApp, opened: Result<(), String>, _| {
                    opened.map(|()| Effected::done())
                },
            ))
        }
    }
}

/// A fact's row, when the fact could be read.
fn fact_row(label: &str, value: &str) -> Option<crate::widgets::Item> {
    (value != "unknown").then(|| value_row(label, value))
}

pub const PORTABLE_BACKUP_BUTTON: &str = "about.identity-backup.button";

pub(crate) fn portable_identity_backup_intent() -> os_actions::ToolIntent {
    os_actions::ToolIntent::IdentityBackup
}

/// Snapshot of the data the About pane reads at construction time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutInfo {
    pub pretty_name: String,
    pub kernel: String,
    pub uptime: String,
    pub hostname: String,
    pub arch: String,
    pub cpu: String,
    pub memory: String,
}

impl AboutInfo {
    pub fn from_system() -> Self {
        Self::from_paths(
            Path::new("/etc/os-release"),
            Path::new("/proc/sys/kernel/osrelease"),
            Path::new("/proc/uptime"),
            Path::new("/proc/sys/kernel/hostname"),
            &[Path::new("/proc/cpuinfo"), Path::new("/sys/devices/soc0")],
            Path::new("/proc/meminfo"),
        )
    }

    pub fn from_paths(
        os_release: &Path,
        kernel_path: &Path,
        uptime_path: &Path,
        hostname_path: &Path,
        [cpuinfo_path, soc_dir]: &[&Path; 2],
        meminfo_path: &Path,
    ) -> Self {
        let pretty_name = fs::read_to_string(os_release)
            .ok()
            .and_then(|s| parse_os_release_pretty_name(&s))
            .unwrap_or_else(|| "unknown".to_string());
        let kernel = fs::read_to_string(kernel_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let uptime = fs::read_to_string(uptime_path)
            .ok()
            .and_then(|s| parse_uptime_seconds(&s))
            .map_or_else(|| "unknown".to_string(), format_uptime);
        let hostname = fs::read_to_string(hostname_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "this-device".to_string());
        let cpu = fs::read_to_string(cpuinfo_path)
            .ok()
            .and_then(|s| parse_cpu_model(&s))
            .or_else(|| soc_name(soc_dir))
            .unwrap_or_else(|| "unknown".to_string());
        let memory = fs::read_to_string(meminfo_path)
            .ok()
            .and_then(|s| parse_mem_total_kb(&s))
            .map_or_else(|| "unknown".to_string(), format_memory);
        // Compile-time target arch equals the running machine's arch — an
        // honest fact that needs no syscall.
        let arch = std::env::consts::ARCH.to_string();
        Self {
            pretty_name,
            kernel,
            uptime,
            hostname,
            arch,
            cpu,
            memory,
        }
    }
}

pub fn build(_cx: &PaneCx) -> Box<dyn Widget> {
    build_with(&AboutInfo::from_system())
}

pub fn build_with(info: &AboutInfo) -> Box<dyn Widget> {
    // The device's name in the accent, the OS beneath: said once.
    let mut hero_column = ui::widget::Column::new()
        .with_no_padding()
        .push(hero(ui::icons::Icon::Monitor, &info.hostname));
    if info.pretty_name != "unknown" {
        hero_column = hero_column.push(Box::new(
            ui::widget::Text::new(&info.pretty_name)
                .with_role(ui::tokens::typography::TextRole::Body),
        ));
    }
    let mut sections = vec![block_section(Box::new(hero_column))];

    let system: Vec<_> = [
        fact_row(&t("settings-about-kernel"), &info.kernel),
        fact_row(&t("settings-about-architecture"), &info.arch),
        fact_row(&t("settings-about-uptime"), &info.uptime),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !system.is_empty() {
        sections.push(section(&t("settings-about-system-card"), rows(system)));
    }

    let hardware: Vec<_> = [
        fact_row(&t("settings-about-processor"), &info.cpu),
        fact_row(&t("settings-about-memory"), &info.memory),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !hardware.is_empty() {
        sections.push(section(&t("settings-about-hardware-card"), rows(hardware)));
    }

    crate::widgets::page(sections)
}

/// The system-on-chip's own name, where the kernel publishes it
/// (`/sys/devices/soc0`): its family and part, `Snapdragon X1E80100`. A
/// family given only as a JEDEC vendor code names no processor.
pub(crate) fn soc_name(soc_dir: &Path) -> Option<String> {
    let read = |name: &str| {
        fs::read_to_string(soc_dir.join(name))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let family = read("family").filter(|family| !family.starts_with("jep106:"))?;
    Some(match read("machine") {
        Some(machine) => format!("{family} {machine}"),
        None => family,
    })
}

pub(crate) fn parse_os_release_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            let unquoted = rest.trim_matches('"').trim_matches('\'').to_string();
            if !unquoted.is_empty() {
                return Some(unquoted);
            }
        }
    }
    None
}

/// Pull the CPU model from `/proc/cpuinfo` — the first `model name` line.
pub(crate) fn parse_cpu_model(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some((key, val)) = line.split_once(':')
            && key.trim() == "model name"
        {
            let v = val.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Pull total memory (in kB) from `/proc/meminfo`'s `MemTotal` line.
pub(crate) fn parse_mem_total_kb(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
        }
    }
    None
}

/// Format a memory size reported in KiB (as `/proc/meminfo` does) with
/// binary units, `15.6 GiB`.
pub(crate) fn format_memory(kb: u64) -> String {
    ui::format::memory(kb.saturating_mul(1024))
}

pub(crate) fn parse_uptime_seconds(contents: &str) -> Option<f64> {
    contents
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
}

pub(crate) fn format_uptime(seconds: f64) -> String {
    let dur = Duration::from_secs_f64(seconds.max(0.0));
    let total = dur.as_secs();
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

    /// The /etc/os-release, /proc/uptime, /proc/cpuinfo and /proc/meminfo
    /// parsers and their display formats.
    #[test]
    fn system_facts_parse_and_format() {
        for (input, want) in [
            (
                "NAME=Fedora\nPRETTY_NAME=\"Fedora Linux 42\"\nID=fedora\n",
                Some("Fedora Linux 42"),
            ),
            ("PRETTY_NAME=Fedora42\n", Some("Fedora42")),
            ("NAME=Foo\n", None),
        ] {
            assert_eq!(
                parse_os_release_pretty_name(input).as_deref(),
                want,
                "{input:?}"
            );
        }

        // Only the first field is the uptime.
        assert_eq!(parse_uptime_seconds("12345.67 6789.01\n"), Some(12345.67));
        for (seconds, want) in [(93_780.0, "1d 2h 3m"), (7_380.0, "2h 3m"), (420.0, "7m")] {
            assert_eq!(format_uptime(seconds), want, "{seconds}s");
        }

        // The first model name wins.
        let s = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 7840U\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7 7840U\n";
        assert_eq!(parse_cpu_model(s).as_deref(), Some("AMD Ryzen 7 7840U"));
        assert!(parse_cpu_model("no model here\n").is_none());

        // 16 GiB in kB.
        assert_eq!(
            parse_mem_total_kb("MemTotal:       16330124 kB\n"),
            Some(16_330_124)
        );
        assert!(parse_mem_total_kb("SwapTotal: 0 kB\n").is_none());
        assert_eq!(format_memory(16_330_124), "15.6 GiB");
        assert_eq!(format_memory(512 * 1024), "512 MiB");
    }

    /// Missing system files fall back to honest placeholders; the backup
    /// action is a shell-owned typed intent.
    #[test]
    fn about_info_falls_back_when_nothing_can_be_read() {
        let missing = Path::new("/nonexistent/tilo/settings/test/path");
        let info = AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[missing, missing],
            missing,
        );
        assert_eq!(info.pretty_name, "unknown");
        assert_eq!(info.kernel, "unknown");
        assert_eq!(info.uptime, "unknown");
        assert_eq!(info.hostname, "this-device");
        assert_eq!(info.cpu, "unknown");
        assert_eq!(info.memory, "unknown");
        // Architecture is a compile-time fact, always present.
        assert!(!info.arch.is_empty());

        let intent = portable_identity_backup_intent();
        assert_eq!(intent, os_actions::ToolIntent::IdentityBackup);
        assert_eq!(intent.package_id(), None);
    }

    #[test]
    fn about_names_the_real_processor_or_no_processor_and_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        let cpuinfo = dir.path().join("cpuinfo");
        // An Arm CPU reports no model name.
        std::fs::write(
            &cpuinfo,
            "processor\t: 0\nCPU implementer\t: 0x51\nCPU part\t: 0x001\n",
        )
        .unwrap();
        let soc = dir.path().join("soc0");
        std::fs::create_dir(&soc).unwrap();
        std::fs::write(soc.join("family"), "Snapdragon\n").unwrap();
        std::fs::write(soc.join("machine"), "X1E80100\n").unwrap();
        let missing = Path::new("/nonexistent/tilo/settings/test/path");
        let info = AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[&cpuinfo, &soc],
            missing,
        );
        assert_eq!(info.cpu, "Snapdragon X1E80100", "the chip's own name");
        let text = crate::widgets::announced_text(build_with(&info).as_ref());
        assert!(
            text.iter().any(|line| line == "Snapdragon X1E80100"),
            "{text:#?}"
        );

        std::fs::write(soc.join("family"), "jep106:0070\n").unwrap();
        let info = AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[&cpuinfo, &soc],
            missing,
        );
        let text = crate::widgets::announced_text(build_with(&info).as_ref());
        for gone in [
            t("settings-about-processor"),
            "unknown".to_owned(),
            t("settings-about-memory"),
        ] {
            assert!(
                !text.contains(&gone),
                "a fact that cannot be read has no row: {gone} in {text:#?}"
            );
        }
        // Who the person is is Users' to say; About keeps no second copy.
        for gone in ["Person", "Not connected", "Workspace identity", "Identity"] {
            assert!(!text.iter().any(|line| line == gone), "{gone}: {text:#?}");
        }
    }
}
