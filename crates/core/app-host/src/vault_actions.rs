//! The vault's own actions a tilolang app calls by address: keeping a code generator's seed and
//! forgetting it, and the reading that shows the code of this moment where the seed is.
//!
//! The app is a package, so no code of the vault is its: the action runs here, in the process
//! that holds the workspace's vault, through the device's provider registry ([`register`]). The
//! seed never leaves. Keeping and forgetting a seed are the ordinary vault writes, made here, as
//! the caller; the code is a reading the window follows, so it writes no change, no use and no
//! history.
//!
//! A grant serves the person and their agents alike: the device's dispatch admits the call
//! against the workspace's policy under this package before it reaches here, and a call that
//! changes something is a change of its caller's.

use std::sync::Arc;

use crate::services::{Caller as PublishedCaller, PublishedResources, Reading, ResourceWatch};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, pending,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate, ResourceSpec};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::providers::Reach;

/// The provider the vault's own actions are registered under on the device.
pub const PROVIDER: &str = "so.tilo.vault";
/// The package they are published as, which an app's code names (`use so.tilo.vault as vault`).
pub const PACKAGE: &str = "so.tilo.vault";
/// The resource the actions are called on: an item of the workspace's vault.
pub const RESOURCE: &str = "vault.item";
/// What the package is called where a catalogue shows it, which is also the resource row's
/// label: the manifest names it the same.
const PACKAGE_TITLE: &str = "Vault";
/// Keep a code generator's seed, or change the one an item names.
pub const KEEP: &str = "keep";
/// Forget the code generator an item names.
pub const FORGET: &str = "forget";

/// The vault item as a declared type: what the package's actions are offered on.
#[derive(tilo_resource::Row)]
#[row(kind = "vault.item", label = "Vault")]
pub struct VaultItem;

/// The vault's own actions, declared as any app declares them: the device that holds the
/// workspace's vault answers them.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.vault", app = "so.tilo.vault")]
pub enum VaultAction {
    /// Keep a seed
    ///
    /// Keeps the seed a service gave for an account, or changes the seed and details of the
    /// item given. Answers the item's reference, `vault.item:<id>`.
    #[effect(kind = "outside", returns = String)]
    Keep {
        #[param("The item to change (`vault.item:…`); a new one when absent")]
        item: Option<String>,
        #[param("The service the account is for")]
        issuer: String,
        #[param("Who it signs in as")]
        label: Option<String>,
        #[param(secret, "The seed the service gave")]
        secret: String,
        #[param("The hash the codes are made with")]
        algorithm: Option<String>,
        #[param("How many digits a code has")]
        digits: Option<i64>,
        #[param("How many seconds a code stands")]
        period: Option<i64>,
    },
    /// Forget a seed
    ///
    /// Removes a code generator and its seed.
    #[effect(kind = "outside")]
    Forget {
        #[param("The item to remove (`vault.item:…`)")]
        item: String,
    },
}

/// The package the vault's actions are published as, by their contracts.
fn resources() -> Vec<ResourceSpec> {
    let mut spec = tilo_resource::typed_resource::<VaultItem>(RESOURCE, PACKAGE_TITLE);
    spec.app_id = Some(PACKAGE.to_owned());
    vec![tilo_resource::effect::offer::<VaultAction>(spec)]
}

/// Serve the vault's own actions on this device from now on, decided by the grants of
/// [`PACKAGE`].
///
/// # Panics
/// Called outside a reactor: a call reaches the vault on the reactor it was registered on.
pub fn register(workspaces: &Workspaces) {
    let runtime = tokio::runtime::Handle::current();
    workspaces.providers().provide(
        PROVIDER,
        PACKAGE,
        Reach::ThisDevice,
        Arc::new(VaultActions {
            workspaces: workspaces.clone(),
            runtime,
            resources: resources(),
        }),
    );
}

/// The vault's own actions, as the device serves them.
struct VaultActions {
    workspaces: Workspaces,
    /// Where a call reaches the vault: opening it reads the workspace, which is asynchronous.
    runtime: tokio::runtime::Handle,
    resources: Vec<ResourceSpec>,
}

impl ActionProvider for VaultActions {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.resources.clone()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::published(
            PROVIDER,
            &self.resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let address = &request.target.address;
        if address.resource != RESOURCE {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let action = address.action.clone();
        if !matches!(action.as_str(), KEEP | FORGET) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None | InvokeArgument::Text(_) => serde_json::Map::new(),
        };
        let Some(workspace) = caller.workspace().map(str::to_owned) else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let by = caller.acting().clone();
        let item = arguments
            .get("item")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let id = request.invocation;
        let workspaces = self.workspaces.clone();
        let (complete, answer) = pending();
        self.runtime.spawn(async move {
            let outcome = match action.as_str() {
                KEEP => {
                    vault_keep(&workspaces, (&workspace, &by), item.as_deref(), &arguments).await
                }
                FORGET => match item {
                    Some(item) => vault_forget(&workspaces, (&workspace, &by), &item)
                        .await
                        .map(|()| serde_json::Value::Null),
                    None => Err(ActionCallError::InvalidRequest {
                        reason: "forget names the item to remove".to_owned(),
                    }),
                },
                _ => Err(ActionCallError::UnknownAction),
            };
            complete.complete(match outcome {
                Ok(value) => Ok(InvocationState::new(
                    id,
                    InvocationUpdate::Succeeded {
                        invocation: id,
                        output: ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                )
                .receipt()),
                Err(error) => Err(error),
            });
        });
        answer
    }

    /// Nothing to follow later: a call of these actions answers before it returns.
    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    /// No standing invocation to stop: each call has already answered.
    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// The code of `code`, as the app reads it.
fn code_answer(code: &accounts::vault_items::Code) -> serde_json::Value {
    serde_json::json!({
        "code": code.digits(),
        "until": iso_text(code.until_unix),
        "period": code.params.period,
        "digits": code.params.digits,
        "algorithm": code.params.algorithm.word(),
    })
}

/// Keep the seed `arguments` names for a code generator, answering its reference: the item
/// named changes, or a new one is kept.
async fn vault_keep(
    workspaces: &Workspaces,
    (workspace, by): (&str, &tilo_action_policy::Acting),
    item: Option<&str>,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, ActionCallError> {
    let said = |name: &str| {
        arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let secret = said("secret");
    let changing = item.map(item_id).transpose()?;
    if secret.is_empty() && changing.is_none() {
        return Err(ActionCallError::InvalidRequest {
            reason: "keeping a seed names the seed the service gave".to_owned(),
        });
    }
    let issuer = said("issuer");
    if issuer.is_empty() {
        return Err(ActionCallError::InvalidRequest {
            reason: "keeping a seed names the service the account is for".to_owned(),
        });
    }
    let label = said("label");
    let algorithm = said("algorithm");
    let digits = arguments
        .get("digits")
        .and_then(serde_json::Value::as_u64)
        .and_then(|digits| u8::try_from(digits).ok())
        .unwrap_or(accounts::vault_items::CODE_DIGITS);
    let period = arguments
        .get("period")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(accounts::vault_items::CODE_SECS);
    let facts = workspaces
        .changing_vault(workspace, (Some(by), None), |items| {
            let code = accounts::vault_items::CodeParams::checked(
                if algorithm.is_empty() {
                    "SHA1"
                } else {
                    algorithm.as_str()
                },
                digits,
                period,
            )?;
            match &changing {
                // A seed left out keeps the one the item already holds: the service's key
                // never leaves the vault, so changing an account's details cannot show it.
                Some(id) if secret.is_empty() => items.retune(id, &label, code),
                _ => {
                    let item = accounts::vault_items::NewItem::new(
                        accounts::vault_items::ItemKind::CodeGenerator,
                        Vec::new(),
                        label,
                        secret,
                    )
                    .with_code(code);
                    match &changing {
                        Some(id) => items.keep(Some(id), item, &issuer),
                        None => items.add(item),
                    }
                }
            }
        })
        .await
        .map_err(ActionCallError::refused)?;
    Ok(serde_json::Value::String(format!(
        "{RESOURCE}:{}",
        facts.id.as_str()
    )))
}

/// Forget the code generator the item `item` names.
async fn vault_forget(
    workspaces: &Workspaces,
    (workspace, by): (&str, &tilo_action_policy::Acting),
    item: &str,
) -> Result<(), ActionCallError> {
    let id = item_id(item)?;
    workspaces
        .changing_vault(workspace, (Some(by), None), |items| {
            if items.remove(&id) {
                Ok(())
            } else {
                Err(accounts::vault_items::ItemError::NotFound)
            }
        })
        .await
        .map_err(ActionCallError::refused)
}

/// The item a reference names (`vault.item:<id>`).
fn item_id(reference: &str) -> Result<accounts::vault_items::ItemId, ActionCallError> {
    reference
        .strip_prefix(&format!("{RESOURCE}:"))
        .and_then(accounts::vault_items::ItemId::parse)
        .ok_or_else(|| ActionCallError::InvalidRequest {
            reason: "an item is named by its reference, `vault.item:<id>`".to_owned(),
        })
}

/// The vault's items as the session publishes them: the reading of `vault.item:<id>` is the
/// code of this moment, its countdown and how the service makes it. A window that shows an app's
/// accounts follows them, so the code reaches the view as a reading, never as a record and never
/// as a change; reading it here writes nothing down and keeps no use.
pub struct VaultPublished {
    workspaces: Workspaces,
    runtime: tokio::runtime::Handle,
    /// The watches this provider minted, by the number the table minted for it.
    watches: std::sync::Mutex<Vec<(u64, tokio::task::AbortHandle)>>,
    next: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for VaultPublished {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VaultPublished")
    }
}

impl VaultPublished {
    /// The vault's readings, served on `runtime` from `workspaces`.
    ///
    /// # Panics
    /// Called outside a reactor: a watch runs on the reactor it was made on.
    #[must_use]
    pub fn new(workspaces: Workspaces, runtime: tokio::runtime::Handle) -> Self {
        Self {
            workspaces,
            runtime,
            watches: std::sync::Mutex::new(Vec::new()),
            next: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// The number a watch is minted under.
    fn mint(&self) -> u64 {
        self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

/// The reading of `resource` now, or why the caller is not granted it.
async fn reading(
    workspaces: &Workspaces,
    caller: &PublishedCaller,
    resource: &str,
) -> Result<String, String> {
    // The table that publishes this serves the apps this device started in its workspace, and
    // the catalogue it publishes is the grant: an item answers its code there, never its seed.
    if caller.workspace.is_empty() {
        return Err("The call names no workspace".to_owned());
    }
    let code = workspaces.vault_code(&caller.workspace, resource).await?;
    serde_json::to_string(&code_answer(&code)).map_err(|error| error.to_string())
}

impl PublishedResources for VaultPublished {
    fn snapshot(
        &self,
        caller: &PublishedCaller,
        resource: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let workspaces = self.workspaces.clone();
        let caller = caller.clone();
        let resource = resource.to_owned();
        self.runtime.spawn(async move {
            let reading = reading(&workspaces, &caller, &resource).await;
            let _ = answer.send(reading.map(|reading| (1, Some(reading.into_bytes()))));
        });
        answered
    }

    fn watch(
        &self,
        caller: &PublishedCaller,
        resource: &str,
        sink: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        let caller = caller.clone();
        let resource = resource.to_owned();
        let workspaces = self.workspaces.clone();
        let watch = self.mint();
        let named = resource;
        let task = self.runtime.spawn(async move {
            // The code changes every period; a reading a second keeps its countdown right, and
            // the first one arrives at once.
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            let mut revision = 0_u64;
            loop {
                tick.tick().await;
                let said = match reading(&workspaces, &caller, &named).await {
                    Ok(reading) => Some(reading.into_bytes()),
                    Err(_) => None,
                };
                revision = revision.saturating_add(1);
                let event = runtime_collab::wire::Event::Reading {
                    resource: named.clone(),
                    revision,
                    reading: said,
                };
                if sink.send(event).is_err() {
                    return;
                }
            }
        });
        self.watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((watch, task.abort_handle()));
        Ok(ResourceWatch::new(watch))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        let mut watches = self
            .watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = watches.iter().position(|(id, _)| *id == watch.id()) {
            let (_, task) = watches.remove(at);
            drop(watches);
            task.abort();
        }
    }

    fn invoke(
        &self,
        _caller: &PublishedCaller,
        _resource: &str,
        _action: &str,
        _arguments: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let _ = answer.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: "A vault item's actions are asked as the vault's own".to_owned(),
        });
        answered
    }
}

/// The instant `unix_secs` as RFC 3339 text in UTC: what a `Time` reads as.
fn iso_text(unix_secs: u64) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let secs = unix_secs % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's `civil_from_days`). Every operand
    // is bounded far below the integer's end, so the saturating forms take nothing away.
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .saturating_sub(doe / 1_460)
        .saturating_add(doe / 36_524)
        .saturating_sub(doe / 146_096)
        / 365;
    let y = yoe.saturating_add(era.saturating_mul(400));
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe / 4)
            .saturating_sub(yoe / 100),
    );
    let mp = doy.saturating_mul(5).saturating_add(2) / 153;
    let d = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(1);
    let m = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    };
    let y = if m <= 2 { y.saturating_add(1) } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    /// What code compiles its calls against (`apps/tl/vault`) is what the device registers: the
    /// manifest's id and name, and the actions the surface describes.
    #[test]
    fn the_committed_package_is_what_the_device_registers() {
        let registered = tilo_actions::remote::ProvidedPackage::of(&super::resources())
            .expect("the vault publishes a package");
        let manifest: toml::Table =
            toml::from_str(include_str!("../../../../apps/tl/vault/tilo-app.toml"))
                .expect("the manifest reads");
        assert_eq!(manifest["package"]["id"].as_str(), Some(super::PACKAGE));
        assert_eq!(registered.app, super::PACKAGE);
        assert_eq!(
            manifest["package"]["name"].as_str(),
            Some(registered.title.as_str())
        );
        let committed: serde_json::Value =
            serde_json::from_str(include_str!("../../../../apps/tl/vault/surface.json"))
                .expect("the description reads");
        let committed: Vec<tilo_actions::surface::Surface> =
            serde_json::from_value(committed["resources"].clone())
                .expect("the description is surfaces");
        let published: Vec<_> = registered
            .resources
            .into_iter()
            .map(|resource| {
                assert_eq!(resource.address, super::RESOURCE);
                resource.surface
            })
            .collect();
        assert_eq!(committed, published);
    }
}
