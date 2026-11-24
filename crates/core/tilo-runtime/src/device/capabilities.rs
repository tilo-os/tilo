//! What the daemon's program mounts on this device beside the core: the services that answer
//! on the daemon's dispatch, and what the machine does that only an optional subsystem knows how
//! to do (hold suspend off, read a radio's wake setting, serve the account sign-ins).
//!
//! The core names none of those subsystems. The `tilo-runtime` program wires them in once, before
//! the daemon starts ([`mount`]): each service is registered on the one table of providers the
//! daemon's dispatch answers from ([`Providers`]), as the core's own are, and each hook is called
//! where the core needs it. A daemon nothing was mounted on (a test's, or a program that runs one
//! in process without them) runs the core alone: it serves none of those services, and says so
//! where it would have used one.

use std::any::Any;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use tilo_actions::remote::{Caller, ResourceSearch};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::Providers;

pub use crate::apps::connectors::SignIns;

/// What the daemon's services are started with, as it starts them: where they register, and what
/// of the device they read.
pub struct Mounting<'a> {
    /// The one table of providers the daemon's dispatch answers from.
    pub providers: &'a Providers,
    /// The blob service this daemon serves, which a picture's bytes are read from.
    pub blob_socket: &'a Path,
    /// The device's index, once its search is served.
    pub search: Option<Arc<dyn ResourceSearch>>,
    /// The person the daemon acts for.
    pub person: &'a str,
}

/// Something kept while the machine is held awake: dropping it lets the machine sleep again.
pub type Awake = Box<dyn Send>;

/// Taking a hold on suspend: `None` where the machine cannot be held now.
pub type Holding = Pin<Box<dyn Future<Output = Option<Awake>> + Send>>;

/// The account sign-ins while they are served: what hands an origin connector its own, and the
/// service itself, which stops when dropped.
pub struct SignInsServed {
    pub hands: Box<SignIns>,
    pub served: Box<dyn Any + Send + Sync>,
}

/// Registers one service on the daemon's providers as the daemon starts.
pub type Register = Box<dyn Fn(&Mounting<'_>) + Send + Sync>;

/// Serves the account sign-ins of the workspaces it is handed, as the person it is handed.
pub type ServeSignIns = Box<dyn Fn(&Workspaces, Caller) -> Option<SignInsServed> + Send + Sync>;

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
    /// The account sign-ins this login's cloud service, model manager and origin connectors
    /// reach.
    pub account_sign_ins: Option<ServeSignIns>,
    /// Whether the Wi-Fi radio named (`phy0`) wakes its machine on a magic packet.
    pub radio_wakes: Option<fn(&str) -> Result<bool, String>>,
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

/// Take a hold on suspend, where something mounted can.
#[cfg(target_os = "linux")]
pub(crate) async fn stay_awake() -> Option<Awake> {
    let stay_awake = mounted()?.stay_awake.as_ref()?;
    stay_awake().await
}

/// Serve the account sign-ins of `workspaces` as `person`, where something mounted does.
pub(crate) fn account_sign_ins(workspaces: &Workspaces, person: Caller) -> Option<SignInsServed> {
    let Some(serve) = mounted().and_then(|mounted| mounted.account_sign_ins.as_ref()) else {
        tracing::warn!(
            "this daemon serves no account sign-ins: the cloud service and origin connectors reach \
             no account's sign-in"
        );
        return None;
    };
    serve(workspaces, person)
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
