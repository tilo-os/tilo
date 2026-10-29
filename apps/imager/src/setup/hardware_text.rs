//! The words of the hardware compatibility report, shared by both sides of
//! the setup: the host's report of this or another computer and the live
//! installer's report of the computer it runs on.
//!
//! The backend reports typed facts and outcomes. This module owns every
//! sentence: component names, reasons, evidence and next steps are Fluent
//! messages, while hardware names, driver names and versions are the values
//! those messages carry.

use crate::setup::strings::{t, t_args};
use imager_backend::support_report::{
    Applicability, ComponentKind, ComponentSupport, DeviceBinding, Evidence, EvidenceFact,
    EvidenceScope, Remedy, SupportReason, SupportState,
};
use ui::icons::Icon;

pub(crate) fn component_title(kind: ComponentKind) -> String {
    t(match kind {
        ComponentKind::Graphics => "installer-hardware-component-graphics",
        ComponentKind::Display => "installer-hardware-component-display",
        ComponentKind::Wifi => "installer-hardware-component-wifi",
        ComponentKind::Ethernet => "installer-hardware-component-ethernet",
        ComponentKind::Bluetooth => "installer-hardware-component-bluetooth",
        ComponentKind::Audio => "installer-hardware-component-audio",
        ComponentKind::Microphone => "installer-hardware-component-microphone",
        ComponentKind::Camera => "installer-hardware-component-camera",
        ComponentKind::Fingerprint => "installer-hardware-component-fingerprint",
        ComponentKind::Power => "installer-hardware-component-power",
        ComponentKind::Acpi => "installer-hardware-component-acpi",
        ComponentKind::Boot => "installer-hardware-component-boot",
        ComponentKind::Storage => "installer-hardware-component-storage",
        ComponentKind::Input => "installer-hardware-component-input",
    })
}

pub(crate) fn state_label(state: SupportState) -> String {
    t(match state {
        SupportState::Supported => "installer-hardware-state-supported",
        SupportState::Limited => "installer-hardware-state-limited",
        SupportState::Unsupported => "installer-hardware-state-unsupported",
        SupportState::Unverified => "installer-hardware-state-unverified",
    })
}

pub(crate) fn reason_text(component: &ComponentSupport) -> String {
    let reason = &component.reason;
    match reason {
        SupportReason::DriverBound { bindings } => {
            if bindings.len() == 1 {
                binding_text("installer-hardware-reason-driver-bound-one", &bindings[0])
            } else {
                t_args(
                    "installer-hardware-reason-driver-bound-many",
                    &[("count", bindings.len().into())],
                )
            }
        }
        SupportReason::HybridGraphics { primary, .. } => {
            binding_text("installer-hardware-reason-hybrid-graphics", primary)
        }
        SupportReason::DriverMissing { devices } => t_args(
            "installer-hardware-reason-driver-missing",
            &[("count", devices.len().into())],
        ),
        SupportReason::VirtualAdapter { device } => t_args(
            "installer-hardware-reason-virtual-adapter",
            &[("device", device.as_str().into())],
        ),
        SupportReason::OpenNvidia { device } => t_args(
            "installer-hardware-reason-open-nvidia",
            &[("device", device.as_str().into())],
        ),
        SupportReason::PlatformPipeline { device } => t_args(
            "installer-hardware-reason-platform-pipeline",
            &[("device", device.as_str().into())],
        ),
        SupportReason::NotDetected => t("installer-hardware-reason-not-detected"),
        SupportReason::DisplayDisconnected => t("installer-hardware-reason-display-disconnected"),
        SupportReason::DisplayInactive { device } => t_args(
            "installer-hardware-reason-display-inactive",
            &[("device", device.as_str().into())],
        ),
        SupportReason::DisplaysActive {
            internal, external, ..
        } => t_args(
            "installer-hardware-reason-displays-active",
            &[
                ("internal", (*internal).into()),
                ("external", (*external).into()),
            ],
        ),
        SupportReason::NoPlayback { device } => t_args(
            "installer-hardware-reason-no-playback",
            &[("device", device.as_str().into())],
        ),
        SupportReason::NoCapture { device } => t_args(
            "installer-hardware-reason-no-capture",
            &[("device", device.as_str().into())],
        ),
        SupportReason::FingerprintUnverified { device } => t_args(
            "installer-hardware-reason-fingerprint-unverified",
            &[("device", device.as_str().into())],
        ),
        SupportReason::SuspendUnavailable => t("installer-hardware-reason-suspend-unavailable"),
        SupportReason::Battery {
            device,
            capacity,
            status,
            health,
        } => {
            let mut text = t_args(
                "installer-hardware-reason-battery",
                &[("device", device.as_str().into())],
            );
            if let Some(capacity) = capacity {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-battery-capacity",
                    &[("capacity", (*capacity).into())],
                ));
            }
            if let Some(status) = status {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-battery-status",
                    &[("status", status.as_str().into())],
                ));
            }
            if let Some(health) = health {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-battery-health",
                    &[("health", (*health).into())],
                ));
            }
            text
        }
        SupportReason::AcOnly => t("installer-hardware-reason-ac-only"),
        SupportReason::AcpiTables { tables } => t_args(
            "installer-hardware-reason-acpi-tables",
            &[("tables", (*tables).into())],
        ),
        SupportReason::Uefi { secure_boot, tpm } => {
            let mut text = t("installer-hardware-reason-uefi");
            if let Some(secure_boot) = secure_boot {
                text.push(' ');
                text.push_str(&t(if *secure_boot {
                    "installer-hardware-reason-secure-boot-on"
                } else {
                    "installer-hardware-reason-secure-boot-off"
                }));
            }
            if let Some(tpm) = tpm {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-tpm",
                    &[("tpm", tpm.as_str().into())],
                ));
            }
            text
        }
        SupportReason::LegacyBoot => t("installer-hardware-reason-legacy-boot"),
        SupportReason::StartupUnknown => t("installer-hardware-reason-startup-unknown"),
        SupportReason::InputDevices { count } => t_args(
            "installer-hardware-reason-input-devices",
            &[("count", (*count).into())],
        ),
        SupportReason::NoKeyboard => t("installer-hardware-reason-no-keyboard"),
        SupportReason::StartsHere {
            architecture,
            board,
            devicetree,
            secure_boot,
            tpm,
        } => {
            let mut text = t_args(
                "installer-hardware-reason-starts-here",
                &[("architecture", architecture.as_str().into())],
            );
            if let Some(board) = board {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-starts-board",
                    &[("board", board.as_str().into())],
                ));
            }
            if let Some(devicetree) = devicetree {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-starts-devicetree",
                    &[("devicetree", devicetree.as_str().into())],
                ));
            }
            if let Some(secure_boot) = secure_boot {
                text.push(' ');
                text.push_str(&t(if *secure_boot {
                    "installer-hardware-reason-secure-boot-on"
                } else {
                    "installer-hardware-reason-secure-boot-off"
                }));
            }
            if let Some(tpm) = tpm {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-tpm",
                    &[("tpm", tpm.as_str().into())],
                ));
            }
            text
        }
        SupportReason::OtherArchitecture {
            architecture,
            running,
        } => t_args(
            "installer-hardware-reason-other-architecture",
            &[
                ("architecture", architecture.as_str().into()),
                ("running", running.as_str().into()),
            ],
        ),
        SupportReason::BoardWithoutDevicetree {
            board,
            architecture,
        } => t_args(
            "installer-hardware-reason-board-without-devicetree",
            &[
                ("board", board.as_str().into()),
                ("architecture", architecture.as_str().into()),
            ],
        ),
    }
}

fn binding_text(key: &str, binding: &DeviceBinding) -> String {
    match &binding.driver {
        Some(driver) => t_args(
            key,
            &[
                ("device", binding.device.as_str().into()),
                ("driver", driver.as_str().into()),
            ],
        ),
        None => t_args(
            &format!("{key}-no-driver"),
            &[("device", binding.device.as_str().into())],
        ),
    }
}

pub(crate) fn evidence_text(evidence: &Evidence) -> String {
    let text = match &evidence.fact {
        EvidenceFact::Device {
            name,
            driver,
            firmware,
            vendor,
            product,
            reported_problem,
        } => {
            let mut parts = vec![name.clone()];
            if let Some(driver) = driver {
                parts.push(t_args(
                    "installer-hardware-evidence-driver",
                    &[("driver", driver.as_str().into())],
                ));
            }
            if let Some(firmware) = firmware {
                parts.push(t_args(
                    "installer-hardware-evidence-firmware-version",
                    &[("firmware", firmware.as_str().into())],
                ));
            }
            if let Some(vendor) = vendor {
                parts.push(t_args(
                    "installer-hardware-evidence-vendor",
                    &[("vendor", vendor.as_str().into())],
                ));
            }
            if let Some(product) = product {
                parts.push(t_args(
                    "installer-hardware-evidence-product",
                    &[("product", product.as_str().into())],
                ));
            }
            if *reported_problem {
                parts.push(t("installer-hardware-evidence-problem"));
            }
            parts.join(", ")
        }
        EvidenceFact::Display {
            name,
            connected,
            active,
        } => {
            let state = if *active {
                t("installer-hardware-evidence-display-active")
            } else if *connected {
                t("installer-hardware-evidence-display-connected")
            } else {
                t("installer-hardware-evidence-display-disconnected")
            };
            t_args(
                "installer-hardware-evidence-display",
                &[("name", name.as_str().into()), ("state", state.into())],
            )
        }
        EvidenceFact::Kernel { version } => t_args(
            "installer-hardware-evidence-kernel",
            &[("version", version.as_str().into())],
        ),
        EvidenceFact::Acpi { tables } => t_args(
            "installer-hardware-evidence-acpi",
            &[("tables", (*tables).into())],
        ),
        EvidenceFact::Suspend { states } => t_args(
            "installer-hardware-evidence-suspend",
            &[("states", states.join(", ").into())],
        ),
        EvidenceFact::Architecture { name } => t_args(
            "installer-hardware-evidence-architecture",
            &[("architecture", name.as_str().into())],
        ),
        EvidenceFact::Virtualization { names } => t_args(
            "installer-hardware-evidence-virtualization",
            &[("names", names.join(", ").into())],
        ),
        EvidenceFact::Board { id, devicetree } => {
            let mut text = t_args(
                "installer-hardware-evidence-board",
                &[("board", id.as_str().into())],
            );
            if let Some(devicetree) = devicetree {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-evidence-devicetree",
                    &[("devicetree", devicetree.as_str().into())],
                ));
            }
            text
        }
    };
    if evidence.scope == EvidenceScope::SelectedRelease {
        t_args(
            "installer-hardware-evidence-release",
            &[("fact", text.into())],
        )
    } else {
        text
    }
}

pub(crate) fn remedy_text(kind: ComponentKind, remedy: Remedy) -> String {
    t(match remedy {
        Remedy::RunOnDestination => "installer-hardware-remedy-run-on-destination",
        Remedy::Connect => match kind {
            ComponentKind::Display => "installer-hardware-remedy-connect-display",
            ComponentKind::Audio | ComponentKind::Microphone => {
                "installer-hardware-remedy-connect-audio"
            }
            _ => "installer-hardware-remedy-connect",
        },
        Remedy::CheckDriver => "installer-hardware-remedy-check-driver",
        Remedy::PasswordSignIn => "installer-hardware-remedy-password",
        Remedy::ExternalCamera => "installer-hardware-remedy-external-camera",
        Remedy::ExternalKeyboard => "installer-hardware-remedy-external-keyboard",
        Remedy::PowerSettings => "installer-hardware-remedy-power-settings",
        Remedy::DisplaySettings => "installer-hardware-remedy-display-settings",
        Remedy::UefiBoot => "installer-hardware-remedy-uefi",
        Remedy::PowerAdapter => "installer-hardware-remedy-power-adapter",
    })
}


pub(crate) fn component_icon(kind: ComponentKind) -> Icon {
    match kind {
        ComponentKind::Graphics | ComponentKind::Display => Icon::Monitor,
        ComponentKind::Wifi => Icon::Wifi,
        ComponentKind::Ethernet => Icon::EthernetPort,
        ComponentKind::Bluetooth => Icon::Bluetooth,
        ComponentKind::Audio => Icon::Volume2,
        ComponentKind::Microphone => Icon::Mic,
        ComponentKind::Camera => Icon::Camera,
        ComponentKind::Fingerprint => Icon::ScanLine,
        ComponentKind::Power => Icon::Battery,
        ComponentKind::Acpi => Icon::Power,
        ComponentKind::Boot => Icon::Shield,
        ComponentKind::Storage => Icon::HardDrive,
        ComponentKind::Input => Icon::Keyboard,
    }
}
