//! A hosted workspace's Browser: a window that runs in a VM of the
//! workspace's own, reached through this host.
//!
//! The relay runs no apps. Browser's page actions exist only on a live
//! Browser window, so a workspace hosted here opens Browser by starting a
//! *browser environment*: a short-lived VM beside the workspace's Linux VM,
//! running a headless Tilo session (`docs/hosted-browser.md`). That session
//! is a device of the workspace — a fresh key this host certifies for this
//! one workspace — and it opens the Browser window on the record the open
//! made, so the window is live and published like any other.
//!
//! This module is what the relay does about it:
//!
//! - **Place** ([`HostedBrowsers::place`]): after `app.so.tilo.browser#open`
//!   made a record, certify a device for the environment scoped to the
//!   workspace, list it as a member device, ask the environments service to
//!   start it ([`BrowserEnvironments::start`]) with that material, and
//!   remember the window it answered. The open answers once the window is
//!   live; a second open attaches to the same environment.
//! - **Route** ([`HostedBrowsers::route`]): an action addressed to that
//!   window reaches this host (a door agent's calls go to the workspace's
//!   host device) and is handed to the device holding the window, stamped
//!   with the caller, so it runs there as the caller's: History records the
//!   agent's run for the person it acts for. The relay runs none of it, and
//!   the approval a page action needs is asked there, as on any device.
//! - **Meter** ([`BrowserMeter`]): every page action handed over is admitted
//!   for the workspace's account first, and counted. A refused one is
//!   answered with the host's typed [`Refusal`] — the VM path's refusal, not
//!   a second one.
//!
//! What starts and stops environments is the hosted-VM service's
//! ([`BrowserEnvironments`]): this host places, routes and counts.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use tilo_actions::remote::host_account::{NextStep, NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, InvokeRequest,
    RemoteActionMessage, RemoteActionTarget, provider_workspace,
};
use tilo_actions::selection::logical_resource_id;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

/// The app a hosted workspace opens Browser through.
pub const APP: &str = "so.tilo.browser";
/// The resource the window's page actions are actions of (`browser.page`);
/// their provider names the window (`browser@42`).
pub const PAGE_RESOURCE: &str = "browser.page";
/// How long a device certified for an environment lasts: as long as the
/// environment may run.
pub const DEVICE_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);
/// The label the workspace shows the environment's device under.
pub const DEVICE_LABEL: &str = "Browser";

/// What a host hands a browser environment when it gives it a workspace: the
/// device it certified for it, and where to be that device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Material {
    /// The workspace, by its UUID: the one that lists the device.
    pub workspace: String,
    /// The workspace, as the relay carries it.
    pub workspace_id: u64,
    /// The Browser record the window opens on.
    pub record: String,
    /// The device the workspace lists while the environment runs, and no
    /// other: certified by this host as a member device of the person the
    /// workspace belongs to, for [`DEVICE_LIFETIME`].
    pub device: DeviceCert,
    /// That device's signing key, the only copy: the environment holds it.
    pub secret: [u8; 32],
    /// Where the session joins the workspace: this host's endpoint.
    pub relay: EndpointAddr,
    /// The person the workspace belongs to.
    pub user_public_key: [u8; 32],
    /// Who pays for the workspace's VMs, as this host knows it.
    pub account: String,
}

impl Material {
    /// This device's signing key.
    ///
    /// # Errors
    ///
    /// The secret is not a key.
    pub fn key(&self) -> Result<SigningKey, String> {
        let bytes: [u8; 32] = self
            .secret
            .as_slice()
            .try_into()
            .map_err(|_| "an environment's secret is a 32-byte key".to_owned())?;
        Ok(SigningKey::from_bytes(&bytes))
    }
}

/// A browser environment that is up, and the window it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The device of the workspace the session in the VM joined as.
    pub device: [u8; 32],
    /// The provider token the window publishes under (`browser@42`): what a
    /// page action names in place of the app.
    pub provider: String,
}

/// The service that starts and stops a workspace's browser environments.
///
/// The hosted-VM service's client implements this: it asks the placer for the
/// host of `<workspace>/browser`, asks that host's member device to start the
/// environment with the [`Material`], and answers once the session in it
/// reports its window live.
pub trait BrowserEnvironments: Send + Sync {
    /// Start the workspace's environment with `material`, and answer its
    /// window once it is live.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: no host had room, or the host
    /// would not start it.
    fn start(&self, material: &Material) -> Result<Window, String>;

    /// The workspace's environment stopped, or must: its device is forgotten
    /// and its window no longer exists.
    fn stop(&self, workspace_id: u64);
}

/// What a host counts page actions against: its plans and the account's
/// standing.
pub trait BrowserMeter: Send + Sync {
    /// Whether `account` may run one more page action on a Browser this host
    /// placed.
    ///
    /// # Errors
    ///
    /// The typed refusal the action is answered with, which is the VM path's
    /// ([`Refusal`]).
    fn admit(&self, account: &str) -> Result<(), Refusal>;

    /// One page action ran for `account`.
    fn record(&self, account: &str);
}

/// A host whose plans file sells nothing: it counts nothing and refuses
/// nothing, as it does for VMs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unmetered;

impl BrowserMeter for Unmetered {
    fn admit(&self, _: &str) -> Result<(), Refusal> {
        Ok(())
    }

    fn record(&self, _: &str) {}
}

/// What placing needs of the host itself, once it runs: the certificate it
/// makes environments' devices with, where it is reached, and the workspaces
/// it serves.
pub struct Authority {
    pub user_public_key: [u8; 32],
    /// The person's stand-in approver for this host
    /// ([`tilo_user_identity::prepare_stand_in_cert`]): what an environment's
    /// device is certified with, as a member device of the workspace's person
    /// for as long as the environment may run. A host the person did not give
    /// one holds no certificate to make an environment a device, and placing
    /// one is refused.
    pub cert: DeviceCert,
    /// That stand-in's own signing key.
    pub key: SigningKey,
    /// Where the session in the environment joins the workspace.
    pub relay: EndpointAddr,
    pub workspaces: Option<crate::workspaces::Workspaces>,
    /// The runtime the workspaces run on: listing a device writes membership
    /// there.
    pub runtime: tokio::runtime::Handle,
}

/// One workspace's window, while its environment runs.
struct Open {
    window: Window,
    /// The device the workspace lists for it, forgotten when it stops.
    device: DeviceCert,
}

/// The Browser this host places for its workspaces, and the page actions it
/// hands to the windows.
pub struct HostedBrowsers {
    environments: Box<dyn BrowserEnvironments>,
    meter: Box<dyn BrowserMeter>,
    /// Who pays for the workspaces' VMs, as this host knows it.
    account: String,
    /// The windows up now, by workspace.
    open: Mutex<BTreeMap<u64, Open>>,
    authority: Mutex<Option<Authority>>,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "each method holds one guard for one answer; placing holds it so two opens make one environment"
)]
impl HostedBrowsers {
    /// A host that places Browser through `environments`, counting page
    /// actions of `account` in `meter`.
    pub fn new(
        environments: Box<dyn BrowserEnvironments>,
        meter: Box<dyn BrowserMeter>,
        account: String,
    ) -> Self {
        Self {
            environments,
            meter,
            account,
            open: Mutex::default(),
            authority: Mutex::default(),
        }
    }

    /// Name the host's identity and workspaces, once it runs: needed to
    /// certify an environment's device and to list it in its workspace.
    pub fn serve_from(&self, authority: Authority) {
        *self.authority.lock_recover() = Some(authority);
    }

    /// The window `workspace_id`'s Browser is in, when one is up.
    #[must_use]
    pub fn window(&self, workspace_id: u64) -> Option<Window> {
        self.open
            .lock_recover()
            .get(&workspace_id)
            .map(|open| open.window.clone())
    }

    /// Where `workspace_id`'s page action goes: to the device holding its
    /// window, once the meter admits it for the workspace's account; refused
    /// with the host's typed refusal otherwise. `None` for an action that is
    /// not a page action of that window, which this host serves itself.
    #[must_use]
    pub fn route(
        &self,
        workspace_id: u64,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        let RemoteActionMessage::Invoke(request) = message else {
            return None;
        };
        let (provider, device) = {
            let open = self.open.lock_recover();
            let up = open.get(&workspace_id)?;
            (up.window.provider.clone(), up.window.device)
        };
        if !page_action(&request.target, &provider) {
            return None;
        }
        Some(match self.meter.admit(&self.account) {
            Ok(()) => {
                self.meter.record(&self.account);
                crate::app_serving::Routing::Device(device)
            }
            Err(refusal) => crate::app_serving::Routing::Refused(ActionHostError::Refused {
                reason: refusal.to_reason(),
            }),
        })
    }

    /// The UUID of the workspace this host carries as `workspace_id`, when it
    /// serves one: what a page action and an environment's device name it by.
    #[must_use]
    pub fn workspace_name(&self, workspace_id: u64) -> Option<String> {
        let authority = self.authority.lock_recover();
        let authority = authority.as_ref()?;
        let workspaces = authority.workspaces.as_ref()?;
        authority
            .runtime
            .block_on(workspaces.workspace_identity(workspace_id))
    }

    /// Place `workspace_id`'s Browser — the workspace whose UUID is
    /// `workspace` — on the record `record`, and answer the window once it is
    /// live. A Browser already up is the same window: a second open attaches
    /// to it.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: this host is not running, no device
    /// could be certified, or the environment would not start.
    pub fn place(
        &self,
        workspace: &str,
        workspace_id: u64,
        record: &str,
    ) -> Result<Window, String> {
        let mut open = self.open.lock_recover();
        if let Some(up) = open.get(&workspace_id) {
            return Ok(up.window.clone());
        }
        let (device, material) = {
            let authority = self.authority.lock_recover();
            let authority = authority.as_ref().ok_or("this host is not running yet")?;
            let (device, secret) = certified(&authority.cert, &authority.key)?;
            let material = Material {
                workspace: workspace.to_owned(),
                workspace_id,
                record: record.to_owned(),
                device: device.clone(),
                secret,
                relay: authority.relay.clone(),
                user_public_key: authority.user_public_key,
                account: self.account.clone(),
            };
            (device, material)
        };
        let window = self.environments.start(&material)?;
        // The workspace lists the environment's device while it runs, so the
        // window's actions reach it and its own session is admitted.
        self.list(workspace_id, &device, true);
        open.insert(
            workspace_id,
            Open {
                window: window.clone(),
                device,
            },
        );
        Ok(window)
    }

    /// The workspace's environment stopped: forget its device and its window.
    pub fn stopped(&self, workspace_id: u64) {
        let stopped = self.open.lock_recover().remove(&workspace_id);
        let Some(stopped) = stopped else {
            return;
        };
        self.environments.stop(workspace_id);
        self.list(workspace_id, &stopped.device, false);
    }

    /// List (or forget) the environment's device in the workspace it serves,
    /// as a member device of the person the workspace belongs to.
    fn list(&self, workspace_id: u64, device: &DeviceCert, listed: bool) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        let Some(workspaces) = &authority.workspaces else {
            return;
        };
        let listed = authority.runtime.block_on(workspaces.list_device(
            workspace_id,
            device.device_public_key,
            listed,
        ));
        if let Err(error) = listed {
            tracing::warn!(
                %error,
                device = %hex::encode(device.device_public_key),
                "a browser environment's device was not listed in its workspace"
            );
        }
    }
}

/// A fresh device for an environment, certified by the host's stand-in as a
/// member device of the workspace's person, for as long as the environment
/// may run.
fn certified(stand_in: &DeviceCert, key: &SigningKey) -> Result<(DeviceCert, [u8; 32]), String> {
    if !stand_in.capabilities.stand_in {
        return Err(
            "this host holds no stand-in certificate for the workspace's person, so it cannot \
             make an environment a device"
                .to_owned(),
        );
    }
    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
    let device = SigningKey::from_bytes(&secret);
    let now = tilo_clock::unix_now_ns();
    let mut minted = DeviceCert {
        user_public_key: stand_in.user_public_key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: DEVICE_LABEL.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: Some(
            now.saturating_add(u64::try_from(DEVICE_LIFETIME.as_nanos()).unwrap_or(u64::MAX)),
        ),
        signature: [0; 64],
        issuer: Some(Box::new(stand_in.clone())),
    };
    sign_device_cert(&mut minted, key).map_err(|error| error.to_string())?;
    Ok((minted, secret))
}

/// Whether `target` is an action of the page `provider`'s window publishes:
/// the window's provider ([`Window::provider`]) and the page's resource.
#[must_use]
pub fn page_action(target: &RemoteActionTarget, provider: &str) -> bool {
    target.provider == provider && logical_resource_id(&target.address.resource) == PAGE_RESOURCE
}

/// [`Reason::NoPaymentMethod`] for an account whose period's free actions are
/// used up, with the one step that would let the next action through.
#[must_use]
pub fn no_payment_method(free: u64) -> Refusal {
    Refusal {
        reason: Reason::NoPaymentMethod,
        message: format!(
            "This period's {free} free Browser actions are used up. A card continues them."
        ),
        next: vec![NextStep {
            kind: NextStepKind::AddPaymentMethod,
            url: None,
        }],
    }
}

/// What a host that places no Browser refuses an open of it with: words that
/// say why, since a Browser window needs an environment this host has not.
#[must_use]
pub fn unplaced() -> String {
    "Browser does not open here: this host places no browser environment, and a Browser \
     window needs one. Open it on your own device instead."
        .to_owned()
}

/// The answer to an open of Browser that ran in `workspace_id`: `message`
/// with the window placed, or the refusal in its place.
///
/// The open answered once its record was made; this places the environment on
/// that record, and the open answers once the window is live.
#[must_use]
pub fn placed(
    browsers: &HostedBrowsers,
    workspace_id: u64,
    invocation: tilo_actions::InvocationId,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    let Some(record) = opened_record(&message) else {
        return message;
    };
    let placement = browsers
        .workspace_name(workspace_id)
        .ok_or_else(|| "this host serves no such workspace".to_owned())
        .and_then(|workspace| browsers.place(&workspace, workspace_id, &record));
    match placement {
        Ok(_) => message,
        Err(reason) => refused(invocation, format!("Browser did not open: {reason}")),
    }
}

/// The answer to `invocation` refused with `reason`.
#[must_use]
pub fn refused(invocation: tilo_actions::InvocationId, reason: String) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
        invocation,
        status: tilo_actions::remote::ReceiptStatus::Rejected {
            error: ActionHostError::Refused { reason },
        },
    })
}

/// Whether `request` opens Browser (`app.so.tilo.browser#open`): the open the
/// plane places an environment after.
#[must_use]
pub fn opens_browser(request: &InvokeRequest) -> bool {
    provider_workspace(&request.target.provider).is_some()
        && request.target.address.resource == format!("{APPLICATION_RESOURCE_PREFIX}{APP}")
        && request.target.address.action == APPLICATION_OPEN_ACTION
}

/// The record an answered [`opens_browser`] open made, when it made one: the
/// record its answer names.
#[must_use]
pub fn opened_record(message: &RemoteActionMessage) -> Option<String> {
    let RemoteActionMessage::Receipt(receipt) = message else {
        return None;
    };
    let tilo_actions::remote::ReceiptStatus::Completed { output } = &receipt.status else {
        return None;
    };
    output
        .value
        .as_ref()
        .and_then(|value| value.get("record"))
        .and_then(|record| record.as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::ActionOutput;

    fn page(provider: &str, resource: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new(resource, "read-page"),
        }
    }

    #[test]
    fn a_windows_page_action_is_the_one_addressed_to_that_window() {
        assert!(page_action(
            &page("browser@42", PAGE_RESOURCE),
            "browser@42"
        ));
        assert!(
            !page_action(&page("browser@42", PAGE_RESOURCE), "browser@7"),
            "another window's page is another device's action"
        );
        assert!(
            !page_action(&page("browser@42", "browser.tab"), "browser@42"),
            "the record's own actions are the host's, not the window's"
        );
    }

    #[test]
    fn an_open_of_browser_is_the_one_that_places_an_environment() {
        use tilo_actions::remote::{APPLICATION_OPEN_ACTION, workspace_provider};
        let open = |resource: &str, action: &str| InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            on_behalf_of: None,
            target: RemoteActionTarget {
                provider: workspace_provider("ws"),
                address: ActionAddress::new(resource, action),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        assert!(opens_browser(&open(
            "app.so.tilo.browser",
            APPLICATION_OPEN_ACTION
        )));
        assert!(!opens_browser(&open(
            "app.so.tilo.notes",
            APPLICATION_OPEN_ACTION
        )));
    }

    #[test]
    fn an_open_that_made_a_record_answers_it_by_its_id() {
        let made = RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
            invocation: tilo_actions::InvocationId::new(),
            status: tilo_actions::remote::ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(
                        serde_json::json!({"provider": "record:so.tilo.browser:abc", "record": "abc"}),
                    ),
                    resources: Vec::new(),
                },
            },
        });
        assert_eq!(opened_record(&made).as_deref(), Some("abc"));
        let refused = refused(tilo_actions::InvocationId::new(), unplaced());
        assert_eq!(opened_record(&refused), None);
    }

    #[test]
    fn a_host_that_places_nothing_says_why_instead_of_naming_an_unknown_app() {
        let words = unplaced();
        assert!(words.contains("browser environment"), "{words}");
        assert!(!words.contains("no app"), "{words}");
    }
}
