//! About / Tilo Identity pane — device identity and transfer.
//!
//! Reads real system facts where they're cheap: `/etc/os-release`
//! (`PRETTY_NAME`), `/proc/sys/kernel/osrelease` (kernel),
//! `/proc/uptime` (uptime), and the hostname. Every read is best-effort —
//! a missing file degrades to `"unknown"` so the pane renders cleanly
//! under `--shots` on minimal containers.
//!
//! The "device identity" block surfaces a stable device name + a short
//! identity fingerprint (the handle peers see in collaboration). Moving a
//! session between machines is driven from the compositor's P2P layer, not
//! from this app, so this pane reports identity truthfully rather than
//! offering transfer buttons it can't fulfil.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;
use ui::widget::{Button, Row, Spacer, Widget};

use crate::host::PaneCx;
use crate::i18n::t;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{block_section, card, hero, note, row, rows, value_row};

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
                move || os_actions::open_tool(portable_identity_backup_intent()),
                |_: &mut SettingsApp, opened: Result<(), String>, _| {
                    opened.map(|()| Effected::done())
                },
            ))
        }
    }
}

fn fact(value: &str) -> String {
    if value == "unknown" {
        t("settings-about-unknown")
    } else {
        value.to_string()
    }
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
    pub identity_name: Option<String>,
    pub identity_public_key: Option<[u8; 32]>,
}

impl AboutInfo {
    pub fn from_system() -> Self {
        let mut info = Self::from_paths(
            Path::new("/etc/os-release"),
            Path::new("/proc/sys/kernel/osrelease"),
            Path::new("/proc/uptime"),
            Path::new("/proc/sys/kernel/hostname"),
            Path::new("/proc/cpuinfo"),
            Path::new("/proc/meminfo"),
        );
        // Only who this is is shown, so only the identity's public fields
        // are read: a record on disk is read without asking its secret
        // provider, which may wait on (or prompt for) the system secret
        // store while the window waits.
        if let Ok(identity) = runtime_identity::load_metadata() {
            info.identity_name =
                Some(identity.display_name.trim().to_string()).filter(|name| !name.is_empty());
            info.identity_public_key = Some(identity.group_public_key);
        }
        info
    }

    pub fn from_paths(
        os_release: &Path,
        kernel_path: &Path,
        uptime_path: &Path,
        hostname_path: &Path,
        cpuinfo_path: &Path,
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
            identity_name: None,
            identity_public_key: None,
        }
    }

    /// A short, stable workspace identity fingerprint.
    ///
    /// Connected workspaces use the public group key that identifies the
    /// person. The hostname-derived value is only a fallback for workspaces
    /// without an identity.
    pub fn fingerprint(&self) -> String {
        // The workspace identity is the real thing to fingerprint; the
        // hostname hash is only a stand-in until one has been provisioned.
        let fingerprint = self.identity_public_key.map_or_else(
            || {
                let mut hash: u64 = 1_469_598_103_934_665_603;
                for b in self.hostname.bytes() {
                    hash ^= u64::from(b);
                    hash = hash.wrapping_mul(1_099_511_628_211);
                }
                hash
            },
            |[a, b, c, d, e, f, g, h, ..]| u64::from_be_bytes([a, b, c, d, e, f, g, h]),
        );
        let hex = format!("{fingerprint:016x}");
        format!(
            "{}·{}·{}·{}",
            hex.get(0..4).unwrap_or_default(),
            hex.get(4..8).unwrap_or_default(),
            hex.get(8..12).unwrap_or_default(),
            hex.get(12..16).unwrap_or_default()
        )
    }
}

pub fn build(cx: &PaneCx) -> Box<dyn Widget> {
    build_with(cx, &AboutInfo::from_system())
}

pub fn build_with(cx: &PaneCx, info: &AboutInfo) -> Box<dyn Widget> {
    // A device-identity hero: the machine's name in the accent, the OS beneath.
    let hero_block = block_section(Box::new(
        ui::widget::Column::new()
            .with_no_padding()
            .push(hero(ui::icons::Icon::Monitor, &info.hostname))
            .push(Box::new(
                ui::widget::Text::new(&info.pretty_name)
                    .with_role(ui::tokens::typography::TextRole::Body),
            )),
    ));

    let person = info
        .identity_name
        .as_deref()
        .map_or_else(|| t("settings-about-not-connected"), str::to_string);
    let pretty = fact(&info.pretty_name);
    let kernel = fact(&info.kernel);
    let arch = fact(&info.arch);
    let uptime = fact(&info.uptime);
    let cpu = fact(&info.cpu);
    let memory = fact(&info.memory);
    let backup_label = t("settings-about-backup-button");
    let identity = card(
        &t("settings-about-identity-card"),
        rows(vec![
            value_row(&t("settings-about-person"), &person),
            value_row(&t("settings-about-identity"), &info.fingerprint()),
            note(&t("settings-about-identity-caption")),
        ]),
    );

    let backup = card(
        &t("settings-about-backup-card"),
        rows(vec![
            note(&t("settings-about-backup-caption")),
            row(Box::new(
                Row::new()
                    .push(Box::new(cx.bind_invocation(
                        Button::new(PORTABLE_BACKUP_BUTTON, &backup_label),
                        AboutEffect::BackUpIdentity,
                    )))
                    .push(Box::new(Spacer::new())),
            )),
        ]),
    );

    let system = card(
        &t("settings-about-system-card"),
        rows(vec![
            value_row(&t("settings-about-os"), &pretty),
            value_row(&t("settings-about-kernel"), &kernel),
            value_row(&t("settings-about-architecture"), &arch),
            value_row(&t("settings-about-uptime"), &uptime),
        ]),
    );

    let hardware = card(
        &t("settings-about-hardware-card"),
        rows(vec![
            value_row(&t("settings-about-processor"), &cpu),
            value_row(&t("settings-about-memory"), &memory),
        ]),
    );

    crate::widgets::cards(vec![hero_block, identity, backup, system, hardware])
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
        assert_eq!(format_memory(512 * 1024), "512.0 MiB");
    }

    /// Missing system files fall back to honest placeholders; the device
    /// fingerprint is deterministic and, with a workspace identity, derives
    /// from its public key; the backup action is a shell-owned typed intent.
    #[test]
    fn about_info_falls_back_and_fingerprints_the_identity() {
        let missing = Path::new("/nonexistent/tilo/settings/test/path");
        let mut info = AboutInfo::from_paths(missing, missing, missing, missing, missing, missing);
        assert_eq!(info.pretty_name, "unknown");
        assert_eq!(info.kernel, "unknown");
        assert_eq!(info.uptime, "unknown");
        assert_eq!(info.hostname, "this-device");
        assert_eq!(info.cpu, "unknown");
        assert_eq!(info.memory, "unknown");
        // Architecture is a compile-time fact, always present.
        assert!(!info.arch.is_empty());
        info.identity_public_key = Some([0xab; 32]);
        assert_eq!(info.fingerprint(), "abab·abab·abab·abab");

        let info = AboutInfo {
            pretty_name: "x".into(),
            kernel: "x".into(),
            uptime: "x".into(),
            hostname: "ada-laptop".into(),
            arch: "x86_64".into(),
            cpu: "x".into(),
            memory: "x".into(),
            identity_name: None,
            identity_public_key: None,
        };
        let fp = info.fingerprint();
        // 4 groups of 4 hex chars separated by middots.
        assert_eq!(fp.matches('\u{00B7}').count(), 3);
        assert_eq!(info.fingerprint(), fp, "deterministic");

        let intent = portable_identity_backup_intent();
        assert_eq!(intent, os_actions::ToolIntent::IdentityBackup);
        assert_eq!(intent.package_id(), None);
    }
}
