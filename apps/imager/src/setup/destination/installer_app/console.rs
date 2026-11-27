//! The console endpoints the installer answers while it runs headless or unattended.

use imager_backend::destination::console::{self as console_control, ConsoleApp};
use serde_json::{Value, json};
use ui::tilo_actions::{ActionAddress, ActionFailure, EncodedAction, Exports as _, ResourceSpec};

use super::{
    FILESYSTEMS, InstallMethod, InstallerApp, RECOVERY_KEY_PREFIX, STEP_LABELS, Step,
    friendly_install_status, local_text, secret_copy, step_index,
};
use crate::setup::destination::effects::PROVIDER;

pub const CONSOLE_WAKE: &str = "\u{1}console";

const LOG_TAIL: usize = 20;

pub fn redact_recovery_material(line: &str) -> Option<String> {
    if line.contains(RECOVERY_KEY_PREFIX) {
        return None;
    }
    let looks_like_key = |token: &str| {
        let groups: Vec<&str> = token
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .split('-')
            .collect();
        groups.len() >= 6
            && groups.iter().all(|group| {
                (4..=8).contains(&group.len()) && group.chars().all(|c| c.is_ascii_alphanumeric())
            })
    };
    Some(
        line.split(' ')
            .map(|token| {
                if looks_like_key(token) {
                    "[redacted]"
                } else {
                    token
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

impl InstallerApp {
    #[must_use]
    pub fn with_console(mut self, console: console_control::ConsoleServer) -> Self {
        self.console = Some(console);
        self
    }

    pub(super) fn serve_console(&mut self) {
        let Some(console) = self.console.clone() else {
            return;
        };
        for asked in console.take_pending() {
            asked.answer(&mut Console(self));
        }
    }

    pub(super) fn step_id(&self) -> &'static str {
        match self.step {
            Step::Keyboard => "keyboard",
            Step::Network => "network",
            Step::Disk => "disk",
            Step::Account => "account",
            Step::Options => "options",
            Step::Media => "media",
            Step::Review => "review",
            Step::Install => "install",
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the state report mirrors the wizard in one place so no api drifts"
    )]
    pub fn console_state(&self) -> Value {
        let disks: Vec<Value> = self
            .disks
            .iter()
            .enumerate()
            .map(|(index, disk)| {
                json!({
                    "device": disk.name,
                    "size": disk.size,
                    "size_bytes": disk.size_bytes,
                    "model": disk.model,
                    "partitions": disk.partitions.len(),
                    "filesystems": disk.partition_filesystems(),
                    "installer_medium": disk.is_system,
                    "read_only": disk.read_only,
                    "too_small": disk.too_small(),
                    "removable": disk.removable,
                    "selected": self.selected_disk == Some(index),
                    "prepared_alongside": disk.can_install_alongside(),
                    "free_regions": disk.guided_alongside_plans().len(),
                })
            })
            .collect();
        let tail_from = self.status_lines.len().saturating_sub(LOG_TAIL);
        let log_tail: Vec<String> = self
            .status_lines
            .iter()
            .skip(tail_from)
            .filter_map(|line| redact_recovery_material(line))
            .collect();
        let preflight = if self.storage_preflight_request.is_some() {
            "running"
        } else if self.storage_preflight_passed {
            "passed"
        } else {
            "not-run"
        };
        let layout = self.layout();
        json!({
            "app": PROVIDER,
            "step": self.step_id(),
            "step_number": step_index(self.step).saturating_add(1),
            "steps": STEP_LABELS,
            "status": self.status,
            "keyboard": {
                "layout": self.keymap().trim(),
                "xkb_layout": layout.xkb_layout,
                "xkb_variant": layout.variant_or_empty(),
            },
            "network": {
                "summary": super::network_summary(&self.network),
                "notice": self.network.notice,
                "busy": self.network.is_busy(),
                "selected": self.network.selected_ssid,
                "networks": self.network.networks.iter().map(|n| json!({
                    "ssid": n.name, "secure": n.secure, "connected": n.connected, "signal": n.signal,
                })).collect::<Vec<_>>(),
            },
            "storage": {
                "method": if self.install_method == InstallMethod::Alongside { "alongside" } else { "erase" },
                "disks": disks,
                "selected": self.selected_dev(),
                "confirmation_target": self.destructive_confirmation_target(),
                "selected_region": self.selected_guided_extent,
                "preflight": preflight,
            },
            "account": {
                "username": local_text(&self.username).trim(),
                "full_name": local_text(&self.full_name).trim(),
                "passphrase_set": !secret_copy(&self.password).is_empty(),
                "secret_policy": if self.startup_secret_is_pin() { "tpm-pin" } else { "passphrase" },
            },
            "options": {
                "filesystem": FILESYSTEMS.get(self.selected_fs).copied().unwrap_or("btrfs"),
                "security_key": self.fido2,
                "hostname": local_text(&self.hostname).trim(),
                "locale": self.locale().trim(),
                "timezone": self.timezone().trim(),
                "encryption": "LUKS",
                "unlock_factors": self.encryption_factors(),
            },
            "media": {
                "codecs": self.codecs,
            },
            "review": {
                "recovery_promise": self.recovery_ack,
                "ready": self.encryption_ready() && self.destructive_confirmation_ready(),
            },
            "hardware": self.hardware.to_json(),
            "install": {
                "state": self.install_state_name(),
                "stages": friendly_install_status(&self.status_lines),
                "log_tail": log_tail,
            },
            "recovery_key": {
                "available": self.recovery_key.is_some(),
                "copied": self.recovery_key_copied,
                "saved": self.recovery_key_saved,
            },
            "image": {
                "composefs": console_control::booted_composefs(),
            },
            "unattended": {
                "active": self.unattended_mode,
                "error": self.unattended_error,
            },
        })
    }
}

/// The installer as its console answers.
pub struct Console<'a>(pub &'a mut InstallerApp);

impl ConsoleApp for Console<'_> {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.0
            .published()
            .iter()
            .map(|row| row.exported().clone())
            .collect()
    }

    fn run(
        &mut self,
        _address: &ActionAddress,
        encoded: EncodedAction,
    ) -> Result<Value, Box<ActionFailure>> {
        let output = self.0.apply_encoded(&encoded).map_err(|message| {
            Box::new(ActionFailure {
                code: "action_failed".to_owned(),
                message,
                details: None,
                retryable: false,
            })
        })?;
        Ok(json!({ "output": output.value, "step": self.0.step_id() }))
    }

    fn state(&self) -> Value {
        self.0.console_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_material_is_redacted_from_reported_output() {
        let key = "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk";
        assert_eq!(
            redact_recovery_material(&format!("key: {key}.")).as_deref(),
            Some("key: [redacted]")
        );
        assert_eq!(
            redact_recovery_material(&format!("{RECOVERY_KEY_PREFIX}{key}")),
            None
        );
        assert_eq!(
            redact_recovery_material("==> Installing Tilo onto /dev/vdb").as_deref(),
            Some("==> Installing Tilo onto /dev/vdb")
        );
        let uuid = "00000000-0000-4000-8000-000000000003";
        assert_eq!(redact_recovery_material(uuid).as_deref(), Some(uuid));
    }
}
