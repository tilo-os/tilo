//! What this host offers for running dev VMs: which hypervisor adapter can
//! work here, the resolved path of every external tool it would run, and
//! the one install line that fixes what is missing.
//!
//! Humans watch dev VMs in the host's native hypervisor UI, so the default
//! adapters are exactly those a UI can see: libvirt's rootless
//! `qemu:///session` on Linux (virt-manager, GNOME Boxes), UTM on macOS and
//! Hyper-V on Windows, or WHPX with QEMU on Windows ARM64 Home. The raw `qemu` adapter (`os/dev/vm.sh`) is the
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
    /// An image-based Linux host with rpm-ostree.
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
    /// Every match on `PATH`, in `PATH` order, for each tool of interest.
    pub path: BTreeMap<String, Vec<PathBuf>>,
    /// Linux: can this user open `/dev/kvm` for reading and writing?
    pub kvm: Option<bool>,
    /// Linux with virsh: the guests `virsh capabilities` lists on
    /// [`SESSION_URI`], which only its QEMU driver answers. `Err` carries
    /// virsh's words.
    pub session: Option<Result<Vec<Guest>, String>>,
    /// macOS: UTM.app's `utmctl`, found even when it is not on `PATH`.
    pub utm_app: Option<PathBuf>,
    /// Windows: whether `PowerShell` has the Hyper-V module (`Get-VM`).
    pub hyperv: Option<bool>,
}

/// One guest architecture libvirt's QEMU driver can run, and how: `kvm`
/// (hardware virtualization) and/or `qemu` (TCG emulation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Guest {
    pub arch: String,
    pub domains: Vec<String>,
}

/// The value of attribute `name` in the tag that starts `tag`.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let tag = tag.split_once('>').map_or(tag, |(inside, _)| inside);
    ['\'', '"'].into_iter().find_map(|quote| {
        let (_, rest) = tag.split_once(&format!("{name}={quote}"))?;
        rest.split_once(quote).map(|(value, _)| value)
    })
}

/// The guests in `virsh capabilities` output.
pub fn parse_guests(capabilities: &str) -> Vec<Guest> {
    capabilities
        .split("<guest>")
        .skip(1)
        .filter_map(|guest| {
            let guest = guest.split("</guest>").next().unwrap_or(guest);
            let (_, arch) = guest.split_once("<arch ")?;
            let arch = attribute(arch, "name")?.to_owned();
            let domains = guest
                .split("<domain ")
                .skip(1)
                .filter_map(|tag| attribute(tag, "type"))
                .map(str::to_owned)
                .collect();
            Some(Guest { arch, domains })
        })
        .collect()
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
    "passt",
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
    let paths = std::env::var_os("PATH").unwrap_or_default();
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
    // MSYS2's ARM64 QEMU is commonly installed without adding its bin to
    // the Windows PATH. Keep the detected binary and firmware in one prefix.
    if cfg!(windows)
        && (tool == "qemu-img" || tool == "qemu-system-aarch64")
        && let Some(home) = std::env::var_os("USERPROFILE")
    {
        let candidate = PathBuf::from(home)
            .join("msys64/clangarm64/bin")
            .join(format!("{tool}.exe"));
        if candidate.is_file() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    // MSYS2 installs rsync in usr/bin, not beside CLANGARM64 QEMU. Probe its
    // default per-user location without changing PATH for unrelated tools.
    if cfg!(windows)
        && tool == "rsync"
        && let Some(home) = std::env::var_os("USERPROFILE")
    {
        let candidate = PathBuf::from(home).join("msys64/usr/bin/rsync.exe");
        if candidate.is_file() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

/// The exact host rsync executable used by worktree and image transfers.
pub fn rsync_program() -> Option<String> {
    if cfg!(windows) {
        which_all("rsync")
            .first()
            .map(|path| path.display().to_string())
    } else {
        Some("rsync".into())
    }
}

/// MSYS2 rsync needs the OpenSSH executable from the same POSIX runtime.
/// Native Windows OpenSSH cannot carry its stdin/stdout protocol reliably.
pub fn rsync_ssh_program() -> Option<PathBuf> {
    let rsync = rsync_program()?;
    let sibling = PathBuf::from(rsync).with_file_name("ssh.exe");
    sibling.is_file().then_some(sibling)
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
                &["--connect", SESSION_URI, "capabilities"],
            );
            match runner.output(&command) {
                Ok(o) if o.success => Ok(parse_guests(&o.stdout)),
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
    if facts.os == "windows" && facts.arch == "aarch64" && facts.hyperv != Some(true) {
        return evaluate(facts, "whpx");
    }
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
        "whpx" => {
            let fix = "install MSYS2 CLANGARM64 QEMU (pacman -S mingw-w64-clang-aarch64-qemu), enable Windows Hypervisor Platform, then reboot".to_owned();
            if facts.os != "windows" || facts.arch != "aarch64" {
                return Err(missing("WHPX requires Windows ARM64".into(), fix));
            }
            let system = "qemu-system-aarch64";
            let qemu = facts.first(system).ok_or_else(|| missing(format!("{system} not found"), fix.clone()))?;
            let img = companion(facts, "qemu-img", qemu, system)
                .map_err(|detail| missing(detail, fix.clone()))?;
            let share = qemu.parent().and_then(Path::parent).map(|p| p.join("share/qemu"));
            let code = share.as_ref().map(|p| p.join("edk2-aarch64-code.fd"))
                .filter(|p| p.is_file()).ok_or_else(|| missing("EDK2 AArch64 firmware not found beside QEMU".into(), fix.clone()))?;
            let vars = share.as_ref().map(|p| p.join("edk2-arm-vars.fd"))
                .filter(|p| p.is_file()).ok_or_else(|| missing("EDK2 ARM variables template not found beside QEMU".into(), fix.clone()))?;
            let ps = facts.first("powershell.exe").ok_or_else(|| missing("powershell.exe not found".into(), fix.clone()))?;
            Ok(Choice { adapter: "whpx", tools: vec![
                ToolPath::new(system, qemu), ToolPath::new("qemu-img", img),
                ToolPath::new("firmware-code", &code), ToolPath::new("firmware-vars", &vars),
                ToolPath::new("powershell.exe", ps),
            ] })
        }
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

/// virsh, the qemu-img beside it, and passt, which the VM's network runs on.
/// tilo-vm writes the domain XML itself, so nothing else (no virt-install,
/// no libvirt storage or network driver) is needed.
///
/// # Errors
/// Which is missing, or a qemu-img from another toolchain.
pub fn libvirt_tools(facts: &HostFacts) -> Result<Vec<ToolPath>, Missing> {
    let missing = |detail: String| Missing {
        adapter: "libvirt".into(),
        detail,
        fix: libvirt_fix(facts),
    };
    let (Some(virsh), Some(passt)) = (facts.first("virsh"), facts.first("passt")) else {
        let absent: Vec<&str> = ["virsh", "passt"]
            .into_iter()
            .filter(|t| facts.first(t).is_none())
            .collect();
        return Err(missing(format!("{} not on PATH", absent.join(" and "))));
    };
    let qemu_img = companion(facts, "qemu-img", virsh, "virsh").map_err(missing)?;
    Ok(vec![
        ToolPath::new("virsh", virsh),
        ToolPath::new("qemu-img", qemu_img),
        ToolPath::new("passt", passt),
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

/// The probe [`HostFacts::probe`] runs against the session's QEMU driver.
pub fn session_probe() -> String {
    format!("virsh --connect {SESSION_URI} capabilities")
}

/// Does libvirt's rootless session QEMU driver answer? Unprobed counts as yes.
///
/// # Errors
/// virsh's complaint, verbatim, with the fix.
pub fn session_answers(facts: &HostFacts) -> Result<(), Missing> {
    if let Some(Err(why)) = &facts.session {
        let probe = session_probe();
        return Err(Missing {
            adapter: "libvirt".into(),
            detail: format!("`{probe}` failed: {why}"),
            fix: format!(
                "`{probe}` must work as you, without sudo; it failed with: {why}. It needs libvirt's QEMU driver (virtqemud, or libvirtd), started on demand for your user: {}",
                libvirt_fix(facts)
            ),
        });
    }
    Ok(())
}

/// Can the session run an `arch` guest, with KVM or, when `emulated`, TCG?
/// Unprobed counts as yes.
///
/// # Errors
/// What libvirt lists instead, with the fix.
pub fn session_domain(facts: &HostFacts, arch: &str, emulated: bool) -> Result<String, Missing> {
    let domain = if emulated { "qemu" } else { "kvm" };
    let probe = session_probe();
    let missing = |detail: String, fix: String| Missing {
        adapter: "libvirt".into(),
        detail,
        fix,
    };
    let guests = match &facts.session {
        Some(Ok(guests)) => guests,
        Some(Err(_)) => return session_answers(facts).map(|()| String::new()),
        None => return Ok(format!("{SESSION_URI} not probed")),
    };
    let Some(guest) = guests.iter().find(|g| g.arch == arch) else {
        let listed: Vec<&str> = guests.iter().map(|g| g.arch.as_str()).collect();
        return Err(missing(
            format!(
                "`{probe}` lists no {arch} guest (only: {})",
                if listed.is_empty() {
                    "none".to_owned()
                } else {
                    listed.join(", ")
                }
            ),
            format!(
                "install QEMU's {} for libvirt: {}",
                qemu_system(arch),
                libvirt_fix(facts)
            ),
        ));
    };
    if guest.domains.iter().any(|d| d == domain) {
        return Ok(format!(
            "{SESSION_URI} runs {arch} guests with {domain} ({probe})"
        ));
    }
    Err(missing(
        format!(
            "`{probe}` offers {arch} guests only as {}, not {domain}",
            guest.domains.join(", ")
        ),
        if emulated {
            libvirt_fix(facts)
        } else {
            kvm_fix(facts)
        },
    ))
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

/// The one line that installs libvirt's QEMU driver, virsh, qemu-img and
/// passt (with virt-manager to watch VMs).
pub fn libvirt_fix(facts: &HostFacts) -> String {
    let fedora = "libvirt-daemon-kvm libvirt-client virt-manager qemu-img passt";
    match facts.kind {
        HostKind::RpmOstree => {
            format!("this host is image-based: rpm-ostree install {fedora}, then reboot")
        }
        HostKind::Bootc => format!(
            "this host is image-based (bootc): add {fedora} to your host image and switch to it"
        ),
        HostKind::Apt => {
            "sudo apt install libvirt-daemon-system libvirt-clients virt-manager qemu-utils passt"
                .into()
        }
        HostKind::Pacman => "sudo pacman -S libvirt virt-manager qemu-full passt".into(),
        HostKind::Zypper => {
            "sudo zypper install libvirt-daemon-qemu libvirt-client virt-manager qemu-tools passt"
                .into()
        }
        HostKind::Macos => "macOS uses UTM: tilo-vm vm create <name> --hypervisor utm".into(),
        HostKind::Windows => {
            "Windows uses Hyper-V: tilo-vm vm create <name> --hypervisor hyperv".into()
        }
        HostKind::Dnf => format!("sudo dnf install {fedora}"),
        HostKind::Unknown => {
            "install libvirt's QEMU daemon, virsh, virt-manager, qemu-img and passt with your package manager".into()
        }
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
        HostKind::Dnf => format!("sudo dnf install {rpm}{tail}"),
        HostKind::Unknown => format!("install {rpm} with your package manager{tail}"),
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
        let session = path.contains_key("virsh").then(|| {
            Ok(vec![Guest {
                arch: "x86_64".into(),
                domains: vec!["qemu".into(), "kvm".into()],
            }])
        });
        HostFacts {
            os: "linux".into(),
            arch: "x86_64".into(),
            kind,
            path,
            kvm: Some(true),
            session,
            utm_app: None,
            hyperv: None,
        }
    }

    pub const LIBVIRT: &[(&str, &str)] = &[
        ("virsh", "/usr/bin/virsh"),
        ("qemu-img", "/usr/bin/qemu-img"),
        ("passt", "/usr/bin/passt"),
    ];

    const ANDROID: &str = "/home/u/Android/Sdk/emulator/qemu-img";

    /// Trimmed `virsh --connect qemu:///session capabilities` from a Fedora
    /// 44 `x86_64` host.
    const CAPABILITIES: &str = "<capabilities>
  <host>
    <cpu><arch>x86_64</arch></cpu>
  </host>
  <guest>
    <os_type>hvm</os_type>
    <arch name='i686'>
      <wordsize>32</wordsize>
      <domain type='qemu'/>
      <domain type='kvm'/>
    </arch>
  </guest>
  <guest>
    <os_type>hvm</os_type>
    <arch name='x86_64'>
      <wordsize>64</wordsize>
      <emulator>/usr/bin/qemu-system-x86_64</emulator>
      <machine maxCpus='4096'>pc-q35-10.1</machine>
      <domain type='qemu'/>
      <domain type='kvm'/>
    </arch>
  </guest>
  <guest>
    <os_type>hvm</os_type>
    <arch name=\"aarch64\">
      <domain type=\"qemu\"/>
    </arch>
  </guest>
</capabilities>";

    /// Capabilities list each guest and its domain types; the session must offer KVM or, for emulation,
    /// TCG; virt-install is irrelevant to libvirt; KVM and the session connection are checked.
    #[test]
    fn host_capabilities_and_session_checks_decide_emulation() {
        // Phase: capabilities_list_each_guest_and_its_domain_types.
        {
            let guests = parse_guests(CAPABILITIES);
            let pairs: Vec<(&str, Vec<&str>)> = guests
                .iter()
                .map(|g| {
                    (
                        g.arch.as_str(),
                        g.domains.iter().map(String::as_str).collect(),
                    )
                })
                .collect();
            assert_eq!(
                pairs,
                [
                    ("i686", vec!["qemu", "kvm"]),
                    ("x86_64", vec!["qemu", "kvm"]),
                    ("aarch64", vec!["qemu"]),
                ]
            );
            assert!(parse_guests("error: no capabilities").is_empty());
        }

        // Phase: the_session_must_offer_kvm_or_for_emulation_tcg.
        {
            let mut facts = linux(HostKind::Dnf, LIBVIRT);
            facts.session = Some(Ok(parse_guests(CAPABILITIES)));
            let ok = session_domain(&facts, "x86_64", false).unwrap();
            assert!(ok.contains("x86_64 guests with kvm"), "{ok}");
            assert!(session_domain(&facts, "aarch64", true).is_ok());
            let no_kvm = session_domain(&facts, "aarch64", false).unwrap_err();
            assert!(
                no_kvm.detail.contains("only as qemu, not kvm"),
                "{}",
                no_kvm.detail
            );
            assert!(no_kvm.fix.contains("usermod -aG kvm"), "{}", no_kvm.fix);
            let absent = session_domain(&facts, "riscv64", true).unwrap_err();
            assert!(
                absent
                    .detail
                    .contains("no riscv64 guest (only: i686, x86_64, aarch64)"),
                "{}",
                absent.detail
            );
            assert!(absent.fix.contains("qemu-system-riscv64"), "{}", absent.fix);

            let stderr = "error: failed to connect to the hypervisor\nerror: Failed to connect socket to '/run/user/1000/libvirt/virtqemud-sock': No such file or directory";
            facts.session = Some(Err(stderr.into()));
            let failed = session_domain(&facts, "x86_64", false).unwrap_err();
            assert!(failed.fix.contains(stderr), "{}", failed.fix);
            assert!(
                failed
                    .detail
                    .contains("`virsh --connect qemu:///session capabilities` failed"),
                "{}",
                failed.detail
            );
        }

        // Phase: virt_install_is_irrelevant_to_libvirt.
        {
            let without = linux(HostKind::Dnf, LIBVIRT);
            let mut with_it = LIBVIRT.to_vec();
            with_it.push(("virt-install", "/usr/bin/virt-install"));
            let with = linux(HostKind::Dnf, &with_it);
            assert_eq!(auto(&without), auto(&with));
            let choice = auto(&without).unwrap();
            assert!(choice.tool("virt-install").is_none());
            assert_eq!(choice.tool("passt"), Some("/usr/bin/passt"));
            let only_install = linux(
                HostKind::Dnf,
                &[
                    ("virt-install", "/usr/bin/virt-install"),
                    ("qemu-img", "/usr/bin/qemu-img"),
                ],
            );
            let why = auto(&only_install).unwrap_err();
            assert_eq!(why.detail, "virsh and passt not on PATH");
            assert!(!why.fix.contains("virt-install"), "{}", why.fix);
            assert!(!TOOLS.contains(&"virt-install"));
        }

        // Phase: kvm_and_the_session_connection_are_checked.
        {
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
                why.detail.contains(
                    "`virsh --connect qemu:///session capabilities` failed: failed to connect to the hypervisor"
                ),
                "{}",
                why.detail
            );
        }
    }

    /// Linux defaults to a libvirt session with resolved paths; the qemu-img beside virsh wins; a stray
    /// qemu-img is named and refused; fixes follow the host kind; raw QEMU is never the default; macOS and
    /// Windows check their hypervisors; Windows ARM home chooses matching QEMU and firmware.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn host_defaults_resolve_one_consistent_toolchain_per_platform() {
        // Phase: linux_defaults_to_libvirt_session_with_resolved_paths.
        {
            let facts = linux(HostKind::Dnf, LIBVIRT);
            let choice = auto(&facts).unwrap();
            assert_eq!(choice.adapter, "libvirt");
            assert_eq!(choice.tool("qemu-img"), Some("/usr/bin/qemu-img"));
            let text = choice.describe();
            assert!(text.contains("qemu:///session"), "{text}");
            assert!(text.contains("virsh = /usr/bin/virsh"), "{text}");
        }

        // Phase: the_qemu_img_beside_virsh_wins_over_an_earlier_one.
        {
            let facts = linux(
                HostKind::Dnf,
                &[
                    ("virsh", "/usr/bin/virsh"),
                    ("passt", "/usr/bin/passt"),
                    ("qemu-img", ANDROID),
                    ("qemu-img", "/usr/bin/qemu-img"),
                ],
            );
            assert_eq!(
                auto(&facts).unwrap().tool("qemu-img"),
                Some("/usr/bin/qemu-img")
            );
        }

        // Phase: a_stray_qemu_img_is_named_and_refused.
        {
            let facts = linux(
                HostKind::Dnf,
                &[
                    ("virsh", "/usr/bin/virsh"),
                    ("passt", "/usr/bin/passt"),
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

        // Phase: fixes_follow_the_host_kind.
        {
            let image = linux(HostKind::RpmOstree, &[]);
            let fix = auto(&image).unwrap_err().fix;
            assert!(
                fix.contains("rpm-ostree install libvirt-daemon-kvm libvirt-client virt-manager"),
                "{fix}"
            );
            assert!(fix.contains("reboot"), "{fix}");
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
                "sudo dnf install libvirt-daemon-kvm libvirt-client virt-manager qemu-img passt"
            );
            for kind in [
                HostKind::Apt,
                HostKind::Pacman,
                HostKind::Zypper,
                HostKind::Unknown,
            ] {
                let fix = auto(&linux(kind, &[])).unwrap_err().fix;
                assert!(
                    !fix.contains("virt-install") && !fix.contains("virtinst"),
                    "{fix}"
                );
                assert!(
                    fix.contains("virt-manager") && fix.contains("passt"),
                    "{fix}"
                );
            }
        }

        // Phase: raw_qemu_is_never_the_default_and_points_people_at_libvirt.
        {
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

        // Phase: macos_and_windows_check_their_hypervisors.
        {
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

        // Phase: windows_arm_home_chooses_matching_qemu_and_firmware.
        {
            let temp = tempfile::tempdir().unwrap();
            let prefix = temp.path().join("clangarm64");
            let bin = prefix.join("bin");
            let share = prefix.join("share/qemu");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::create_dir_all(&share).unwrap();
            let system = bin.join("qemu-system-aarch64.exe");
            let img = bin.join("qemu-img.exe");
            let code = share.join("edk2-aarch64-code.fd");
            let vars = share.join("edk2-arm-vars.fd");
            for file in [&system, &img, &code, &vars] {
                std::fs::write(file, "test").unwrap();
            }
            let mut facts = linux(HostKind::Windows, &[]);
            facts.os = "windows".into();
            facts.arch = "aarch64".into();
            facts.hyperv = Some(false);
            facts
                .path
                .insert("qemu-system-aarch64".into(), vec![system]);
            facts.path.insert("qemu-img".into(), vec![img]);
            facts.path.insert(
                "powershell.exe".into(),
                vec!["C:/Windows/powershell.exe".into()],
            );
            let choice = auto(&facts).unwrap();
            assert_eq!(choice.adapter, "whpx");
            assert_eq!(choice.tool("firmware-code"), Some(code.to_str().unwrap()));
            assert_eq!(choice.tool("firmware-vars"), Some(vars.to_str().unwrap()));
            std::fs::remove_file(vars).unwrap();
            assert!(
                auto(&facts)
                    .unwrap_err()
                    .detail
                    .contains("variables template")
            );
        }
    }
}
