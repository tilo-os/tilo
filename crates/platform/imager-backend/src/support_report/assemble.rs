//! Turn raw platform observations into the four support outcomes.
//!
//! A running system shows presence, bound drivers and firmware versions. It
//! does not show that the selected Tilo release carries the same kernel,
//! firmware or userspace service for that device, so an observation alone can
//! never make a component `Supported`. `Supported` is reserved for evidence
//! checked against the selected release; until such evidence exists the honest
//! outcome is `Unverified` (or `Limited` for a limitation the observation did
//! establish). An absent essential component is `Unsupported`; an absent
//! optional one is not.

use super::{
    Applicability, ComponentKind, ComponentSupport, DestinationIdentity, DeviceBinding, Evidence,
    EvidenceFact, EvidenceScope, REPORT_SCHEMA, RawBattery, RawDevice, RawDisk, RawDisplay,
    RawGraphics, RawNetwork, RawReport, RawSoundCard, Remedy, ReportSource, SupportReason,
    SupportReport, SupportState,
};

pub(super) fn assemble(raw: RawReport) -> SupportReport {
    let identity = DestinationIdentity {
        source: ReportSource::LocalScan,
        manufacturer: raw.identity.manufacturer,
        model: raw.identity.model,
        variant: raw.identity.variant,
        board: raw.identity.board,
        sku: raw.identity.sku,
        firmware: raw.identity.firmware,
    };
    let components = vec![
        graphics(&raw.graphics),
        display(&raw.displays),
        network(ComponentKind::Wifi, &raw.wifi),
        network(ComponentKind::Ethernet, &raw.ethernet),
        bluetooth(&raw.bluetooth),
        audio(&raw.sound_cards),
        microphone(&raw.sound_cards),
        camera(&raw.cameras),
        fingerprint(&raw.fingerprint),
        power(raw.battery.as_ref(), raw.ac_online, &raw.suspend_states),
        acpi(raw.acpi_tables, raw.kernel.as_deref()),
        boot(raw.uefi, raw.secure_boot, raw.tpm.as_deref()),
        storage(&raw.disks),
        input(&raw.inputs),
    ];
    SupportReport {
        schema: REPORT_SCHEMA.into(),
        identity,
        kernel: raw.kernel,
        architecture: raw.architecture,
        components,
    }
}

fn observed(
    kind: ComponentKind,
    name: impl Into<String>,
    present: bool,
    state: SupportState,
    reason: SupportReason,
    evidence: Vec<Evidence>,
) -> ComponentSupport {
    ComponentSupport {
        kind,
        name: name.into(),
        present,
        state,
        applicability: Applicability::ObservationOnly,
        reason,
        evidence,
        remedy: None,
    }
}

fn with_remedy(mut component: ComponentSupport, remedy: Remedy) -> ComponentSupport {
    component.remedy = Some(remedy);
    component
}

fn absent(kind: ComponentKind, remedy: Remedy) -> ComponentSupport {
    let state = if kind.critical() {
        SupportState::Unsupported
    } else {
        SupportState::Unverified
    };
    with_remedy(
        observed(
            kind,
            "",
            false,
            state,
            SupportReason::NotDetected,
            Vec::new(),
        ),
        remedy,
    )
}

fn binding(adapter: &RawGraphics) -> DeviceBinding {
    DeviceBinding {
        device: adapter.name.clone(),
        driver: adapter.driver.clone(),
    }
}

fn graphics_evidence(graphics: &[RawGraphics]) -> Vec<Evidence> {
    graphics
        .iter()
        .map(|adapter| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Device {
                name: adapter.name.clone(),
                driver: adapter.driver.clone(),
                firmware: None,
                vendor: adapter.vendor.clone(),
                product: adapter.device.clone(),
                reported_problem: false,
            },
        })
        .collect()
}

fn graphics_reason(active: &[&RawGraphics], primary: &RawGraphics) -> SupportReason {
    let additional = active
        .iter()
        .filter(|adapter| !std::ptr::eq(**adapter, primary))
        .map(|adapter| binding(adapter))
        .collect::<Vec<_>>();
    if additional.is_empty() {
        SupportReason::DriverBound {
            bindings: vec![binding(primary)],
        }
    } else {
        SupportReason::HybridGraphics {
            primary: binding(primary),
            additional,
        }
    }
}

fn graphics(graphics: &[RawGraphics]) -> ComponentSupport {
    if graphics.is_empty() {
        return absent(ComponentKind::Graphics, Remedy::RunOnDestination);
    }
    let evidence = graphics_evidence(graphics);
    if graphics.iter().all(|adapter| adapter.virtual_device) {
        let device = graphics
            .first()
            .map(|adapter| adapter.name.clone())
            .unwrap_or_default();
        return with_remedy(
            observed(
                ComponentKind::Graphics,
                device.clone(),
                true,
                SupportState::Limited,
                SupportReason::VirtualAdapter { device },
                evidence,
            ),
            Remedy::RunOnDestination,
        );
    }
    let physical = graphics
        .iter()
        .filter(|adapter| !adapter.virtual_device)
        .collect::<Vec<_>>();
    let active = if physical.is_empty() {
        graphics.iter().collect::<Vec<_>>()
    } else {
        physical
    };
    let Some(primary) = active
        .iter()
        .find(|adapter| adapter.primary)
        .or_else(|| active.first())
        .copied()
    else {
        return absent(ComponentKind::Graphics, Remedy::RunOnDestination);
    };
    let name = active
        .iter()
        .map(|adapter| adapter.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let driver_missing = active
        .iter()
        .filter(|adapter| adapter.driver.is_none())
        .map(|adapter| adapter.name.clone())
        .collect::<Vec<_>>();
    if !driver_missing.is_empty() {
        return with_remedy(
            observed(
                ComponentKind::Graphics,
                name,
                true,
                SupportState::Limited,
                SupportReason::DriverMissing {
                    devices: driver_missing,
                },
                evidence,
            ),
            Remedy::CheckDriver,
        );
    }
    let open_nvidia = active
        .iter()
        .find(|adapter| adapter.driver.as_deref() == Some("nouveau"))
        .map(|adapter| adapter.name.clone());
    if let Some(device) = open_nvidia {
        return with_remedy(
            observed(
                ComponentKind::Graphics,
                name,
                true,
                SupportState::Limited,
                SupportReason::OpenNvidia { device },
                evidence,
            ),
            Remedy::CheckDriver,
        );
    }
    observed(
        ComponentKind::Graphics,
        name,
        true,
        SupportState::Unverified,
        graphics_reason(&active, primary),
        evidence,
    )
}

fn display(displays: &[RawDisplay]) -> ComponentSupport {
    if displays.is_empty() {
        return absent(ComponentKind::Display, Remedy::Connect);
    }
    let connected = displays
        .iter()
        .filter(|display| display.connected)
        .collect::<Vec<_>>();
    let evidence = displays
        .iter()
        .map(|display| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Display {
                name: display.name.clone(),
                connected: display.connected,
                active: display.connected && display.enabled,
            },
        })
        .collect::<Vec<_>>();
    let name = connected
        .iter()
        .map(|display| display.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    if connected.is_empty() {
        return with_remedy(
            observed(
                ComponentKind::Display,
                name,
                true,
                SupportState::Unverified,
                SupportReason::DisplayDisconnected,
                evidence,
            ),
            Remedy::Connect,
        );
    }
    let active = connected
        .iter()
        .filter(|display| display.enabled)
        .copied()
        .collect::<Vec<_>>();
    if active.is_empty() {
        let device = connected
            .first()
            .map(|display| display.name.clone())
            .unwrap_or_default();
        return with_remedy(
            observed(
                ComponentKind::Display,
                name,
                true,
                SupportState::Limited,
                SupportReason::DisplayInactive { device },
                evidence,
            ),
            Remedy::DisplaySettings,
        );
    }
    let internal = active
        .iter()
        .filter(|display| display.internal == Some(true))
        .count();
    let external = active
        .iter()
        .filter(|display| display.internal == Some(false))
        .count();
    observed(
        ComponentKind::Display,
        active
            .iter()
            .map(|display| display.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
        true,
        SupportState::Unverified,
        SupportReason::DisplaysActive {
            internal,
            external,
            devices: active.iter().map(|display| display.name.clone()).collect(),
        },
        evidence,
    )
}

fn network(kind: ComponentKind, adapters: &[RawNetwork]) -> ComponentSupport {
    if adapters.is_empty() {
        return absent(kind, Remedy::Connect);
    }
    let evidence = adapters
        .iter()
        .map(|adapter| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Device {
                name: adapter.name.clone(),
                driver: adapter.driver.clone(),
                firmware: adapter.firmware.clone(),
                vendor: None,
                product: None,
                reported_problem: false,
            },
        })
        .collect::<Vec<_>>();
    let name = adapters
        .iter()
        .map(|adapter| adapter.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    if adapters.iter().any(|adapter| adapter.driver.is_none()) {
        return with_remedy(
            observed(
                kind,
                name,
                true,
                SupportState::Limited,
                SupportReason::DriverMissing {
                    devices: adapters
                        .iter()
                        .filter(|adapter| adapter.driver.is_none())
                        .map(|adapter| adapter.name.clone())
                        .collect(),
                },
                evidence,
            ),
            Remedy::CheckDriver,
        );
    }
    observed(
        kind,
        name,
        true,
        SupportState::Unverified,
        SupportReason::DriverBound {
            bindings: adapters
                .iter()
                .map(|adapter| DeviceBinding {
                    device: adapter.name.clone(),
                    driver: adapter.driver.clone(),
                })
                .collect(),
        },
        evidence,
    )
}

fn device_evidence(devices: &[RawDevice]) -> Vec<Evidence> {
    devices
        .iter()
        .map(|device| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Device {
                name: device.name.clone(),
                driver: device.driver.clone(),
                firmware: None,
                vendor: device.vendor.clone(),
                product: device.product.clone(),
                reported_problem: device.healthy == Some(false),
            },
        })
        .collect()
}

fn binding_of_device(device: &RawDevice) -> DeviceBinding {
    DeviceBinding {
        device: device.name.clone(),
        driver: device.driver.clone(),
    }
}

fn device_names(devices: &[RawDevice]) -> String {
    devices
        .iter()
        .map(|device| device.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn driver_missing(
    kind: ComponentKind,
    names: String,
    devices: Vec<String>,
    evidence: Vec<Evidence>,
) -> ComponentSupport {
    with_remedy(
        observed(
            kind,
            names,
            true,
            SupportState::Limited,
            SupportReason::DriverMissing { devices },
            evidence,
        ),
        Remedy::CheckDriver,
    )
}

fn bluetooth(adapters: &[RawDevice]) -> ComponentSupport {
    if adapters.is_empty() {
        return absent(ComponentKind::Bluetooth, Remedy::Connect);
    }
    let evidence = device_evidence(adapters);
    let names = device_names(adapters);
    if adapters
        .iter()
        .all(|adapter| adapter.driver.is_some() || adapter.healthy == Some(true))
    {
        return observed(
            ComponentKind::Bluetooth,
            names,
            true,
            SupportState::Unverified,
            SupportReason::DriverBound {
                bindings: adapters
                    .iter()
                    .map(|adapter| DeviceBinding {
                        device: adapter.name.clone(),
                        driver: adapter.driver.clone(),
                    })
                    .collect(),
            },
            evidence,
        );
    }
    driver_missing(
        ComponentKind::Bluetooth,
        names,
        adapters
            .iter()
            .filter(|adapter| adapter.driver.is_none() && adapter.healthy != Some(true))
            .map(|adapter| adapter.name.clone())
            .collect(),
        evidence,
    )
}

fn sound_evidence(cards: &[RawSoundCard]) -> Vec<Evidence> {
    cards
        .iter()
        .map(|card| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Device {
                name: card.name.clone(),
                driver: card.driver.clone(),
                firmware: None,
                vendor: None,
                product: None,
                reported_problem: false,
            },
        })
        .collect()
}

fn sound_names(cards: &[RawSoundCard]) -> String {
    cards
        .iter()
        .map(|card| card.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn audio(cards: &[RawSoundCard]) -> ComponentSupport {
    if cards.is_empty() {
        return absent(ComponentKind::Audio, Remedy::Connect);
    }
    let names = sound_names(cards);
    let evidence = sound_evidence(cards);
    if cards.iter().any(|card| card.playback > 0) {
        return observed(
            ComponentKind::Audio,
            names,
            true,
            SupportState::Unverified,
            SupportReason::DriverBound {
                bindings: cards
                    .iter()
                    .filter(|card| card.playback > 0)
                    .map(|card| DeviceBinding {
                        device: card.name.clone(),
                        driver: card.driver.clone(),
                    })
                    .collect(),
            },
            evidence,
        );
    }
    with_remedy(
        observed(
            ComponentKind::Audio,
            names,
            true,
            SupportState::Limited,
            SupportReason::NoPlayback {
                device: sound_names(cards),
            },
            evidence,
        ),
        Remedy::Connect,
    )
}

fn microphone(cards: &[RawSoundCard]) -> ComponentSupport {
    if cards.is_empty() {
        return absent(ComponentKind::Microphone, Remedy::Connect);
    }
    let names = sound_names(cards);
    let evidence = sound_evidence(cards);
    if cards.iter().any(|card| card.capture > 0) {
        return observed(
            ComponentKind::Microphone,
            names,
            true,
            SupportState::Unverified,
            SupportReason::DriverBound {
                bindings: cards
                    .iter()
                    .filter(|card| card.capture > 0)
                    .map(|card| DeviceBinding {
                        device: card.name.clone(),
                        driver: card.driver.clone(),
                    })
                    .collect(),
            },
            evidence,
        );
    }
    with_remedy(
        observed(
            ComponentKind::Microphone,
            names,
            true,
            SupportState::Limited,
            SupportReason::NoCapture {
                device: sound_names(cards),
            },
            evidence,
        ),
        Remedy::Connect,
    )
}

fn camera(cameras: &[RawDevice]) -> ComponentSupport {
    if cameras.is_empty() {
        return absent(ComponentKind::Camera, Remedy::ExternalCamera);
    }
    let evidence = device_evidence(cameras);
    let names = device_names(cameras);
    let platform_pipeline = cameras.iter().find(|camera| {
        camera
            .driver
            .as_deref()
            .is_some_and(|driver| driver.contains("ipu") || driver.contains("atomisp"))
    });
    if let Some(camera) = platform_pipeline {
        return with_remedy(
            observed(
                ComponentKind::Camera,
                names,
                true,
                SupportState::Limited,
                SupportReason::PlatformPipeline {
                    device: camera.name.clone(),
                },
                evidence,
            ),
            Remedy::ExternalCamera,
        );
    }
    if cameras
        .iter()
        .any(|camera| camera.driver.as_deref() == Some("uvcvideo") || camera.healthy == Some(true))
    {
        return observed(
            ComponentKind::Camera,
            names,
            true,
            SupportState::Unverified,
            SupportReason::DriverBound {
                bindings: cameras
                    .iter()
                    .filter(|camera| camera.driver.is_some() || camera.healthy == Some(true))
                    .map(|camera| DeviceBinding {
                        device: camera.name.clone(),
                        driver: camera.driver.clone(),
                    })
                    .collect(),
            },
            evidence,
        );
    }
    let missing = cameras
        .iter()
        .filter(|camera| camera.driver.is_none())
        .map(|camera| camera.name.clone())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return observed(
            ComponentKind::Camera,
            names,
            true,
            SupportState::Unverified,
            SupportReason::DriverBound {
                bindings: cameras.iter().map(binding_of_device).collect(),
            },
            evidence,
        );
    }
    driver_missing(ComponentKind::Camera, names, missing, evidence)
}

fn fingerprint(readers: &[RawDevice]) -> ComponentSupport {
    if readers.is_empty() {
        return absent(ComponentKind::Fingerprint, Remedy::PasswordSignIn);
    }
    with_remedy(
        observed(
            ComponentKind::Fingerprint,
            device_names(readers),
            true,
            SupportState::Limited,
            SupportReason::FingerprintUnverified {
                device: device_names(readers),
            },
            device_evidence(readers),
        ),
        Remedy::PasswordSignIn,
    )
}

fn power(
    battery: Option<&RawBattery>,
    ac_online: Option<bool>,
    suspend_states: &[String],
) -> ComponentSupport {
    let evidence = if suspend_states.is_empty() {
        Vec::new()
    } else {
        vec![Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Suspend {
                states: suspend_states.to_vec(),
            },
        }]
    };
    let Some(battery) = battery else {
        return if ac_online == Some(true) {
            observed(
                ComponentKind::Power,
                "",
                true,
                SupportState::Unverified,
                SupportReason::AcOnly,
                evidence,
            )
        } else {
            with_remedy(
                absent(ComponentKind::Power, Remedy::PowerAdapter),
                Remedy::PowerAdapter,
            )
        };
    };
    let reason = SupportReason::Battery {
        device: battery.name.clone(),
        capacity: battery.capacity.map(|capacity| capacity.min(100)),
        status: battery.status.clone(),
        health: battery.health.map(|health| health.min(100)),
    };
    if suspend_states.is_empty() {
        return with_remedy(
            observed(
                ComponentKind::Power,
                battery.name.clone(),
                true,
                SupportState::Limited,
                reason,
                evidence,
            ),
            Remedy::PowerSettings,
        );
    }
    observed(
        ComponentKind::Power,
        battery.name.clone(),
        true,
        SupportState::Unverified,
        reason,
        evidence,
    )
}

fn acpi(tables: Option<usize>, kernel: Option<&str>) -> ComponentSupport {
    let Some(tables) = tables.filter(|tables| *tables > 0) else {
        return with_remedy(
            observed(
                ComponentKind::Acpi,
                "",
                false,
                SupportState::Unverified,
                SupportReason::NotDetected,
                Vec::new(),
            ),
            Remedy::PowerSettings,
        );
    };
    let mut evidence = vec![Evidence {
        scope: EvidenceScope::Observation,
        fact: EvidenceFact::Acpi { tables },
    }];
    if let Some(kernel) = kernel {
        evidence.push(Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Kernel {
                version: kernel.to_string(),
            },
        });
    }
    observed(
        ComponentKind::Acpi,
        "",
        true,
        SupportState::Unverified,
        SupportReason::AcpiTables { tables },
        evidence,
    )
}

fn boot(uefi: Option<bool>, secure_boot: Option<bool>, tpm: Option<&str>) -> ComponentSupport {
    match uefi {
        Some(true) => observed(
            ComponentKind::Boot,
            "",
            true,
            SupportState::Unverified,
            SupportReason::Uefi {
                secure_boot,
                tpm: tpm.map(str::to_string),
            },
            Vec::new(),
        ),
        Some(false) => with_remedy(
            observed(
                ComponentKind::Boot,
                "",
                true,
                SupportState::Unsupported,
                SupportReason::LegacyBoot,
                Vec::new(),
            ),
            Remedy::UefiBoot,
        ),
        None => with_remedy(
            observed(
                ComponentKind::Boot,
                "",
                false,
                SupportState::Unverified,
                SupportReason::StartupUnknown,
                Vec::new(),
            ),
            Remedy::UefiBoot,
        ),
    }
}

fn storage(disks: &[RawDisk]) -> ComponentSupport {
    if disks.is_empty() {
        return absent(ComponentKind::Storage, Remedy::RunOnDestination);
    }
    let evidence = disks
        .iter()
        .map(|disk| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Device {
                name: disk.model.clone().unwrap_or_else(|| disk.name.clone()),
                driver: disk.driver.clone(),
                firmware: None,
                vendor: None,
                product: disk
                    .size_bytes
                    .map(|bytes| format!("{} GiB", bytes / (1 << 30))),
                reported_problem: false,
            },
        })
        .collect::<Vec<_>>();
    let names = disks
        .iter()
        .map(|disk| disk.model.clone().unwrap_or_else(|| disk.name.clone()))
        .collect::<Vec<_>>()
        .join(", ");
    if disks.iter().all(|disk| disk.driver.is_none()) {
        return driver_missing(
            ComponentKind::Storage,
            names,
            disks.iter().map(|disk| disk.name.clone()).collect(),
            evidence,
        );
    }
    observed(
        ComponentKind::Storage,
        names,
        true,
        SupportState::Unverified,
        SupportReason::DriverBound {
            bindings: disks
                .iter()
                .map(|disk| DeviceBinding {
                    device: disk.model.clone().unwrap_or_else(|| disk.name.clone()),
                    driver: disk.driver.clone(),
                })
                .collect(),
        },
        evidence,
    )
}

fn input(inputs: &[String]) -> ComponentSupport {
    if inputs.is_empty() {
        return absent(ComponentKind::Input, Remedy::ExternalKeyboard);
    }
    let keyboard = inputs
        .iter()
        .any(|name| name.to_ascii_lowercase().contains("keyboard"));
    let evidence = inputs
        .iter()
        .take(8)
        .map(|name| Evidence {
            scope: EvidenceScope::Observation,
            fact: EvidenceFact::Device {
                name: name.clone(),
                driver: None,
                firmware: None,
                vendor: None,
                product: None,
                reported_problem: false,
            },
        })
        .collect::<Vec<_>>();
    if keyboard {
        return observed(
            ComponentKind::Input,
            "",
            true,
            SupportState::Unverified,
            SupportReason::InputDevices {
                count: inputs.len(),
            },
            evidence,
        );
    }
    with_remedy(
        observed(
            ComponentKind::Input,
            "",
            true,
            SupportState::Limited,
            SupportReason::NoKeyboard,
            evidence,
        ),
        Remedy::ExternalKeyboard,
    )
}
