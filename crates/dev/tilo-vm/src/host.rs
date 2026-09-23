//! What this host offers for running dev VMs: which hypervisor adapter can
//! work here, the resolved path of every external tool it would run, and
//! the one install line that fixes what is missing.
//!
//! Humans watch dev VMs in the host's native hypervisor UI, so the default
//! adapters are exactly those a UI can see: libvirt's rootless
//! `qemu:///session` on Linux (virt-manager, GNOME Boxes), UTM on macOS and
//! Hyper-V on Windows. The raw `qemu` adapter (`os/dev/vm.sh`) is the
//! headless adapter for CI and scenario tests; it is used only when asked
//! for by name.
//!
//! [`HostFacts::probe`] looks at the real host once; everything after it is
//! a pure function of those facts, so each decision is unit-tested.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::hypervisor::{CommandLine, Runner};

/// The libvirt connection dev VMs use: per user, no root, visible in
/// virt-manager as "QEMU/KVM User session".
pub const SESSION_URI: &str = "qemu:///session";

/// How to see a libvirt dev VM in a UI.
pub const VIRT_MANAGER_TIP: &str = "watch it in virt-manager: File > Add Connection > QEMU/KVM user session (GNOME Boxes shows session VMs too)";

/// How software gets installed on this host, which decides the fix line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostKind {
    /// An image-based Linux host (Silverblue, Bazzite, Kinoite) with rpm-ostree.
    RpmOstree,
    /// An image-based Linux host with bootc only: packages go in the image.
    Bootc,
    Dnf,
    Apt,
    Pacman,
    Zypper,
    Macos,
    Windows,
    Unknown,
}

impl HostKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::RpmOstree => "image-based Linux (rpm-ostree)",
            Self::Bootc => "image-based Linux (bootc)",
            Self::Dnf => "Linux with dnf",
            Self::Apt => "Linux with apt",
            Self::Pacman => "Linux with pacman",
            Self::Zypper => "Linux with zypper",
            Self::Macos => "macOS",
            Self::Windows => "Windows",
            Self::Unknown => "Linux (unknown package manager)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostFacts {
    pub os: String,
    pub arch: String,
    pub kind: HostKind,
    /// `ID` from `/etc/os-release` (Linux).
    pub distro: Option<String>,
    /// Every match on `PATH`, in `PATH` order, for each tool of interest.
    pub path: BTreeMap<String, Vec<PathBuf>>,
    /// Linux: can this user open `/dev/kvm` for reading and writing?
    pub kvm: Option<bool>,
    /// Linux with virsh: does [`SESSION_URI`] answer? `Err` carries virsh's words.
    pub session: Option<Result<(), String>>,
    /// macOS: UTM.app's `utmctl`, found even when it is not on `PATH`.
    pub utm_app: Option<PathBuf>,
    /// Windows: whether PowerShell has the Hyper-V module (`Get-VM`).
    pub hyperv: Option<bool>,
}

/// Tools `doctor` and the adapters care about.
pub const TOOLS: &[&str] = &[
    "ssh",
    "git",
    "rsync",
    "curl",
    "cargo",
    "python3",
    "podman",
    "bash",
    "virsh",
    "virt-install",
    "qemu-img",
    "qemu-system-x86_64",
    "qemu-system-aarch64",
    "utmctl",
    "powershell.exe",
    "rpm-ostree",
    "bootc",
    "dnf",
    "apt-get",
    "pacman",
    "zypper",
];

/// The qemu system emulator for `arch`.
pub fn qemu_system(arch: &str) -> String {
    format!("qemu-system-{arch}")
}

/// Every executable named `tool` on `PATH`, in order.
pub fn which_all(tool: &str) -> Vec<PathBuf> {
    let Some(paths) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let extensions: Vec<String> = if cfg!(windows) && Path::new(tool).extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in std::env::split_paths(&paths) {
        for ext in &extensions {
            let candidate = dir.join(format!("{tool}{ext}"));
            if candidate.is_file() && !found.contains(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
}

/// `ID=` from os-release text.
pub fn os_release_id(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("ID="))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

impl HostFacts {
    /// Look at this host.
    pub fn probe(runner: &mut dyn Runner) -> Self {
        let path: BTreeMap<String, Vec<PathBuf>> = TOOLS
            .iter()
            .map(|t| ((*t).to_owned(), which_all(t)))
            .filter(|(_, found)| !found.is_empty())
            .collect();
        let has = |t: &str| path.contains_key(t);
        let os = std::env::consts::OS.to_owned();
        let image_based = Path::new("/run/ostree-booted").exists();
        let kind = match os.as_str() {
            "macos" => HostKind::Macos,
            "windows" => HostKind::Windows,
            _ if image_based && has("rpm-ostree") => HostKind::RpmOstree,
            _ if image_based || has("bootc") && !has("dnf") => HostKind::Bootc,
            _ if has("dnf") => HostKind::Dnf,
            _ if has("apt-get") => HostKind::Apt,
            _ if has("pacman") => HostKind::Pacman,
            _ if has("zypper") => HostKind::Zypper,
            _ => HostKind::Unknown,
        };
        let linux = os == "linux";
        let distro = linux
            .then(|| std::fs::read_to_string("/etc/os-release").ok())
            .flatten()
            .and_then(|t| os_release_id(&t));
        let kvm = linux.then(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/kvm")
                .is_ok()
        });
        let session = path.get("virsh").and_then(|v| v.first()).map(|virsh| {
            let command = CommandLine::new(
                &virsh.display().to_string(),
                &["--connect", SESSION_URI, "uri"],
            );
            match runner.output(&command) {
                Ok(o) if o.success => Ok(()),
                Ok(o) => Err(o.stderr.trim().to_owned()),
                Err(e) => Err(e.to_string()),
            }
        });
        let utm_app = (os == "macos")
            .then(|| PathBuf::from("/Applications/UTM.app/Contents/MacOS/utmctl"))
            .filter(|p| p.is_file());
        let hyperv = (os == "windows" && has("powershell.exe")).then(|| {
            runner
                .output(&CommandLine::new(
                    "powershell.exe",
                    &[
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        "if (Get-Command Get-VM -ErrorAction SilentlyContinue) { 'yes' } else { 'no' }",
                    ],
                ))
                .is_ok_and(|o| o.success && o.stdout.trim() == "yes")
        });
        Self {
            os,
            arch: std::env::consts::ARCH.to_owned(),
            kind,
            distro,
            path,
            kvm,
            session,
            utm_app,
            hyperv,
        }
    }

    /// The first `tool` on `PATH`.
    pub fn first(&self, tool: &str) -> Option<&Path> {
        self.path
            .get(tool)
            .and_then(|found| found.first())
            .map(PathBuf::as_path)
    }

    /// `tool` in `dir`, if `PATH` has it there.
    fn beside(&self, tool: &str, dir: &Path) -> Option<&Path> {
        self.path
            .get(tool)?
            .iter()
            .find(|p| p.parent() == Some(dir))
            .map(PathBuf::as_path)
    }
}

/// One external tool an adapter will run, with the path it resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolPath {
    pub tool: String,
    pub path: String,
}

impl ToolPath {
    fn new(tool: &str, path: &Path) -> Self {
        Self {
            tool: tool.into(),
            path: path.display().to_string(),
        }
    }
}

/// An adapter that works on this host, and exactly what it will run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub adapter: &'static str,
    pub tools: Vec<ToolPath>,
}

impl Choice {
    pub fn tool(&self, name: &str) -> Option<&str> {
        self.tools
            .iter()
            .find(|t| t.tool == name)
            .map(|t| t.path.as_str())
    }

    pub fn describe(&self) -> String {
        let tools = self
            .tools
            .iter()
            .map(|t| format!("{} = {}", t.tool, t.path))
            .collect::<Vec<_>>()
            .join(", ");
        let connection = if self.adapter == "libvirt" {
            format!(" ({SESSION_URI})")
        } else {
            String::new()
        };
        format!("{}{connection}: {tools}", self.adapter)
    }
}

/// Why an adapter cannot work here, and the line that fixes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Missing {
    pub adapter: String,
    pub detail: String,
    pub fix: String,
}

/// The adapter a new VM gets on this OS: the one its native UI shows.
pub fn default_adapter(os: &str) -> &'static str {
    match os {
        "macos" => "utm",
        "windows" => "hyperv",
        _ => "libvirt",
    }
}

/// The default adapter, if it works here.
///
/// # Errors
/// What is missing and how to install it.
pub fn auto(facts: &HostFacts) -> Result<Choice, Missing> {
    evaluate(facts, default_adapter(&facts.os))
}

/// Can `adapter` work here?
///
/// # Errors
/// What is missing and how to install it.
pub fn evaluate(facts: &HostFacts, adapter: &str) -> Result<Choice, Missing> {
    let missing = |detail: String, fix: String| Missing {
        adapter: adapter.to_owned(),
        detail,
        fix,
    };
    match adapter {
        "libvirt" => {
            let tools = libvirt_tools(facts)?;
            kvm_usable(facts)?;
            session_answers(facts)?;
            Ok(Choice {
                adapter: "libvirt",
                tools,
            })
        }
        "qemu" => {
            let system = qemu_system(&facts.arch);
            let Some(qemu) = facts.first(&system) else {
                return Err(missing(
                    format!("{system} not on PATH (the qemu adapter is the headless adapter for CI and scenario tests; people use libvirt)"),
                    qemu_fix(facts),
                ));
            };
            let img = companion(facts, "qemu-img", qemu, &system)
                .map_err(|detail| missing(detail, qemu_fix(facts)))?;
            let bash = facts.first("bash").ok_or_else(|| {
                missing("bash not on PATH".into(), "install bash".into())
            })?;
            Ok(Choice {
                adapter: "qemu",
                tools: vec![
                    ToolPath::new(&system, qemu),
                    ToolPath::new("qemu-img", img),
                    ToolPath::new("bash", bash),
                ],
            })
        }
        "utm" => {
            if let Some(utmctl) = facts.first("utmctl") {
                return Ok(Choice {
                    adapter: "utm",
                    tools: vec![ToolPath::new("utmctl", utmctl)],
                });
            }
            let fix_line = match &facts.utm_app {
                Some(app) => format!(
                    "UTM is installed but utmctl is not on PATH: sudo ln -s '{}' /usr/local/bin/utmctl",
                    app.display()
                ),
                None => "install UTM (brew install --cask utm, or from mac.getutm.app), then: sudo ln -s /Applications/UTM.app/Contents/MacOS/utmctl /usr/local/bin/utmctl".into(),
            };
            Err(missing("utmctl not on PATH".into(), fix_line))
        }
        "hyperv" => match (facts.first("powershell.exe"), facts.hyperv) {
            (Some(ps), Some(true)) => Ok(Choice {
                adapter: "hyperv",
                tools: vec![ToolPath::new("powershell.exe", ps)],
            }),
            (None, _) => Err(missing(
                "powershell.exe not on PATH".into(),
                "run tilo-vm from a Windows shell that has powershell.exe on PATH".into(),
            )),
            _ => Err(missing(
                "PowerShell has no Hyper-V module (Get-VM)".into(),
                "enable Hyper-V (Windows Pro, Enterprise or Education), in an admin PowerShell: Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All, then reboot".into(),
            )),
        },
        other => Err(missing(
            format!("unknown hypervisor '{other}'"),
            "use libvirt, qemu, utm or hyperv".into(),
        )),
    }
}

/// virsh, virt-install and the qemu-img beside them.
///
/// # Errors
/// Which is missing, or a qemu-img from another toolchain.
pub fn libvirt_tools(facts: &HostFacts) -> Result<Vec<ToolPath>, Missing> {
    let missing = |detail: String| Missing {
        adapter: "libvirt".into(),
        detail,
        fix: libvirt_fix(facts),
    };
    let (Some(virsh), Some(install)) = (facts.first("virsh"), facts.first("virt-install")) else {
        let absent: Vec<&str> = ["virsh", "virt-install"]
            .into_iter()
            .filter(|t| facts.first(t).is_none())
            .collect();
        return Err(missing(format!("{} not on PATH", absent.join(" and "))));
    };
    let qemu_img = companion(facts, "qemu-img", virsh, "virsh").map_err(missing)?;
    Ok(vec![
        ToolPath::new("virsh", virsh),
        ToolPath::new("virt-install", install),
        ToolPath::new("qemu-img", qemu_img),
    ])
}

/// Linux: can this user use KVM? Unknown elsewhere counts as yes.
///
/// # Errors
/// `/dev/kvm` cannot be opened, with the fix.
pub fn kvm_usable(facts: &HostFacts) -> Result<(), Missing> {
    if facts.kvm == Some(false) {
        return Err(Missing {
            adapter: "libvirt".into(),
            detail: "cannot open /dev/kvm for reading and writing".into(),
            fix: kvm_fix(facts),
        });
    }
    Ok(())
}

/// Does libvirt's rootless session answer? Unprobed counts as yes.
///
/// # Errors
/// virsh's complaint, with the fix.
pub fn session_answers(facts: &HostFacts) -> Result<(), Missing> {
    if let Some(Err(why)) = &facts.session {
        return Err(Missing {
            adapter: "libvirt".into(),
            detail: format!("{SESSION_URI} does not answer: {why}"),
            fix: format!(
                "virsh --connect {SESSION_URI} uri must work as you, without sudo; it needs the libvirt daemon: {}",
                libvirt_fix(facts)
            ),
        });
    }
    Ok(())
}

/// `tool` beside `anchor` (the same toolchain), else an explanation. A
/// `tool` elsewhere on `PATH` (an Android SDK's qemu-img, say) is named and
/// refused rather than silently used.
fn companion<'a>(
    facts: &'a HostFacts,
    tool: &str,
    anchor: &Path,
    anchor_name: &str,
) -> Result<&'a Path, String> {
    let dir = anchor.parent().unwrap_or_else(|| Path::new("/"));
    if let Some(found) = facts.beside(tool, dir) {
        return Ok(found);
    }
    match facts.first(tool) {
        Some(stray) => Err(format!(
            "{tool} on PATH is {}, not beside {anchor_name} in {}; refusing a {tool} from another toolchain",
            stray.display(),
            dir.display()
        )),
        None => Err(format!("{tool} not on PATH")),
    }
}

/// The one line that installs libvirt (with virt-manager to watch VMs).
pub fn libvirt_fix(facts: &HostFacts) -> String {
    let fedora = "libvirt-daemon-kvm virt-install virt-manager qemu-img";
    let bazzite = if facts.distro.as_deref() == Some("bazzite") {
        "ujust setup-virtualization (Bazzite's own recipe), or "
    } else {
        ""
    };
    match facts.kind {
        HostKind::RpmOstree => {
            format!("this host is image-based: {bazzite}rpm-ostree install {fedora}, then reboot")
        }
        HostKind::Bootc => format!(
            "this host is image-based (bootc): add {fedora} to your host image and switch to it"
        ),
        HostKind::Apt => {
            "sudo apt install libvirt-daemon-system libvirt-clients virtinst virt-manager qemu-utils"
                .into()
        }
        HostKind::Pacman => "sudo pacman -S libvirt virt-install virt-manager qemu-full".into(),
        HostKind::Zypper => {
            "sudo zypper install libvirt-daemon-qemu virt-install virt-manager qemu-tools".into()
        }
        HostKind::Macos => "macOS uses UTM: tilo-vm vm create <name> --hypervisor utm".into(),
        HostKind::Windows => {
            "Windows uses Hyper-V: tilo-vm vm create <name> --hypervisor hyperv".into()
        }
        HostKind::Dnf | HostKind::Unknown => format!("sudo dnf install {fedora}"),
    }
}

fn kvm_fix(facts: &HostFacts) -> String {
    let group = match facts.kind {
        HostKind::RpmOstree | HostKind::Bootc => {
            // The kvm group lives in the image's /usr/lib/group, so usermod cannot see it.
            "grep -E '^kvm:' /usr/lib/group | sudo tee -a /etc/group && sudo usermod -aG kvm $USER"
        }
        _ => "sudo usermod -aG kvm $USER",
    };
    format!(
        "enable virtualization (VT-x/AMD-V) in the firmware if /dev/kvm is missing; otherwise join the kvm group: {group}, then log out and in"
    )
}

fn qemu_fix(facts: &HostFacts) -> String {
    let aarch64 = facts.arch == "aarch64";
    let rpm = format!(
        "{} qemu-img",
        if aarch64 {
            "qemu-system-aarch64"
        } else {
            "qemu-system-x86"
        }
    );
    let tail = "; for a VM you can watch, use libvirt instead: tilo-vm vm create <name>";
    match facts.kind {
        HostKind::RpmOstree => format!("rpm-ostree install {rpm}, then reboot{tail}"),
        HostKind::Bootc => format!("add {rpm} to your host image{tail}"),
        HostKind::Apt => format!(
            "sudo apt install {} qemu-utils{tail}",
            if aarch64 {
                "qemu-system-arm"
            } else {
                "qemu-system-x86"
            }
        ),
        HostKind::Pacman => format!("sudo pacman -S qemu-full{tail}"),
        HostKind::Zypper => format!("sudo zypper install qemu-kvm qemu-tools{tail}"),
        HostKind::Macos | HostKind::Windows => {
            "the qemu adapter runs on Linux only (CI and scenario tests)".into()
        }
        HostKind::Dnf | HostKind::Unknown => format!("sudo dnf install {rpm}{tail}"),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A Linux host with `tools` at the given paths, KVM usable and the
    /// libvirt session answering.
    pub fn linux(kind: HostKind, tools: &[(&str, &str)]) -> HostFacts {
        let mut path: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for (tool, at) in tools {
            path.entry((*tool).to_owned())
                .or_default()
                .push(PathBuf::from(at));
        }
        let session = path.contains_key("virsh").then_some(Ok(()));
        HostFacts {
            os: "linux".into(),
            arch: "x86_64".into(),
            kind,
            distro: None,
            path,
            kvm: Some(true),
            session,
            utm_app: None,
            hyperv: None,
        }
    }

    pub const LIBVIRT: &[(&str, &str)] = &[
        ("virsh", "/usr/bin/virsh"),
        ("virt-install", "/usr/bin/virt-install"),
        ("qemu-img", "/usr/bin/qemu-img"),
    ];

    const ANDROID: &str = "/home/u/Android/Sdk/emulator/qemu-img";

    #[test]
    fn linux_defaults_to_libvirt_session_with_resolved_paths() {
        let facts = linux(HostKind::Dnf, LIBVIRT);
        let choice = auto(&facts).unwrap();
        assert_eq!(choice.adapter, "libvirt");
        assert_eq!(choice.tool("qemu-img"), Some("/usr/bin/qemu-img"));
        let text = choice.describe();
        assert!(text.contains("qemu:///session"), "{text}");
        assert!(text.contains("virsh = /usr/bin/virsh"), "{text}");
    }

    #[test]
    fn the_qemu_img_beside_virsh_wins_over_an_earlier_one() {
        let facts = linux(
            HostKind::Dnf,
            &[
                ("virsh", "/usr/bin/virsh"),
                ("virt-install", "/usr/bin/virt-install"),
                ("qemu-img", ANDROID),
                ("qemu-img", "/usr/bin/qemu-img"),
            ],
        );
        assert_eq!(
            auto(&facts).unwrap().tool("qemu-img"),
            Some("/usr/bin/qemu-img")
        );
    }

    #[test]
    fn a_stray_qemu_img_is_named_and_refused() {
        let facts = linux(
            HostKind::Dnf,
            &[
                ("virsh", "/usr/bin/virsh"),
                ("virt-install", "/usr/bin/virt-install"),
                ("qemu-img", ANDROID),
            ],
        );
        let why = auto(&facts).unwrap_err();
        assert!(why.detail.contains(ANDROID), "{}", why.detail);
        assert!(why.detail.contains("refusing"), "{}", why.detail);
        assert!(
            why.fix.starts_with("sudo dnf install libvirt-daemon-kvm"),
            "{}",
            why.fix
        );
    }

    #[test]
    fn kvm_and_the_session_connection_are_checked() {
        let mut facts = linux(HostKind::Dnf, LIBVIRT);
        facts.kvm = Some(false);
        let why = auto(&facts).unwrap_err();
        assert!(why.detail.contains("/dev/kvm"), "{}", why.detail);
        assert!(why.fix.contains("usermod -aG kvm"), "{}", why.fix);
        let mut image = linux(HostKind::RpmOstree, LIBVIRT);
        image.kvm = Some(false);
        assert!(auto(&image).unwrap_err().fix.contains("/usr/lib/group"));
        let mut facts = linux(HostKind::Dnf, LIBVIRT);
        facts.session = Some(Err("failed to connect to the hypervisor".into()));
        let why = auto(&facts).unwrap_err();
        assert!(
            why.detail.contains("qemu:///session does not answer"),
            "{}",
            why.detail
        );
    }

    #[test]
    fn fixes_follow_the_host_kind() {
        let image = linux(HostKind::RpmOstree, &[]);
        let fix = auto(&image).unwrap_err().fix;
        assert!(
            fix.contains("rpm-ostree install libvirt-daemon-kvm virt-install virt-manager"),
            "{fix}"
        );
        assert!(fix.contains("reboot"), "{fix}");
        assert!(!fix.contains("ujust"), "{fix}");
        let mut bazzite = linux(HostKind::RpmOstree, &[]);
        bazzite.distro = Some("bazzite".into());
        assert!(
            auto(&bazzite)
                .unwrap_err()
                .fix
                .contains("ujust setup-virtualization")
        );
        let bootc = linux(HostKind::Bootc, &[]);
        assert!(
            auto(&bootc)
                .unwrap_err()
                .fix
                .contains("add libvirt-daemon-kvm")
        );
        let apt = linux(HostKind::Apt, &[]);
        assert!(
            auto(&apt)
                .unwrap_err()
                .fix
                .starts_with("sudo apt install libvirt-daemon-system")
        );
        let dnf = linux(HostKind::Dnf, &[]);
        assert_eq!(
            auto(&dnf).unwrap_err().fix,
            "sudo dnf install libvirt-daemon-kvm virt-install virt-manager qemu-img"
        );
    }

    #[test]
    fn raw_qemu_is_never_the_default_and_points_people_at_libvirt() {
        let facts = linux(
            HostKind::Dnf,
            &[
                ("qemu-system-x86_64", "/usr/bin/qemu-system-x86_64"),
                ("qemu-img", "/usr/bin/qemu-img"),
                ("bash", "/usr/bin/bash"),
            ],
        );
        assert_eq!(auto(&facts).unwrap_err().adapter, "libvirt");
        let qemu = evaluate(&facts, "qemu").unwrap();
        assert_eq!(qemu.tool("qemu-img"), Some("/usr/bin/qemu-img"));
        let bare = linux(HostKind::Dnf, &[]);
        let why = evaluate(&bare, "qemu").unwrap_err();
        assert!(why.fix.contains("use libvirt instead"), "{}", why.fix);
    }

    #[test]
    fn macos_and_windows_check_their_hypervisors() {
        let mut mac = linux(HostKind::Macos, &[]);
        mac.os = "macos".into();
        let why = auto(&mac).unwrap_err();
        assert_eq!(why.adapter, "utm");
        assert!(why.fix.contains("brew install --cask utm"));
        mac.utm_app = Some("/Applications/UTM.app/Contents/MacOS/utmctl".into());
        assert!(auto(&mac).unwrap_err().fix.contains("sudo ln -s"));
        mac.path
            .insert("utmctl".into(), vec!["/usr/local/bin/utmctl".into()]);
        assert_eq!(auto(&mac).unwrap().adapter, "utm");

        let mut win = linux(
            HostKind::Windows,
            &[("powershell.exe", r"C:\ps\powershell.exe")],
        );
        win.os = "windows".into();
        win.hyperv = Some(false);
        let why = auto(&win).unwrap_err();
        assert!(why.fix.contains("Microsoft-Hyper-V"), "{}", why.fix);
        win.hyperv = Some(true);
        assert_eq!(auto(&win).unwrap().adapter, "hyperv");
    }

    #[test]
    fn os_release_ids_parse() {
        assert_eq!(
            os_release_id("NAME=\"Bazzite\"\nID=bazzite\nID_LIKE=\"fedora\"\n").as_deref(),
            Some("bazzite")
        );
        assert_eq!(os_release_id("ID=\"fedora\"").as_deref(), Some("fedora"));
        assert_eq!(os_release_id("NAME=x"), None);
    }
}
