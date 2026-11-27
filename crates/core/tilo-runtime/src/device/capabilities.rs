//! What the daemon's program mounts on this device beside the core: the services that answer
//! on the daemon's dispatch, and what the machine does that only an optional subsystem knows how
//! to do (hold suspend off, read a radio's wake setting, carry the device VPN).
//!
//! The core names none of those subsystems. The `tilo-runtime` program wires them in once, before
//! the daemon starts ([`mount`]): each service is registered on the one table of providers the
//! daemon's dispatch answers from ([`Providers`]), as the core's own are, and each hook is called
//! where the core needs it. A daemon nothing was mounted on (a test's, or a program that runs one
//! in process without them) runs the core alone: it serves none of those services, and says so
//! where it would have used one.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use tilo_workspace_records::search::Answers;
use tilo_workspace_service::workspaces::providers::Providers;

/// What the daemon's services are started with, as it starts them: where they register, and what
/// of the device they read.
pub struct Mounting<'a> {
    /// The one table of providers the daemon's dispatch answers from.
    pub providers: &'a Providers,
    /// The blob service this daemon serves, which a picture's bytes are read from.
    pub blob_socket: &'a Path,
    /// The device's index, once its search is served.
    pub search: Option<Arc<dyn Answers>>,
    /// The person the daemon acts for.
    pub person: &'a str,
}

/// Something kept while the machine is held awake: dropping it lets the machine sleep again.
pub type Awake = Box<dyn Send>;

/// Whether the Wi-Fi radio named (`phy0`) wakes its machine on a magic packet.
pub type RadioWakes = fn(&str) -> Result<bool, String>;

/// Taking a hold on suspend: `None` where the machine cannot be held now.
pub type Holding = Pin<Box<dyn Future<Output = Option<Awake>> + Send>>;

/// Registers one service on the daemon's providers as the daemon starts.
pub type Register = Box<dyn Fn(&Mounting<'_>) + Send + Sync>;

/// The person's devices, as their record lists them: certified by their root, not taken back and
/// not expired. A mounted device VPN admits only these.
pub type Devices = Arc<dyn Fn() -> BTreeSet<[u8; 32]> + Send + Sync>;

/// What a mounted device VPN serves on the endpoint: the protocols it answers there, and what
/// carries a shell's VPN links until they end.
pub struct VpnServing {
    pub protocols: Vec<(&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>)>,
    pub links: tilo_app_host::shell::DeviceVpnLinks,
}

/// Serves a device VPN on the endpoint, admitting the person's devices.
pub type DeviceVpn = Arc<dyn Fn(iroh::Endpoint, Devices) -> VpnServing + Send + Sync>;

/// The person's devices' fabric, as the daemon's speakers travel reaches it: the envelope one of
/// their devices sends another, the check that it is that device's to that one, and the clock
/// both time it by. The envelope model lives with the desktop's network stack, so the program
/// that runs the daemon mounts its implementation ([`Capabilities::fabric`]): the core names the
/// interface alone.
pub trait DeviceFabric: Send + Sync {
    /// When a frame reached this device, in microseconds on the monotonic clock both the person's
    /// devices are on.
    fn now_us(&self) -> u64;

    /// `envelope`, read and checked as the one `source` sent `target` at `now_ms`: the bytes a
    /// link carries on, when it is; what is wrong with it otherwise.
    ///
    /// # Errors
    /// The envelope does not read, is larger than one link carries, or is not `source`'s to
    /// `target`.
    fn check(
        &self,
        source: [u8; 32],
        target: [u8; 32],
        now_ms: u64,
        envelope: &[u8],
    ) -> Result<Vec<u8>, String>;
}

/// What the daemon's program mounts.
#[derive(Default)]
pub struct Capabilities {
    /// The language runtime the program links, which its workspace service reads and changes
    /// tilolang apps' records through ([`tilolang`]): the daemon's program mounts it.
    pub tilolang:
        Option<Box<dyn Fn() -> tilo_workspace_service::tl_records::Tilolang + Send + Sync>>,
    /// The services registered on the daemon's providers once its search is served.
    pub providers: Vec<Register>,
    /// Holds suspend off while another of the person's devices uses this machine.
    pub stay_awake: Option<Box<dyn Fn() -> Holding + Send + Sync>>,
    /// Whether the Wi-Fi radio named (`phy0`) wakes its machine on a magic packet.
    pub radio_wakes: Option<RadioWakes>,
    /// The platform's notifications, which keep a notification for a later moment and show it
    /// whether or not this program runs then: a moment an app's trigger lands on this device is
    /// armed there ([`tilo_app_host::wakeups`]). A phone's app mounts its own.
    pub alarms: Option<Arc<dyn tilo_app_host::wakeups::Notifier>>,
    /// The device VPN, which answers the person's other devices on the endpoint: the daemon is
    /// the one process with the device key.
    pub vpn: Option<DeviceVpn>,
    /// The person's devices' fabric, which checks the envelopes their speakers travel in: the
    /// desktop network's model, which a daemon nothing was mounted on (a phone's) carries none
    /// of, and then serves no speakers travel.
    pub fabric: Option<Arc<dyn DeviceFabric>>,
}

static MOUNTED: OnceLock<Capabilities> = OnceLock::new();

/// Mount `capabilities` on every daemon this process starts. A process mounts them once, before
/// its first daemon starts.
///
/// # Panics
/// When this process mounted its capabilities already.
pub fn mount(capabilities: Capabilities) {
    assert!(
        MOUNTED.set(capabilities).is_ok(),
        "a process mounts the daemon's capabilities once"
    );
}

fn mounted() -> Option<&'static Capabilities> {
    MOUNTED.get()
}

/// Register the mounted services on `at.providers`.
pub(crate) fn register(at: &Mounting<'_>) {
    for register in mounted().map_or(&[][..], |mounted| &mounted.providers[..]) {
        register(at);
    }
}

/// The language runtime mounted here, where the program mounted one: what the workspace service
/// reads and changes tilolang apps' records through.
pub(crate) fn tilolang() -> Option<tilo_workspace_service::tl_records::Tilolang> {
    mounted()?.tilolang.as_ref().map(|host| host())
}

/// The platform's notifications the program mounted for this device's wakeups, where it did.
#[cfg(not(windows))]
pub(crate) fn alarms() -> Option<Arc<dyn tilo_app_host::wakeups::Notifier>> {
    mounted()?.alarms.clone()
}

/// The device VPN the program mounted, where it mounted one.
pub(crate) fn vpn() -> Option<DeviceVpn> {
    mounted()?.vpn.clone()
}

/// The person's devices' fabric the program mounted, where it mounted one.
pub(crate) fn fabric() -> Option<Arc<dyn DeviceFabric>> {
    mounted()?.fabric.clone()
}

/// The person's devices as their record lists them, for a mounted device VPN to admit: certified
/// by their root, not taken back, and not expired. The record is read as this device last saved
/// it, as every decision that cannot wait reads it, so a dial never waits on the app runtime.
pub(crate) fn person_devices(
    devices: tilo_workspace_service::workspaces::FollowedRecord,
    person: [u8; 32],
) -> Devices {
    Arc::new(move || {
        let now = tilo_clock::unix_now_ns();
        devices
            .state::<tilo_workspace_service::workspaces::person_workspace::devices::Devices>()
            .map(|record| {
                record
                    .listed(&person)
                    .into_iter()
                    .filter(|listed| {
                        !listed.device.revoked(&listed.cert) && !listed.cert.is_expired_at(now)
                    })
                    .map(|listed| listed.cert.device_public_key)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Take a hold on suspend, where something mounted can.
#[cfg(target_os = "linux")]
pub(crate) async fn stay_awake() -> Option<Awake> {
    let stay_awake = mounted()?.stay_awake.as_ref()?;
    stay_awake().await
}

/// Whether the Wi-Fi radio `phy` wakes its machine on a magic packet.
///
/// # Errors
/// Nothing mounted reads a radio's wake setting, or the radio's answer.
#[cfg(target_os = "linux")]
pub(crate) fn radio_wakes(phy: &str) -> Result<bool, String> {
    let read = mounted()
        .and_then(|mounted| mounted.radio_wakes)
        .ok_or_else(|| "this daemon reads no radio's wake setting".to_owned())?;
    read(phy)
}

/// A tilolang process this device started on Windows.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) enum Confined {
    /// In its app's `AppContainer`, in a job the daemon holds ([`launcher_windows::launch`]).
    Contained(launcher_windows::Launched),
    /// With no confinement at all: a test build's start where Windows has no `AppContainer`
    /// ([`allow_unconfined_test_launch`]).
    #[cfg(feature = "test-support")]
    Unconfined(runtime_portal::process_link::Child),
}

#[cfg(windows)]
impl Confined {
    /// Its process id.
    pub(crate) const fn pid(&self) -> u32 {
        match self {
            Self::Contained(launched) => launched.pid(),
            #[cfg(feature = "test-support")]
            Self::Unconfined(child) => child.id(),
        }
    }

    /// Its error output, taken once.
    pub(crate) fn take_stderr(&mut self) -> Option<std::fs::File> {
        match self {
            Self::Contained(launched) => launched.child.stderr.take(),
            #[cfg(feature = "test-support")]
            Self::Unconfined(child) => child.stderr.take(),
        }
    }

    /// End it now: a contained process with everything in its job, as the job closes.
    pub(crate) fn kill(self) {
        match self {
            Self::Contained(launched) => drop(launched),
            #[cfg(feature = "test-support")]
            Self::Unconfined(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// Wait for it to end, and how it ended.
    ///
    /// # Errors
    /// The system's error waiting for it.
    pub(crate) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Contained(launched) => launched.wait(),
            #[cfg(feature = "test-support")]
            Self::Unconfined(child) => child.wait(),
        }
    }
}

/// Whether this test build starts a tilolang process unconfined where Windows has no
/// `AppContainer`.
#[cfg(all(windows, feature = "test-support"))]
static UNCONFINED_TEST_LAUNCH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Let the daemons of this test program start a tilolang process with no confinement at all
/// where Windows has no `AppContainer` to start it in (Wine), so a walk through the daemon there
/// reaches an app. It exists only in a build with `test-support`, which no shipping build has
/// (`tools/policy/check_test_support_shipping.py`), and nothing else turns it on. A process it
/// starts is confined by nothing: a walk that uses it proves nothing about the sandbox. Where
/// Windows has an `AppContainer`, every start is confined whether this was called or not.
#[cfg(all(windows, feature = "test-support"))]
pub fn allow_unconfined_test_launch() {
    UNCONFINED_TEST_LAUNCH.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Start `request` confined: in its app's `AppContainer`, granted its paths alone, in a job the
/// daemon holds ([`launcher_windows::launch`]). Where Windows has no `AppContainer`, the start is
/// refused, unless this is a test program that allowed an unconfined start
/// ([`allow_unconfined_test_launch`]).
///
/// # Errors
/// As [`launcher_windows::launch`].
#[cfg(windows)]
pub(crate) fn launch_confined(
    request: launcher_windows::LaunchRequest,
) -> std::io::Result<(runtime_portal::process_link::Descriptor, Confined)> {
    #[cfg(feature = "test-support")]
    let command = clone_command(&request.command);
    match launcher_windows::launch(request) {
        Ok((link, launched)) => Ok((link, Confined::Contained(launched))),
        #[cfg(feature = "test-support")]
        Err(error)
            if error.kind() == std::io::ErrorKind::Unsupported
                && UNCONFINED_TEST_LAUNCH.load(std::sync::atomic::Ordering::SeqCst) =>
        {
            tracing::warn!(
                %error,
                "starting a tilolang process unconfined: this test build allowed it where Windows \
                 has no AppContainer"
            );
            unconfined_test_launch(command)
        }
        Err(error) => Err(error),
    }
}

/// The test build's start of `command` with no confinement: [`launch_confined`] where Windows has
/// no `AppContainer` and the test program allowed it.
#[cfg(all(windows, feature = "test-support"))]
fn unconfined_test_launch(
    command: std::process::Command,
) -> std::io::Result<(runtime_portal::process_link::Descriptor, Confined)> {
    runtime_portal::process_link::spawn(command)
        .map(|(link, child)| (link, Confined::Unconfined(child)))
}

/// A copy of `command`: its program, arguments, environment and directory.
#[cfg(all(windows, feature = "test-support"))]
fn clone_command(command: &std::process::Command) -> std::process::Command {
    let mut copy = std::process::Command::new(command.get_program());
    copy.args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => copy.env(key, value),
            None => copy.env_remove(key),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        copy.current_dir(dir);
    }
    copy
}
