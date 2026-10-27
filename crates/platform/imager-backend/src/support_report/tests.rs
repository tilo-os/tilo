use super::assemble::assemble;
use super::*;

fn write(root: &std::path::Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[cfg(unix)]
fn link(root: &std::path::Path, target: &str, path: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target = root.join(target);
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(target, path).unwrap();
}

#[cfg(target_os = "linux")]
fn hybrid_laptop() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, value) in [
        ("sys_vendor", "Test Vendor"),
        ("product_name", "Test Laptop 15"),
        ("product_version", "1.0"),
        ("product_sku", "SKU-1"),
        ("board_vendor", "Test Vendor"),
        ("board_name", "TESTBOARD"),
        ("board_version", "1.0"),
        ("bios_vendor", "Test Firmware"),
        ("bios_version", "1.2.3"),
        ("bios_date", "01/01/2026"),
    ] {
        write(root.path(), &format!("sys/class/dmi/id/{name}"), value);
    }
    write(root.path(), "proc/sys/kernel/osrelease", "7.2.7-test\n");

    write(root.path(), "sys/class/drm/card1/device/vendor", "0x1002\n");
    write(root.path(), "sys/class/drm/card1/device/device", "0x15bf\n");
    write(root.path(), "sys/class/drm/card1/device/boot_vga", "1\n");
    link(
        root.path(),
        "sys/bus/pci/drivers/amdgpu",
        "sys/class/drm/card1/device/driver",
    );
    write(root.path(), "sys/class/drm/card0/device/vendor", "0x10de\n");
    write(root.path(), "sys/class/drm/card0/device/device", "0x2860\n");
    write(root.path(), "sys/class/drm/card0/device/boot_vga", "0\n");
    link(
        root.path(),
        "sys/bus/pci/drivers/nvidia",
        "sys/class/drm/card0/device/driver",
    );
    write(
        root.path(),
        "sys/class/drm/card1-eDP-1/status",
        "connected\n",
    );
    write(
        root.path(),
        "sys/class/drm/card1-eDP-1/enabled",
        "enabled\n",
    );
    write(
        root.path(),
        "sys/class/drm/card0-DP-1/status",
        "connected\n",
    );
    write(root.path(), "sys/class/drm/card0-DP-1/enabled", "enabled\n");
    write(
        root.path(),
        "sys/class/drm/card0-HDMI-A-1/status",
        "disconnected\n",
    );

    write(root.path(), "sys/class/net/wlp4s0/wireless/uevent", "");
    link(
        root.path(),
        "sys/bus/pci/drivers/iwlwifi",
        "sys/class/net/wlp4s0/device/driver",
    );
    write(
        root.path(),
        "sys/class/net/wlp4s0/device/firmware/version",
        "89.test\n",
    );
    link(
        root.path(),
        "sys/bus/pci/drivers/r8169",
        "sys/class/net/enp3s0/device/driver",
    );
    write(root.path(), "sys/class/net/lo/uevent", "DEVTYPE=loopback\n");

    link(
        root.path(),
        "sys/bus/usb/drivers/btusb",
        "sys/class/bluetooth/hci0/device/driver",
    );

    write(root.path(), "sys/class/sound/card0/id", "Generic\n");
    link(
        root.path(),
        "sys/bus/pci/drivers/snd_hda_intel",
        "sys/class/sound/card0/device/driver",
    );
    std::fs::create_dir_all(root.path().join("sys/class/sound/pcmC0D0p")).unwrap();
    std::fs::create_dir_all(root.path().join("sys/class/sound/pcmC0D0c")).unwrap();

    write(
        root.path(),
        "sys/class/video4linux/video0/name",
        "Integrated Camera\n",
    );
    link(
        root.path(),
        "sys/bus/usb/drivers/uvcvideo",
        "sys/class/video4linux/video0/device/driver",
    );
    write(
        root.path(),
        "sys/class/video4linux/video1/name",
        "Integrated Camera\n",
    );

    write(
        root.path(),
        "sys/bus/usb/devices/1-2/product",
        "Fingerprint Sensor\n",
    );
    write(root.path(), "sys/bus/usb/devices/1-2/idVendor", "1234\n");
    write(root.path(), "sys/bus/usb/devices/1-2/idProduct", "5678\n");

    write(root.path(), "sys/class/power_supply/BAT1/type", "Battery\n");
    write(root.path(), "sys/class/power_supply/BAT1/capacity", "80\n");
    write(
        root.path(),
        "sys/class/power_supply/BAT1/status",
        "Charging\n",
    );
    write(
        root.path(),
        "sys/class/power_supply/BAT1/model_name",
        "Test battery\n",
    );
    write(
        root.path(),
        "sys/class/power_supply/BAT1/energy_full",
        "90000000\n",
    );
    write(
        root.path(),
        "sys/class/power_supply/BAT1/energy_full_design",
        "100000000\n",
    );
    write(root.path(), "sys/class/power_supply/ACAD/type", "Mains\n");
    write(root.path(), "sys/class/power_supply/ACAD/online", "1\n");
    write(root.path(), "sys/power/state", "freeze mem disk\n");

    std::fs::create_dir_all(root.path().join("sys/firmware/efi")).unwrap();
    write(
        root.path(),
        "sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c",
        "\0\0\0\0\0",
    );
    write(root.path(), "sys/firmware/acpi/tables/DSDT", "fixture");
    write(root.path(), "sys/class/tpm/tpm0/tpm_version_major", "2\n");

    write(
        root.path(),
        "sys/class/block/nvme0n1/device/model",
        "Test NVMe\n",
    );
    write(root.path(), "sys/class/block/nvme0n1/size", "3907029168\n");
    link(
        root.path(),
        "sys/bus/pci/drivers/nvme",
        "sys/class/block/nvme0n1/device/driver",
    );
    write(root.path(), "sys/class/block/nvme0n1p1/partition", "1\n");
    write(root.path(), "sys/class/block/zram0/size", "1024\n");

    write(
        root.path(),
        "sys/class/input/input0/name",
        "AT Translated Set 2 keyboard\n",
    );
    write(
        root.path(),
        "sys/class/input/input1/name",
        "Test touchpad\n",
    );
    root
}

#[cfg(target_os = "linux")]
#[test]
fn fixture_laptop_reports_model_hybrid_graphics_and_component_evidence() {
    let root = hybrid_laptop();
    let report = linux::probe_root(root.path());
    assert_eq!(report.identity.manufacturer.as_deref(), Some("Test Vendor"));
    assert_eq!(report.identity.model.as_deref(), Some("Test Laptop 15"));
    assert_eq!(report.identity.variant.as_deref(), Some("TESTBOARD"));
    assert!(report.identity.is_specific());
    assert_eq!(report.kernel.as_deref(), Some("7.2.7-test"));
    assert_eq!(report.architecture.as_deref(), Some(std::env::consts::ARCH));
    report.validate().unwrap();

    let graphics = component(&report, ComponentKind::Graphics);
    // A driver bound in the running system is not proof that the selected
    // Tilo release ships that driver or firmware.
    assert_eq!(graphics.state, SupportState::Unverified);
    assert_eq!(graphics.applicability, Applicability::ObservationOnly);
    assert!(matches!(
        graphics.reason,
        SupportReason::HybridGraphics { .. }
    ));
    assert!(
        graphics
            .evidence
            .iter()
            .any(|evidence| matches!(&evidence.fact, EvidenceFact::Device { driver: Some(driver), .. } if driver == "amdgpu"))
    );
    assert!(
        graphics
            .evidence
            .iter()
            .any(|evidence| matches!(&evidence.fact, EvidenceFact::Device { driver: Some(driver), .. } if driver == "nvidia"))
    );

    let display = component(&report, ComponentKind::Display);
    assert_eq!(display.state, SupportState::Unverified);
    assert!(matches!(
        display.reason,
        SupportReason::DisplaysActive {
            internal: 1,
            external: 1,
            ..
        }
    ));

    for kind in [
        ComponentKind::Wifi,
        ComponentKind::Ethernet,
        ComponentKind::Bluetooth,
        ComponentKind::Audio,
        ComponentKind::Microphone,
        ComponentKind::Camera,
        ComponentKind::Power,
        ComponentKind::Acpi,
        ComponentKind::Boot,
        ComponentKind::Storage,
        ComponentKind::Input,
    ] {
        let component = component(&report, kind);
        assert_eq!(component.state, SupportState::Unverified, "{}", kind.key());
        assert_eq!(component.applicability, Applicability::ObservationOnly);
    }
    let fingerprint = component(&report, ComponentKind::Fingerprint);
    assert_eq!(fingerprint.state, SupportState::Limited);
    assert!(fingerprint.present);
    assert_eq!(fingerprint.remedy, Some(Remedy::PasswordSignIn));
    assert!(!report.blocks_install());
    assert_eq!(report.summary().supported, 0);
    assert_eq!(report.summary().limited, 1);
}

fn component(report: &SupportReport, kind: ComponentKind) -> &ComponentSupport {
    report
        .components
        .iter()
        .find(|component| component.kind == kind)
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn report_carries_no_device_serial_or_network_address() {
    let root = hybrid_laptop();
    let report = linux::probe_root(root.path());
    let json = String::from_utf8(report.to_json().unwrap()).unwrap();
    for field in ["serial", "wwid", "mac", "MAC"] {
        assert!(!json.contains(field), "{field} must not be published");
    }
    assert!(!json.contains("TESTBOARD/"));
}

fn observed_device(name: &str, driver: &str) -> Evidence {
    Evidence {
        scope: EvidenceScope::Observation,
        fact: EvidenceFact::Device {
            name: name.into(),
            driver: Some(driver.into()),
            firmware: None,
            vendor: None,
            product: None,
            reported_problem: false,
        },
    }
}

fn report_with(components: Vec<ComponentSupport>) -> SupportReport {
    SupportReport {
        schema: REPORT_SCHEMA.into(),
        identity: DestinationIdentity {
            source: ReportSource::LocalScan,
            manufacturer: Some("Test Vendor".into()),
            model: Some("Test Laptop 15".into()),
            variant: Some("TESTBOARD".into()),
            board: None,
            sku: None,
            firmware: None,
        },
        kernel: Some("7.2.7-test".into()),
        architecture: Some("aarch64".into()),
        components,
    }
}

#[test]
fn a_supported_component_requires_evidence_checked_against_the_release() {
    let mut report = report_with(vec![ComponentSupport {
        kind: ComponentKind::Graphics,
        name: "Test graphics".into(),
        present: true,
        state: SupportState::Supported,
        applicability: Applicability::ObservationOnly,
        reason: SupportReason::DriverBound {
            bindings: vec![DeviceBinding {
                device: "Test graphics".into(),
                driver: Some("test".into()),
            }],
        },
        evidence: vec![observed_device("Test graphics", "test")],
        remedy: None,
    }]);
    assert!(
        report.validate().is_err(),
        "an observation alone cannot claim release support"
    );
    report.components[0].applicability = Applicability::ReleaseVerified;
    assert!(
        report.validate().is_err(),
        "observation evidence cannot become release evidence by relabelling"
    );
    report.components[0].evidence[0].scope = EvidenceScope::SelectedRelease;
    report.validate().unwrap();

    // A release-verified fact is the only evidence that may claim support.
    let mut release_fact = report.clone();
    release_fact.components[0].evidence = Vec::new();
    assert!(release_fact.validate().is_err());
}

#[test]
fn absent_optional_devices_do_not_block_but_absent_essentials_do() {
    let optional = |kind| ComponentSupport {
        kind,
        name: String::new(),
        present: false,
        state: SupportState::Unverified,
        applicability: Applicability::Unknown,
        reason: SupportReason::NotDetected,
        evidence: Vec::new(),
        remedy: Some(Remedy::Connect),
    };
    let essential = |kind| ComponentSupport {
        kind,
        name: String::new(),
        present: false,
        state: SupportState::Unsupported,
        applicability: Applicability::ObservationOnly,
        reason: SupportReason::NotDetected,
        evidence: Vec::new(),
        remedy: Some(Remedy::RunOnDestination),
    };
    let mut report = report_with(vec![
        optional(ComponentKind::Wifi),
        optional(ComponentKind::Bluetooth),
        optional(ComponentKind::Camera),
    ]);
    report.validate().unwrap();
    assert!(!report.blocks_install());
    assert_eq!(report.summary().blocking, 0);
    assert_eq!(report.attention().count(), 0);
    assert_eq!(report.working().count(), 0);

    report.components.push(essential(ComponentKind::Storage));
    report.validate().unwrap();
    assert!(report.blocks_install());
    assert_eq!(report.summary().blocking, 1);
}

#[test]
fn imported_report_is_relabelled_and_requires_specific_identity_and_evidence() {
    let report = report_with(vec![ComponentSupport {
        kind: ComponentKind::Graphics,
        name: "Test graphics".into(),
        present: true,
        state: SupportState::Unverified,
        applicability: Applicability::ObservationOnly,
        reason: SupportReason::DriverBound {
            bindings: vec![DeviceBinding {
                device: "Test graphics".into(),
                driver: Some("test".into()),
            }],
        },
        evidence: vec![observed_device("Test graphics", "test")],
        remedy: None,
    }]);
    let bytes = report.to_json().unwrap();
    let imported = SupportReport::import_json(&bytes).unwrap();
    assert_eq!(imported.identity.source, ReportSource::Imported);
    assert!(imported.validate().is_ok());

    let mut unspecific = report;
    unspecific.identity.variant = None;
    unspecific.identity.board = None;
    unspecific.identity.sku = None;
    assert!(unspecific.validate().is_err());
    assert!(SupportReport::declared_model("Test Vendor", "Test Laptop", "").is_err());
    let declared = SupportReport::declared_model("Test Vendor", "Test Laptop", "Board 1").unwrap();
    assert_eq!(declared.identity.source, ReportSource::DeclaredModel);
    assert!(declared.components.is_empty());
    assert!(!declared.blocks_install());
    assert_eq!(declared.architecture, None);
    let detailed = SupportReport::declared_model_with(
        "Test Vendor",
        "Test Laptop",
        "Board 1",
        Some("TESTBOARD"),
        Some("SKU-1"),
    )
    .unwrap();
    assert_eq!(detailed.identity.board.as_deref(), Some("TESTBOARD"));
    assert_eq!(detailed.identity.sku.as_deref(), Some("SKU-1"));
    assert!(detailed.identity.is_specific());

    // A report from an older schema is refused rather than interpreted.
    let mut older: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    older["schema"] = serde_json::json!("tilo.hardware-report.v1");
    assert!(SupportReport::import_json(&serde_json::to_vec(&older).unwrap()).is_err());
}

#[test]
fn declared_model_is_not_component_evidence() {
    let declared = SupportReport::declared_model_with(
        "Test Vendor",
        "Test Laptop",
        "",
        Some("TESTBOARD"),
        None,
    )
    .unwrap();
    assert!(declared.components.is_empty());
    assert_eq!(declared.summary().supported, 0);
    assert_eq!(declared.summary().unverified, 0);
    assert!(!declared.blocks_install());
}

#[test]
fn a_device_without_a_driver_is_limited_not_supported() {
    let raw = RawReport {
        identity: RawIdentity::default(),
        architecture: Some("x86_64".into()),
        wifi: vec![RawNetwork {
            name: "wlan0".into(),
            driver: None,
            firmware: None,
        }],
        ..RawReport::default()
    };
    let report = assemble(raw);
    let wifi = component(&report, ComponentKind::Wifi);
    assert_eq!(wifi.state, SupportState::Limited);
    assert!(matches!(wifi.reason, SupportReason::DriverMissing { .. }));
}

#[test]
fn windows_observation_parses_actual_model_drivers_and_optional_devices() {
    let json = r#"{
      "manufacturer":"Test Vendor","model":"Test Laptop 15","sku":"SKU-1",
      "biosVendor":"Test Firmware","biosVersion":"1.2.3","biosDate":"01/01/2026",
      "boardVendor":"Test Vendor","boardProduct":"TESTBOARD","boardVersion":"1.0",
      "osVersion":"Microsoft Windows NT 10.0.26100.0",
      "firmwareType":"Uefi","secureBoot":true,
      "graphics":[
        {"name":"Test Integrated Graphics","vendor":"Test Vendor","driverVersion":"1.2.3","deviceId":"PCI\\VEN_1002&DEV_15BF"},
        {"name":"Test Discrete Graphics","vendor":"Test Vendor","driverVersion":"4.5.6","deviceId":"PCI\\VEN_10DE&DEV_2860"}
      ],
      "network":[
        {"name":"Test Wi-Fi","type":"Wireless","enabled":true,"id":"PCI\\VEN_8086&DEV_2725"},
        {"name":"Test Ethernet","type":"Ethernet 802.3","enabled":true,"id":"PCI\\VEN_10EC&DEV_8168"}
      ],
      "battery":[{"name":"Test battery","capacity":80,"status":6}],
      "devices":[
        {"name":"Test Bluetooth Adapter","class":"Bluetooth","status":"OK","id":"USB\\VID_1234&PID_0001"},
        {"name":"Test Camera","class":"Camera","status":"OK","id":"USB\\VID_1234&PID_0002"},
        {"name":"Test Fingerprint Sensor","class":"Biometric","status":"OK","id":"USB\\VID_1234&PID_0003"},
        {"name":"Test Speakers","class":"AudioEndpoint","status":"OK","id":"SWD\\MMDEVAPI\\TEST"},
        {"name":"Test Microphone","class":"AudioEndpoint","status":"OK","id":"SWD\\MMDEVAPI\\MIC"},
        {"name":"Test Keyboard","class":"Keyboard","status":"OK","id":"HID\\VID_1234&PID_0004"}
      ],
      "monitors":[{"name":"DISPLAY\\TEST\\MONITOR0","active":true}],
      "disks":[{"name":"Test NVMe","size":"2000398934016","type":"NVMe","status":"OK"}],
      "tpm":[{"spec":"2.0","enabled":true}],
      "drivers":[
        {"deviceId":"PCI\\VEN_1002&DEV_15BF","provider":"Test Vendor","version":"1.2.3"},
        {"deviceId":"PCI\\VEN_10DE&DEV_2860","provider":"Test Vendor","version":"4.5.6"},
        {"deviceId":"PCI\\VEN_8086&DEV_2725","provider":"Test Vendor","version":"10.0.1"},
        {"deviceId":"PCI\\VEN_10EC&DEV_8168","provider":"Test Vendor","version":"10.0.2"},
        {"deviceId":"USB\\VID_1234&PID_0001","provider":"Test Vendor","version":"2.0.0"},
        {"deviceId":"USB\\VID_1234&PID_0002","provider":"Test Vendor","version":"3.0.0"}
      ]
    }"#;
    let raw = windows_json::parse(json).unwrap();
    let report = raw.into_report();
    assert_eq!(report.identity.manufacturer.as_deref(), Some("Test Vendor"));
    assert_eq!(report.identity.model.as_deref(), Some("Test Laptop 15"));
    assert_eq!(report.identity.variant.as_deref(), Some("TESTBOARD"));
    assert_eq!(
        report.kernel.as_deref(),
        Some("Microsoft Windows NT 10.0.26100.0")
    );
    report.validate().unwrap();
    // Windows device-manager drivers are evidence about Windows, not about
    // the Tilo release; every observed component stays not verified.
    for kind in [
        ComponentKind::Graphics,
        ComponentKind::Wifi,
        ComponentKind::Bluetooth,
        ComponentKind::Camera,
        ComponentKind::Boot,
    ] {
        assert_eq!(
            component(&report, kind).state,
            SupportState::Unverified,
            "{}",
            kind.key()
        );
    }
    assert_eq!(
        component(&report, ComponentKind::Fingerprint).state,
        SupportState::Limited
    );
    assert!(!report.blocks_install());
}
