//! The two device actions this computer's daemon publishes: serving a ticket for a new computer
//! (`tilo device add`), and logging in with a ticket another of the person's devices issued
//! (`tilo login`). Both need what only the daemon has: the device's endpoint, its session, and,
//! on a computer that holds the person's root, that root.
//!
//! They are ordinary actions (`tilo.device`'s `devices#issue` and `devices#logIn`), answered by
//! the daemon's action provider, beside the person's devices themselves ([`manage`]): listing,
//! renaming, revoking and updating them. On a server the same provider answers the machine's own
//! controls (`server#status`, `server#restart` and the rest, [`SERVER`]) and its turn among its
//! person's servers in an update ([`UPDATES`]). A ticket's certify action is answered by the device action
//! service the daemon's one iroh router already serves (`tilo-actions/1`), as the one call it
//! admits from a computer with no certificate yet, so no second endpoint or service is made.

mod agent_key;
mod manage;

pub(crate) use manage::OnThisDevice;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Where the daemon stages the update approvals its root unit carries into place.
pub(crate) const APPROVALS_STAGING: &str = "approvals-next";

use super::workspaces::ServiceWorkspaces;
use crate::device::issuing::{Role, TicketServer};
use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType,
};
use tilo_device_identity::login::{
    ALREADY_A_PERSONS, ANSWER_SIGN_IN, APPROVE, BACKUP, CERTIFY_KEY, DEV_ID, Device, Direction,
    FOUND, IDENTITIES, ISSUE, LOG_IN, OPEN_VAULT, PENDING, PROTECT_IDENTITY, PROVIDER, Place,
    RECOVERY_WORDS, RELEASE_HALF, REMOVE_IDENTITY, RENAME, REPORTED, RESOURCE, RESTORE, REVOKE,
    Report, TICKET_LIFETIME_NS, Ticket, UPDATE, USE_IDENTITY,
};
use tilo_identity::store::SigningIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::DeviceCert;

/// How long the issuer keeps serving once the computer is certified, for it to read its answer.
const TELL_WAIT: Duration = Duration::from_secs(30);
/// How long the issuer keeps serving after it told the computer how its call ended: an answer
/// that ended with its connection is asked for again, and answered where the call is.
const TELL_GRACE: Duration = Duration::from_secs(5);
/// What the device actions' catalogue page is revised by: they are fixed, so one revision.
const REVISION: &str = "tilo.device/1";
/// What this device is ([`device_info`]).
pub(crate) const INFO: &str = "info";
/// The resource the update actions are on: a server's turn among its person's
/// servers, which the automatic update path asks its own daemon for.
pub(crate) const UPDATES: &str = "updates";
/// May this machine restart into the staged image now? Arguments: `target`.
pub(crate) const PREPARE: &str = "prepare";
/// This machine booted `image` and passed its health verdict. Arguments: `image`.
pub(crate) const BOOTED: &str = "booted";
/// Ask the person's devices for this server's rollback floor to move.
pub(crate) const FLOOR: &str = "floor";
/// The resource the machine controls are on: what a server's person, and the administrators of
/// the workspaces it serves, ask the machine itself to do ([`crate::device::keep::operations::ACTIONS`]).
pub(crate) const SERVER: &str = "server";
/// What a caller who may not control this machine reads.
const NOT_YOURS: &str = "Only the person this server belongs to, and the administrators of the \
                         workspaces it serves, control it.";

/// The daemon's `tilo.device` provider, as the catalogue lists it beside the workspace's apps: one
/// running service under each app its rows name (`devices`, `server`, `updates`), so a call that
/// names any of them by its app (`server#status`, `updates#prepare`) is resolved to it.
#[must_use]
pub(crate) fn providers(workspace: &str) -> [CatalogueProvider; 3] {
    [RESOURCE, SERVER, UPDATES].map(|app| CatalogueProvider {
        provider: PROVIDER.to_owned(),
        app_id: app.to_owned(),
        workspace: workspace.to_owned(),
        browsable: true,
        ..CatalogueProvider::default()
    })
}

/// The `tilo.device` page `query` asks for: the `devices` resource and its actions, resolved and
/// invoked the way an app's are. Founding and restoring a person stay among them on a person's
/// device, answered with [`ALREADY_A_PERSONS`], so whatever asks a device to found its person
/// (a dev session's start, `tilo login --new-identity`) is told it has one rather than that the
/// action is gone.
#[must_use]
pub(crate) fn catalogue(query: &CatalogueQuery) -> CataloguePage {
    catalogue_of(query, |_| true)
}

/// [`catalogue`] with only what a device answers before its person's services run: what it is,
/// logging it in with a ticket, making it a new or a restored person's first device, and what
/// its key does.
#[must_use]
pub(crate) fn device_only_catalogue(query: &CatalogueQuery) -> CataloguePage {
    catalogue_of(query, |action| {
        matches!(action, INFO | LOG_IN | FOUND | RESTORE)
            || matches!(action, super::records::SET | super::records::REMOVE)
            || super::key_use::asked_of_the_key(action)
    })
}

/// The `devices` resource and those of its actions `keep` keeps.
#[expect(
    clippy::too_many_lines,
    reason = "every device action's row, read top to bottom"
)]
fn catalogue_of(query: &CatalogueQuery, keep: impl Fn(&str) -> bool) -> CataloguePage {
    let resource = CatalogueResource {
        id: RESOURCE.to_owned(),
        kind: "device".to_owned(),
        label: "Devices".to_owned(),
        presentation: None,
    };
    let row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: RESOURCE.to_owned(),
        parent_resource: String::new(),
        resource: resource.clone(),
        action,
        ..CatalogueRow::default()
    };
    let mut rows = vec![
        row(None),
        row(Some(descriptor(
            INFO,
            "What this device is: its key, how it is listed, and the people whose \
                 certificates name it.",
            &[],
        ))),
        row(Some(descriptor(
            UPDATE,
            "Update the device's apps and system, as `tilo update` does on it.",
            &[ActionParam::optional(
                "device",
                ParamType::Text,
                "The device's key: another of the person's devices carries it out itself.",
            )],
        ))),
        row(Some(descriptor(
            PENDING,
            "See the browser a sign-in ticket on another host waits for, and what signing it \
                 in makes it.",
            &[ActionParam::required(
                "ticket",
                ParamType::Text,
                "The ticket the browser's prompt carried.",
            )],
        ))),
        row(Some(descriptor(
            APPROVE,
            "Sign the browser's certificate, so it enters the person's workspace.",
            &[ActionParam::required(
                "ticket",
                ParamType::Text,
                "The ticket the browser's prompt carried.",
            )],
        ))),
        row(Some(descriptor(
            REPORTED,
            "Tell the browser that showed the ticket what this computer did as it logged in.",
            &[
                ActionParam::required(
                    "ticket",
                    ParamType::Text,
                    "The ticket this computer logged in with.",
                ),
                ActionParam::required("step", ParamType::Text, "What it did."),
            ],
        ))),
        row(Some(descriptor(
            LOG_IN,
            "Become one of the person's devices with a ticket another of their computers printed, \
             or, on a computer that holds the person's identity, log in at a server by its line.",
            &[
                ActionParam::optional("ticket", ParamType::Text, "The ticket, as it was printed."),
                ActionParam::optional(
                    "server",
                    ParamType::Text,
                    "A server's line from its web-host.json, instead of a ticket.",
                ),
                ActionParam::optional("agent", ParamType::Text, "The agent that runs here."),
                ActionParam::optional(
                    "label",
                    ParamType::Text,
                    "How the devices list this computer.",
                ),
            ],
        ))),
        row(Some(descriptor(
            FOUND,
            "Make a new person whose first device this computer is, with their own workspace.",
            &[ActionParam::optional(
                "name",
                ParamType::Text,
                "What the person is called: the computer account's name when left out.",
            )],
        ))),
        row(Some(descriptor(
            DEV_ID,
            "The id a development build of an app runs under on this device, which a build \
             compiles the app as before the device installs it.",
            &[ActionParam::required(
                "app",
                ParamType::Text,
                "The app's own id, as its sources declare it.",
            )],
        ))),
        row(Some(descriptor(
            RESTORE,
            "Bring a person back on this computer from their recovery words or a backup.",
            &[
                ActionParam::optional("words", ParamType::Text, "The 24 recovery words."),
                ActionParam::optional(
                    "backup",
                    ParamType::Text,
                    "A backup Tilo wrote, its bytes in hexadecimal.",
                ),
                ActionParam::optional(
                    "passphrase",
                    ParamType::Text,
                    "The passphrase the backup was sealed with.",
                ),
            ],
        ))),
        row(Some(descriptor(
            RECOVERY_WORDS,
            "The 24 words that bring the person back on a new computer, from the root kept here.",
            &[],
        ))),
        row(Some(descriptor(
            CERTIFY_KEY,
            "Certify a key with the root kept here: for one agent in one workspace, for a \
             computer that reaches one workspace alone, for a device's boot key, or, naming \
             nothing, as an agent's key in every workspace of the person's. The key is listed \
             among their devices.",
            &[
                ActionParam::required("key", ParamType::Text, "The key, as hex."),
                ActionParam::optional(
                    "for",
                    ParamType::Object,
                    "What the key is for: {\"agent\": {\"workspace\", \"agent\"}}, \
                     {\"workspace\": id}, or \"boot\".",
                ),
                ActionParam::optional("name", ParamType::Text, "The name it is listed by."),
                ActionParam::optional(
                    "request",
                    ParamType::Text,
                    "A boot's request, the key's proof, its bytes in hex: with \"boot\".",
                ),
                ActionParam::optional(
                    "device",
                    ParamType::Text,
                    "The device a boot key belongs to, as hex: with \"boot\".",
                ),
                ActionParam::optional(
                    "approval_digest",
                    ParamType::Text,
                    "The unattended boot chain to accept, 32 bytes in hex.",
                ),
            ],
        ))),
        row(Some(descriptor(
            RELEASE_HALF,
            "Open a boot's sealed half and seal it to the boot's key.",
            &[
                ActionParam::required(
                    "cert",
                    ParamType::Text,
                    "The boot key's certificate, its bytes in hex.",
                ),
                ActionParam::required(
                    "sealed_half",
                    ParamType::Text,
                    "The half as it was sealed to your devices, its bytes in hex.",
                ),
                ActionParam::required(
                    "request_digest",
                    ParamType::Text,
                    "The boot's request, as 32 bytes in hex.",
                ),
                ActionParam::required(
                    "ephemeral",
                    ParamType::Text,
                    "The boot's ephemeral key, as 32 bytes in hex.",
                ),
            ],
        ))),
        row(Some(descriptor(
            ANSWER_SIGN_IN,
            "Sign a site's sign-in challenge and certify the key it asked to sign in with.",
            &[
                ActionParam::required("audience", ParamType::Text, "The site asking."),
                ActionParam::required(
                    "challenge",
                    ParamType::Text,
                    "The challenge it gave, as 32 bytes in hex.",
                ),
            ],
        ))),
        row(Some(descriptor(
            BACKUP,
            "A backup of the person's root kept here, sealed with a passphrase.",
            &[ActionParam::required(
                "passphrase",
                ParamType::Text,
                "What the backup is sealed with.",
            )],
        ))),
    ];
    // The actions a desktop hands on to this daemon, as they are described everywhere.
    rows.extend(
        tilo_server_protocol::device_tickets::HANDED_ON
            .into_iter()
            .map(|action| {
                let mut descriptor = tilo_server_protocol::device_tickets::descriptor(action);
                if let Some(descriptor) = &mut descriptor {
                    descriptor.action_label = crate::i18n::t(&format!("device-{action}"));
                }
                row(descriptor)
            }),
    );
    // What the daemon does with the device key for the other programs of this computer.
    rows.extend(
        super::key_use::descriptors()
            .into_iter()
            .map(|descriptor| row(Some(descriptor))),
    );
    // What the device's shell runtime asks of it: Terminal's environments and hosted shells.
    rows.extend(
        super::shell_runtime::descriptors()
            .into_iter()
            .map(|descriptor| row(Some(descriptor))),
    );
    // The device's own settings: what it keeps before anyone claims it, read through the record
    // API and changed here.
    if keep(super::records::SET) {
        rows.extend(settings_rows());
    }
    // A server's turn among its person's servers, which its own daemon answers:
    // the automatic update path asks the machine itself before it reboots.
    let updates = CatalogueResource {
        id: UPDATES.to_owned(),
        kind: "update".to_owned(),
        label: "Updates".to_owned(),
        presentation: None,
    };
    let update_row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: UPDATES.to_owned(),
        parent_resource: String::new(),
        resource: updates.clone(),
        action,
        ..CatalogueRow::default()
    };
    let update_rows = vec![
        update_row(None),
        update_row(Some(update_descriptor(
            PREPARE,
            "Take the person's servers' update lease, check every workspace's copies, and drain \
             this server, so it may restart into the staged image alone.",
            &[ActionParam::required(
                "target",
                ParamType::Text,
                "The staged image it would restart into.",
            )],
        ))),
        update_row(Some(update_descriptor(
            BOOTED,
            "Say which image this server booted and that it passed its health verdict, so the \
             update lease goes to the person's next server.",
            &[ActionParam::required(
                "image",
                ParamType::Text,
                "The image it booted.",
            )],
        ))),
        update_row(Some(update_descriptor(
            FLOOR,
            "Ask the person's other devices for approvals for this server's floor to move past \
             the generation that booted.",
            &[
                ActionParam::required(
                    "generation",
                    ParamType::Integer,
                    "The generation that booted.",
                ),
                ActionParam::required("image", ParamType::Text, "The image it booted."),
                ActionParam::required("updates", ParamType::Text, "The updates it follows."),
                ActionParam::required(
                    "authority",
                    ParamType::Text,
                    "The public half of the person's floor key.",
                ),
                ActionParam::required(
                    "request",
                    ParamType::Text,
                    "What its TPM states, as `tilo-rollback floor-request` wrote it.",
                ),
            ],
        ))),
    ];
    // A device before its person's services run takes no update turn, and is asked nothing of
    // its machine: each resource is listed where one of its actions is kept.
    let update_rows = if keep(PREPARE) {
        update_rows
    } else {
        Vec::new()
    };
    let server_rows = if crate::device::keep::operations::ACTIONS
        .iter()
        .any(|action| keep(action))
    {
        server_rows()
    } else {
        Vec::new()
    };
    let rows = rows
        .into_iter()
        .chain(update_rows)
        .chain(server_rows)
        .filter(|row| {
            row.action
                .as_ref()
                .is_none_or(|action| keep(&action.address.action))
        })
        .collect();
    CataloguePage::from_rows(REVISION.to_owned(), rows, query)
}

/// The page `query` asks of the device's own settings actions alone: what a device placed for
/// records lists of its own, as it sends each to its workspace's host ([`super::confined`]).
#[must_use]
pub(crate) fn settings_catalogue(query: &CatalogueQuery) -> CataloguePage {
    CataloguePage::from_rows(REVISION.to_owned(), settings_rows(), query)
}

/// The settings rows: the device's own records resource, and the actions that write one.
fn settings_rows() -> Vec<CatalogueRow> {
    let resource = CatalogueResource {
        id: super::records::RESOURCE.to_owned(),
        kind: "settings".to_owned(),
        label: "Settings".to_owned(),
        presentation: None,
    };
    let row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: super::records::RESOURCE.to_owned(),
        parent_resource: String::new(),
        resource: resource.clone(),
        action,
        ..CatalogueRow::default()
    };
    let mut descriptor = descriptor(
        super::records::SET,
        "Keep a setting of this device: what its greeter and launcher read before anyone \
         claims it, and after.",
        &[
            ActionParam::required("name", ParamType::Text, "The setting's name."),
            ActionParam::required("value", ParamType::Object, "What the setting is set to."),
        ],
    );
    descriptor.address =
        tilo_actions::ActionAddress::new(super::records::RESOURCE, super::records::SET);
    "settings".clone_into(&mut descriptor.resource_kind);
    "Settings".clone_into(&mut descriptor.resource_label);
    let mut remove = descriptor.clone();
    remove.address =
        tilo_actions::ActionAddress::new(super::records::RESOURCE, super::records::REMOVE);
    remove.action_label = crate::i18n::t("device-remove");
    remove.description = Some("Forget a setting this device kept.".to_owned());
    remove.params = vec![ActionParam::required(
        "name",
        ParamType::Text,
        "The setting's name.",
    )];
    vec![row(None), row(Some(descriptor)), row(Some(remove))]
}

/// The machine controls' rows: the [`SERVER`] resource and each of its actions.
fn server_rows() -> Vec<CatalogueRow> {
    let resource = CatalogueResource {
        id: SERVER.to_owned(),
        kind: "server".to_owned(),
        label: "Server".to_owned(),
        presentation: None,
    };
    let row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: SERVER.to_owned(),
        parent_resource: String::new(),
        resource: resource.clone(),
        action,
        ..CatalogueRow::default()
    };
    let device = || {
        ActionParam::optional(
            "device",
            ParamType::Text,
            "The server's device key: another of the person's servers carries it out itself; \
             this machine when left out.",
        )
    };
    let service = |what: &'static str| ActionParam::required("service", ParamType::Text, what);
    let mut rows = vec![row(None)];
    for action in crate::device::keep::operations::ACTIONS {
        let (description, mut params) = match action {
            "status" => (
                "Its health, what it holds and how its copies stand, how it unlocks, its \
                 updates and its hardware.",
                Vec::new(),
            ),
            "journal" => (
                "A service's recent journal lines.",
                vec![
                    service("The systemd unit, e.g. tilo-runtime.service."),
                    ActionParam::optional(
                        "lines",
                        ParamType::Integer,
                        "How many lines back to read, at most 500; 200 when left out.",
                    ),
                ],
            ),
            "restart" => (
                "Restart one of the machine's systemd units.",
                vec![service("The systemd unit, e.g. tilo-runtime.service.")],
            ),
            "reboot" => ("Reboot the machine.", Vec::new()),
            "stage-update" => (
                "Fetch and stage the newest OS update now, instead of waiting for its timer.",
                Vec::new(),
            ),
            "pause-updates" => ("Stop taking OS updates until they are resumed.", Vec::new()),
            "resume-updates" => ("Follow OS updates again.", Vec::new()),
            "rollback" => ("Boot the previous system from now on.", Vec::new()),
            "drain" => (
                "Suspend its workspaces' Linux machines and hand their work off, before taking \
                 it out of service.",
                Vec::new(),
            ),
            _ => (
                "Make Tilo the machine's default start, after any network-boot entries; asked \
                 again, it restores the order it wrote.",
                Vec::new(),
            ),
        };
        params.insert(0, device());
        let mut described = descriptor(action, description, &params);
        described.address = tilo_actions::ActionAddress::new(SERVER, action);
        "server".clone_into(&mut described.resource_kind);
        "Server".clone_into(&mut described.resource_label);
        if matches!(action, "status" | "journal") {
            described.contract.kind = ActionKind::Read;
        }
        // Booting the previous system replaces the one the machine runs: whoever asks it is asked
        // first, on the device they ask from.
        if action == "rollback" {
            described.contract.risk = tilo_actions::RiskLevel::Destructive;
            described.contract.settle_confirmation();
        }
        rows.push(row(Some(described)));
    }
    rows
}

/// Whether `caller`, asking from another device, may run the machine control `action`: the one
/// policy decision at this machine's settings node (`devices/<this device>/settings/server`) in
/// its person's own workspace. The person reaches it from any of their devices as their own
/// record; anyone else only by a grant naming them there. A workspace's ownership or role never
/// reaches it.
async fn allows_control(
    controls: Option<&crate::device::keep::operations::Controls>,
    own: &DeviceCert,
    caller: &tilo_actions::remote::Caller,
    (action, confirmation): (&str, Option<&tilo_actions::remote::Confirmation>),
) -> Result<(), ActionCallError> {
    let not_yours = || refused(NOT_YOURS);
    let controls = controls.ok_or_else(not_yours)?;
    let workspace = tilo_server_protocol::servers::person_workspace_id(&own.user_public_key);
    let policy = controls
        .workspaces()
        .policy_of(&workspace)
        .await
        .ok_or_else(not_yours)?;
    let node = tilo_workspace_service::workspaces::server_controls::settings_node(
        &workspace,
        &own.device_public_key,
        tilo_workspace_service::workspaces::server_controls::CONTROL,
    );
    // The control as the machine's catalogue declares it: one it does not declare says nothing of
    // what it is able to do, so no grant admits it, and a grant never answers what its contract
    // asks its caller first.
    let declared = server_rows()
        .into_iter()
        .filter_map(|row| row.action)
        .find(|declared| declared.address.action == action)
        .ok_or_else(not_yours)?;
    if !tilo_workspace_service::workspaces::server_controls::caller_may(
        &policy,
        caller,
        &node,
        declared.contract.kind,
    ) {
        return Err(not_yours());
    }
    if !declared.contract.confirmation.met_by(confirmation) {
        return Err(ActionCallError::confirmation_required());
    }
    Ok(())
}

/// Ask the person's devices, from this machine, for its rollback floor to move,
/// and keep what they signed where `tilo-rollback-confirm` spends it.
async fn floor_answer(
    machine: &crate::device::keep::rollout::ServerMachine,
    args: &Value,
) -> Result<Value, String> {
    let text = |field: &str| {
        args.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|said| !said.trim().is_empty())
            .ok_or_else(|| format!("floor names no {field}"))
    };
    let asked = crate::device::keep::servers::floor::Asked {
        generation: args
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "floor names no generation".to_owned())?,
        image: text("image")?,
        updates: text("updates")?,
        authority: text("authority")?,
        request: text("request")?,
    };
    let staging = machine.state_root().join(APPROVALS_STAGING);
    let answer = match machine.floor(asked).await? {
        Ok(approvals) => {
            crate::device::keep::rollout::store_approvals(&staging, &approvals)?;
            crate::device::keep::rollout::Answer::Floor {
                approvals,
                why: String::new(),
            }
        }
        Err(why) => crate::device::keep::rollout::Answer::Floor {
            approvals: Vec::new(),
            why,
        },
    };
    serde_json::to_value(answer).map_err(|error| error.to_string())
}

/// `page`, the workspace's apps, with the daemon's own `tilo.device` provider beside them: listed
/// for every caller, and its page answered whole when the query names it.
#[must_use]
pub(crate) fn with_device_actions(
    mut page: CataloguePage,
    query: &CatalogueQuery,
    workspace: &str,
) -> CataloguePage {
    if query.provider.as_deref() == Some(PROVIDER) {
        return catalogue(query);
    }
    if query.provider.is_none() {
        page.providers.extend(providers(workspace));
    }
    page
}

/// `action` of `devices`, as the catalogue publishes it, named by the runtime's catalogue
/// (`device-<action>`) in the language read now: the seats' actions start from it too.
pub(crate) fn descriptor(
    action: &str,
    description: &str,
    params: &[ActionParam],
) -> ActionDescriptor {
    let mut descriptor = ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, action),
        resource_kind: "device".to_owned(),
        resource_label: "Devices".to_owned(),
        action_label: crate::i18n::t(&format!("device-{action}")),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(description.to_owned()),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Outside,
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params: params.to_vec(),
        view: None,
    };
    if SECRET_ANSWERS.contains(&action) {
        descriptor
            .contract
            .tags
            .push(tilo_actions::SECRET_ANSWER_TAG.to_owned());
    }
    // Only the person's root issues a ticket for a computer, certifies a key and answers a
    // sign-in ([`tilo_actions::ROOT_ONLY_TAG`]): a device that does not keep it asks them, and an
    // agent of theirs is handed the ask on, never answered a ticket itself.
    if matches!(action, ISSUE | CERTIFY_KEY | RELEASE_HALF | ANSWER_SIGN_IN) {
        descriptor
            .contract
            .tags
            .push(tilo_actions::ROOT_ONLY_TAG.to_owned());
    }
    descriptor
}

/// The device actions whose answer is a secret ([`tilo_actions::SECRET_ANSWER_TAG`]): handed to
/// the person once, and kept nowhere here.
const SECRET_ANSWERS: [&str; 4] = [RECOVERY_WORDS, BACKUP, RELEASE_HALF, OPEN_VAULT];

/// The device actions the root of the account calling them signs: each account's root signs its
/// own calls and no other's.
const ROOT_PERSON_SIGNED: [&str; 5] = [
    RECOVERY_WORDS,
    BACKUP,
    CERTIFY_KEY,
    RELEASE_HALF,
    ANSWER_SIGN_IN,
];

/// The person `caller` is, when it is a person: their root key.
fn caller_person(caller: &tilo_actions::remote::Caller) -> Option<[u8; 32]> {
    hex::decode(caller.acting().person_id()?)
        .ok()?
        .try_into()
        .ok()
}

/// The person whose root signs `caller`'s call: the person themselves, or the one an agent
/// acting for them acts as. What only that root signs is signed with it, never by the agent.
fn acting_person(caller: &tilo_actions::remote::Caller) -> Option<[u8; 32]> {
    caller_person(caller).or_else(|| {
        hex::decode(caller.acting_for_person()?)
            .ok()?
            .try_into()
            .ok()
    })
}

/// The device actions as the device's one dispatch runs them: it admitted the caller, recorded the
/// invocation, and follows it here.
impl tilo_actions::remote::ActionProvider for Tickets {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(catalogue(&query)))
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Tickets::invoke(self, &request, caller)
    }

    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        id: &InvocationId,
    ) -> Option<InvocationState> {
        Tickets::observe(self, id)
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        id: &InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(match Tickets::observe(self, id) {
            Some(_) => ActionCallError::NotCancellable {
                reason: "a device action ends by itself".into(),
            },
            None => ActionCallError::UnknownInvocation,
        }))
    }
}

/// `action` of [`UPDATES`], as the catalogue publishes it: the same shape as a
/// device action, on the update resource.
fn update_descriptor(action: &str, description: &str, params: &[ActionParam]) -> ActionDescriptor {
    let mut descriptor = descriptor(action, description, params);
    descriptor.address = tilo_actions::ActionAddress::new(UPDATES, action);
    "update".clone_into(&mut descriptor.resource_kind);
    "Updates".clone_into(&mut descriptor.resource_label);
    descriptor
}

/// The person's root on this computer, read when a ticket is asked for: the store the daemon
/// keeps, or a test's own.
pub(crate) type RootSource = Arc<dyn Fn() -> Result<Option<SigningIdentity>> + Send + Sync>;

/// Record the certified `cert` as a server in the person's Servers document, as a server's own
/// start does when it is taken in: an entry under its device key, dialled at `endpoint`, or by its
/// key alone until the server fills its endpoint itself when it first starts.
pub(super) async fn server_entry(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    (session, cert): (Option<&Entered>, &DeviceCert),
    endpoint: Option<iroh::EndpointAddr>,
) -> Result<(), String> {
    use tilo_workspace_service::workspaces::servers;

    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => iroh::EndpointAddr::from_parts(
            iroh::EndpointId::from_bytes(&cert.device_public_key)
                .map_err(|error| format!("the server's device key: {error}"))?,
            Vec::new(),
        ),
    };
    if let Some(device) = session {
        let bytes = tilo_user_identity::canonical::canonical_bytes(cert)
            .map_err(|error| format!("the server's certificate: {error}"))?;
        let endpoint = serde_json::to_string(&endpoint).map_err(|error| error.to_string())?;
        return device
            .host_action(
                "server.admit",
                Map::from_iter([
                    ("cert".to_owned(), hex::encode(bytes).into()),
                    ("endpoint".to_owned(), endpoint.into()),
                ]),
            )
            .await
            .map(drop)
            .map_err(|error| format!("{error}"));
    }
    if !tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(workspaces)
        .await
    {
        return Err("this computer does not hold the person's Servers workspace yet".to_owned());
    }
    servers::admit(workspaces, cert, &endpoint).await
}

/// Record the certified `cert` on the plan's entry, as one of the person's own devices does when
/// it approves a machine: the entry becomes the machine's under its key, and the device that
/// hosts the Servers workspace hands it over from there.
pub(super) async fn plan_entry(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    session: Option<&Entered>,
    plan: &str,
    cert: &DeviceCert,
) -> Result<(), String> {
    use tilo_workspace_service::workspaces::servers;

    if let Some(device) = session {
        let shown = device
            .host_action(
                "machine.show",
                Map::from_iter([("plan".to_owned(), plan.into())]),
            )
            .await
            .map_err(|error| format!("{error}"))?;
        let nonce = shown
            .pointer("/plan/token/nonce")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("plan {plan} names no token"))?;
        return device
            .host_action(
                "machine.admit",
                Map::from_iter([
                    (
                        "device".to_owned(),
                        hex::encode(cert.device_public_key).into(),
                    ),
                    ("nonce".to_owned(), nonce.into()),
                ]),
            )
            .await
            .map(drop)
            .map_err(|error| format!("{error}"));
    }
    servers::admit_planned(workspaces, plan, &cert.device_public_key).await
}

/// What this computer's daemon answers the device actions with: its endpoint, the device it is,
/// the login it serves (if any), and the one action service that serves a ticket's certify call.
#[derive(Clone)]
pub(crate) struct Tickets(Arc<Inner>);

struct Inner {
    key: SigningKey,
    cert: DeviceCert,
    endpoint: Endpoint,
    /// Where this device enters its person's workspaces, once it knows a server of theirs.
    home: Option<Arc<super::home::Home>>,
    /// The person's workspace here, on a computer no ticket made: the one a computer it adds
    /// enters, founded when one first does.
    first: Option<Arc<super::first_workspace::FirstWorkspace>>,
    /// The workspace service this daemon's device writes a certified computer's entry through,
    /// and the session it asks its host through while it is a member.
    workspaces: Option<Arc<ServiceWorkspaces>>,
    place: Place,
    service: ActionService,
    /// The tickets this device serves, and the person's devices as they are named and revoked
    /// from this one.
    fleet: TicketServer,
    /// Each invocation of a device action, and where it is.
    calls: std::sync::Mutex<HashMap<InvocationId, InvocationState>>,
    /// What starts this device's update, where it is not `tilo update`.
    update: std::sync::OnceLock<manage::UpdateStart>,
    /// This machine's part in its person's update rollout, when this computer is
    /// one of their servers: what the update actions take its turn through.
    rollout: std::sync::Mutex<Option<crate::device::keep::rollout::ServerMachine>>,
    /// The machine's controls, when this computer is a server: what the [`SERVER`] actions are
    /// carried out through.
    controls: std::sync::Mutex<Option<crate::device::keep::operations::Controls>>,
    /// Where the person's devices are reached that no lookup finds.
    reach: std::sync::Mutex<HashMap<[u8; 32], iroh::EndpointAddr>>,
    /// Told whenever a device action that waited ended.
    ended: tokio::sync::Notify,
    runtime: tokio::runtime::Handle,
    /// The device a ticket made of this computer, once it logged in with one: the person's part
    /// of the daemon starts again as it.
    logged_in: tokio::sync::watch::Sender<Option<Box<Device>>>,
}

impl Tickets {
    /// Serve again every planned machine's ticket this daemon issued before it last stopped and
    /// that still waits for its machine.
    pub(crate) async fn restore(&self) {
        if let Err(error) = self.0.fleet.restore().await {
            tracing::warn!(%error, "the planned machines' tickets are not served again yet");
        }
    }

    /// Serve tickets from this daemon, on `service`: the one action service its router answers
    /// the device actions on, which admits a ticket's certify call while a computer asks.
    ///
    /// # Panics
    /// Outside a runtime context, where an answer could not be followed.
    pub(crate) fn new(
        (key, cert): (SigningKey, DeviceCert),
        endpoint: Endpoint,
        home: Option<Arc<super::home::Home>>,
        root: RootSource,
        first: Option<Arc<super::first_workspace::FirstWorkspace>>,
        workspaces: Option<Arc<ServiceWorkspaces>>,
        place: Place,
        service: ActionService,
    ) -> Self {
        let fleet = TicketServer::new(Arc::new(manage::Fleet {
            key: key.clone(),
            cert: cert.clone(),
            endpoint: endpoint.clone(),
            home: home.clone(),
            root,
            first: first.clone(),
            workspaces: workspaces.clone(),
            service: service.clone(),
        }));
        Self(Arc::new(Inner {
            key,
            cert,
            endpoint,
            home,
            first,
            workspaces,
            place,
            service,
            fleet,
            calls: std::sync::Mutex::new(HashMap::new()),
            update: std::sync::OnceLock::new(),
            rollout: std::sync::Mutex::new(None),
            controls: std::sync::Mutex::new(None),
            reach: std::sync::Mutex::default(),
            ended: tokio::sync::Notify::new(),
            runtime: tokio::runtime::Handle::current(),
            logged_in: tokio::sync::watch::Sender::new(None),
        }))
    }

    /// The device a ticket made of this computer through these device actions, once one did.
    pub(crate) fn logged_in(&self) -> tokio::sync::watch::Receiver<Option<Box<Device>>> {
        self.0.logged_in.subscribe()
    }

    /// The record of the root the calling account's person keeps here, with the identity it
    /// signs for: root-only calls are signed with it and no other account's.
    fn rooted_for(
        caller: &tilo_actions::remote::Caller,
    ) -> Result<(std::path::PathBuf, super::signing::Rooted), String> {
        let record = if caller.is_system() {
            super::services::device_root_record()
        } else {
            let person = caller_person(caller).ok_or_else(|| {
                "only a person's root signs this, and this caller acts for nobody".to_owned()
            })?;
            super::services::root_record_for_person(person)
        }
        .map_err(|error| error.to_string())?;
        let rooted = super::signing::Rooted::at(&record).map_err(|error| error.to_string())?;
        Ok((record, rooted))
    }

    /// Where this device enters its person's workspaces, once it knows.
    async fn here(&self) -> Option<Device> {
        match &self.0.home {
            Some(home) => home.get().await,
            None => None,
        }
    }

    /// This machine's part in its person's update rollout, when this computer is
    /// one of their servers: the update actions take its turn through it.
    pub(crate) fn with_rollout(
        self,
        rollout: Option<crate::device::keep::rollout::ServerMachine>,
    ) -> Self {
        *self.0.rollout.lock_recover() = rollout;
        self
    }

    /// The machine's controls, when this computer is a server: the [`SERVER`] actions are carried
    /// out through them.
    pub(crate) fn with_controls(
        self,
        controls: Option<crate::device::keep::operations::Controls>,
    ) -> Self {
        *self.0.controls.lock_recover() = controls;
        self
    }

    /// Whether `request` is one of the device actions.
    pub(crate) fn answers(request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER
            && ((request.target.address.resource == RESOURCE
                && matches!(
                    request.target.address.action.as_str(),
                    INFO | ISSUE
                        | FOUND
                        | DEV_ID
                        | RESTORE
                        | LOG_IN
                        | PENDING
                        | APPROVE
                        | REPORTED
                        | RENAME
                        | REVOKE
                        | UPDATE
                        | RECOVERY_WORDS
                        | BACKUP
                        | CERTIFY_KEY
                        | RELEASE_HALF
                        | ANSWER_SIGN_IN
                        | IDENTITIES
                        | USE_IDENTITY
                        | REMOVE_IDENTITY
                        | PROTECT_IDENTITY
                ))
                || (request.target.address.resource == UPDATES
                    && matches!(
                        request.target.address.action.as_str(),
                        PREPARE | BOOTED | FLOOR
                    ))
                || (request.target.address.resource == SERVER
                    && crate::device::keep::operations::ACTIONS
                        .contains(&request.target.address.action.as_str())))
    }

    /// Whether `request` asks this device, which serves a person, to found or restore one: refused
    /// to whoever asks, since the answer only says the device has its person.
    pub(crate) fn asks_for_a_person(request: &InvokeRequest) -> bool {
        request.target.provider == PROVIDER
            && request.target.address.resource == RESOURCE
            && matches!(request.target.address.action.as_str(), FOUND | RESTORE)
    }

    /// Whether `request` only reads what the device is ([`INFO`]) or which identities it keeps,
    /// without their secrets ([`IDENTITIES`]), and lets nobody in: what an agent the person runs
    /// here, a plain connection of their account among them, reads as they do.
    pub(crate) fn only_reads(request: &InvokeRequest) -> bool {
        Self::answers(request)
            && matches!(request.target.address.action.as_str(), INFO | IDENTITIES)
    }

    /// Take `request`, one of [`Self::answers`]' actions, from `caller`: its receipt.
    pub(crate) fn invoke(
        &self,
        request: &InvokeRequest,
        caller: &tilo_actions::remote::Caller,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        // A device a host placed for records works on those records alone: none of this device's
        // actions (a ticket for a new computer, a name, a revocation, the updates and server
        // controls) is its to ask, whichever host placed it.
        if caller.placed_records().is_some() {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        if request.target.address.resource == UPDATES {
            return self.take_the_update_turn(request);
        }
        if request.target.address.resource == SERVER {
            return self.control(request, None);
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(refused("a device action asks with its arguments")));
        };
        let args = Value::Object(argument.clone());
        let action = request.target.address.action.clone();
        // The id a development build runs under is the acting person's own to ask, as installing
        // the build is: it is derived from this device's key, and the build is compiled for it.
        if action == DEV_ID {
            return self.dev_id_answered(&args, invocation, caller);
        }
        // A device confined to one workspace, by a certificate for it or one placed for records of
        // it, issues no ticket: adding a computer is the person's own act on a device that keeps
        // their root, and a confined device keeps none.
        if action == ISSUE && self.0.cert.capabilities.workspace_name().is_some() {
            return Pending::ready(Err(refused(
                "this device is confined to one workspace: it issues no tickets",
            )));
        }
        // What the calling account's own root signs is answered with that root, and with no
        // other: an account whose root this computer keeps not signs nothing here.
        let rooted = if ROOT_PERSON_SIGNED.contains(&action.as_str()) {
            match Self::rooted_for(caller) {
                Ok(rooted) => Some(rooted),
                Err(reason) => return Pending::ready(Err(refused(reason))),
            }
        } else {
            None
        };
        let wait = args.get("wait").and_then(Value::as_bool) == Some(true);
        // One act that hands back several tickets (a founding's first, say) asks for each beside
        // the others: none takes another's place.
        let beside = args.get("beside").and_then(Value::as_bool) == Some(true);
        // A ticket names where the new computer dials this one: made before this device's
        // endpoint reached one of its relays, it would name only the network this device stands
        // in. It waits for the relay first, as an invitation does, unless the ticket is issued as
        // one of a founding's first: its founder waits at the call, and it names the address the
        // device has now rather than waiting on a relay that is not there.
        let unreached = action == ISSUE && !wait && !beside && !reached(&self.0.endpoint);
        let waiting = matches!(
            action.as_str(),
            ISSUE | LOG_IN | PENDING | APPROVE | REPORTED | RENAME | REVOKE | UPDATE
        );
        if !waiting {
            // Certifying a key and answering a sign-in read and write the person's workspace,
            // which waits on the workspaces: never on the app runtime's own thread, whose tasks
            // would all stop behind that wait. Asked there, they are answered from a thread of
            // their own.
            if matches!(action.as_str(), CERTIFY_KEY | ANSWER_SIGN_IN)
                && tilo_rt::app().is_runtime_thread()
            {
                let (answered, answer) = tilo_actions::remote::pending();
                let served = self.clone();
                self.0.runtime.spawn_blocking(move || {
                    answered.complete(served.answer_at_once(&action, &args, rooted, invocation));
                });
                return answer;
            }
            return Pending::ready(self.answer_at_once(&action, &args, rooted, invocation));
        }
        if action == REVOKE {
            return self.revoke_answered(&args, invocation, caller);
        }
        let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        let receipt = accepted.receipt();
        self.0.calls.lock_recover().insert(invocation, accepted);
        let served = self.clone();
        // Issuing a ticket is the person's own act: the caller's where it is one, and the person
        // an agent acting for them acts as, whose root signs it on the device asked.
        let person = if action == ISSUE {
            acting_person(caller)
        } else {
            caller_person(caller)
        };
        self.0
            .runtime
            .spawn(super::services::with_root_caller(person, async move {
                let answer = match action.as_str() {
                    LOG_IN => served.log_in(&args).await,
                    PENDING => served.pending(&args).await,
                    APPROVE => served.approve(&args).await,
                    REPORTED => served.report(&args).await,
                    RENAME | REVOKE | UPDATE => served.manage(&action, &args).await,
                    _ if wait => served.waited().await,
                    _ => {
                        if unreached {
                            tilo_endpoint_reachability::await_relay(&served.0.endpoint).await;
                        }
                        served.issue(&args, beside).await
                    }
                };
                let update = match answer {
                    Ok(value) => succeeded(invocation, value),
                    Err(error) => failed(invocation, &format!("{error:#}")),
                };
                served
                    .0
                    .calls
                    .lock_recover()
                    .insert(invocation, InvocationState::new(invocation, update));
                served.0.ended.notify_waiters();
            }));
        Pending::ready(Ok(receipt))
    }

    /// Answer [`DEV_ID`] here and now: the id a development build runs under is the acting
    /// person's own to ask, as installing the build is.
    fn dev_id_answered(
        &self,
        args: &Value,
        invocation: InvocationId,
        caller: &tilo_actions::remote::Caller,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let personal = !tilo_api::daemon::system_daemon();
        let answer = match dev_id_answer(caller, personal, args, &self.0.key) {
            Ok(answer) => answer,
            Err(error) => return Pending::ready(Err(error)),
        };
        let state = InvocationState::new(invocation, succeeded(invocation, answer));
        let receipt = state.receipt();
        self.0.calls.lock_recover().insert(invocation, state);
        Pending::ready(Ok(receipt))
    }

    /// Answer [`REVOKE`] once this device refuses the key, not when it is queued: the very next
    /// call the key makes, from the answer on, reaches nothing.
    fn revoke_answered(
        &self,
        args: &Value,
        invocation: InvocationId,
        caller: &tilo_actions::remote::Caller,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (answered, answer) = tilo_actions::remote::pending();
        let served = self.clone();
        let (args, action, person) = (args.clone(), REVOKE.to_owned(), caller_person(caller));
        self.0
            .runtime
            .spawn(super::services::with_root_caller(person, async move {
                let answer = served.manage(&action, &args).await;
                let update = match answer {
                    Ok(value) => succeeded(invocation, value),
                    Err(error) => failed(invocation, &format!("{error:#}")),
                };
                let state = InvocationState::new(invocation, update);
                let receipt = state.receipt();
                served.0.calls.lock_recover().insert(invocation, state);
                served.0.ended.notify_waiters();
                answered.complete(Ok(receipt));
            }));
        answer
    }

    /// Answer `action` here and now, with the root the calling account keeps where only it signs
    /// the call: its receipt, and what the call keeps recorded beside it.
    fn answer_at_once(
        &self,
        action: &str,
        args: &Value,
        rooted: Option<(std::path::PathBuf, super::signing::Rooted)>,
        invocation: InvocationId,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let answer = match action {
            INFO => Ok(device_info(&self.0.place, &self.0.key)),
            FOUND | RESTORE => Err(anyhow!("{ALREADY_A_PERSONS}: it serves them")),
            RECOVERY_WORDS => {
                let (record, _) = rooted_or_refused(rooted)?;
                super::services::recovery_words_in(&record)
                    .map(|words| json!({ "words": words }))
                    .map_err(Into::into)
            }
            BACKUP => {
                let (record, _) = rooted_or_refused(rooted)?;
                super::services::backup_in(
                    &record,
                    args.get("passphrase")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
                .map(|backup| json!({ "backup": hex::encode(backup) }))
                .map_err(Into::into)
            }
            CERTIFY_KEY => {
                let (_, root) = rooted_or_refused(rooted)?;
                self.certify_key(args, root)
            }
            RELEASE_HALF => {
                let (_, root) = rooted_or_refused(rooted)?;
                super::signing::release_half(args, &root).map_err(Into::into)
            }
            ANSWER_SIGN_IN => {
                let (_, root) = rooted_or_refused(rooted)?;
                super::signing::answer_sign_in(self.0.place.keys(), args, root).map_err(Into::into)
            }
            _ => super::services::identity_step(action, args).map_err(Into::into),
        };
        let update = match answer {
            Ok(value) => succeeded(invocation, value),
            Err(error) => failed(invocation, &format!("{error:#}")),
        };
        let state = InvocationState::new(invocation, update);
        let receipt = state.receipt();
        let succeeded_now = matches!(state.update, InvocationUpdate::Succeeded { .. });
        // A secret is handed over in the receipt alone: what is kept of the call says only that
        // it was given.
        let kept = if SECRET_ANSWERS.contains(&action) && succeeded_now {
            InvocationState::new(invocation, succeeded(invocation, json!({ "given": true })))
        } else {
            state
        };
        self.0.calls.lock_recover().insert(invocation, kept);
        Ok(receipt)
    }

    /// Issue the ticket `kind` asks for (`server` for a server, `device` for a computer at hand)
    /// as `person`'s own act, through the one `devices#issue` call `tilo device add` makes: the
    /// ticket as the one string the person hands on, served beside the tickets issued before it
    /// so each computer the founding's first tickets are handed to can still spend its own.
    ///
    /// # Errors
    /// The kind is no ticket this device issues, the device is not reachable yet, or it keeps no
    /// root of that person's.
    pub(crate) async fn issue_first(&self, person: [u8; 32], kind: &str) -> Result<Value, String> {
        let mut args = Map::new();
        match kind {
            "server" => {
                args.insert("server".to_owned(), true.into());
            }
            "device" => {}
            other => return Err(format!("{other} is no ticket a founding issues")),
        }
        args.insert("beside".to_owned(), true.into());
        let request = InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, ISSUE),
            },
            argument: InvokeArgument::Json(args),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        // The person just founded holds the root this device keeps, where this computer is the
        // one their account made them on: otherwise their root is the device's own.
        let caller =
            tilo_actions::remote::Caller::authenticated(person, self.0.cert.device_public_key);
        match self.run_to_end(&request, &caller).await {
            InvocationUpdate::Succeeded { output, .. } => Ok(output.value.unwrap_or(Value::Null)),
            InvocationUpdate::Failed { error, .. } => Err(error.message),
            _ => Err("the founding's ticket did not issue".to_owned()),
        }
    }

    /// Take `request` and wait for it to end: how it ended.
    pub(crate) async fn run_to_end(
        &self,
        request: &InvokeRequest,
        caller: &tilo_actions::remote::Caller,
    ) -> InvocationUpdate {
        // The call was kept for this device as one that keeps the caller's person's root: it runs
        // here with that root, as their own call. A ticket is the person's own act, and is signed
        // with the root of the person an agent of theirs acts for ([`Self::invoke`]).
        let invocation = request.invocation;
        if let Some(Err(error)) = super::services::with_root_caller(
            caller_person(caller),
            Tickets::invoke(self, request, caller),
        )
        .await
        {
            return failed(invocation, &error.to_string());
        }
        loop {
            let ended = self.0.ended.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            match self.observe(&invocation) {
                Some(state) if state.update.is_terminal() => return state.update,
                None => return failed(invocation, "the device action was not taken"),
                Some(_) => ended.await,
            }
        }
    }

    /// Take `request`, one of the update actions: this machine's turn among its
    /// person's servers, answered once the machine replied.
    fn take_the_update_turn(
        &self,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(refused("an update action asks with its arguments")));
        };
        let args = Value::Object(argument.clone());
        let action = request.target.address.action.clone();
        let Some(machine) = self.0.rollout.lock_recover().clone() else {
            return Pending::ready(Err(refused(
                "this computer keeps no workspaces: it has no update turn to take",
            )));
        };
        let (completer, pending) = tilo_actions::remote::pending();
        let served = self.clone();
        self.0.runtime.spawn(async move {
            let answer = match action.as_str() {
                FLOOR => floor_answer(&machine, &args).await,
                other => crate::device::keep::rollout::answer_action(&machine, other, &args)
                    .await
                    .and_then(|answer| {
                        serde_json::to_value(answer).map_err(|error| error.to_string())
                    }),
            };
            let update = match answer {
                Ok(value) => succeeded(invocation, value),
                Err(why) => failed(invocation, &why),
            };
            let state = InvocationState::new(invocation, update);
            served
                .0
                .calls
                .lock_recover()
                .insert(invocation, state.clone());
            served.0.ended.notify_waiters();
            completer.complete(Ok(state.receipt()));
        });
        pending
    }

    /// Take `request`, one of the machine's controls ([`SERVER`]): carried out here, or, when it
    /// names another of the person's servers, asked of that one. `asker` is the caller from
    /// another device, held to [`allows_control`]; `None` for one this device's dispatch admitted
    /// already.
    pub(crate) fn control(
        &self,
        request: &InvokeRequest,
        asker: Option<tilo_actions::remote::Caller>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(refused("a server's control asks with its arguments")));
        };
        let args = Value::Object(argument.clone());
        let action = request.target.address.action.clone();
        // The approve this device's dispatch took goes with the call to the server it names,
        // which asks for it where its contract does.
        let confirmation = request.confirmation.clone();
        let (completer, pending) = tilo_actions::remote::pending();
        let served = self.clone();
        self.0.runtime.spawn(async move {
            let controls = served.0.controls.lock_recover().clone();
            if let Some(caller) = &asker
                && let Err(refused) = allows_control(
                    controls.as_ref(),
                    &served.0.cert,
                    caller,
                    (&action, confirmation.as_ref()),
                )
                .await
            {
                completer.complete(Err(refused));
                return;
            }
            let answer = served
                .carry_control(controls, asker.is_some(), (&action, &args), confirmation)
                .await;
            let update = match answer {
                Ok(value) => succeeded(invocation, value),
                Err(why) => failed(invocation, &why),
            };
            let state = InvocationState::new(invocation, update);
            served
                .0
                .calls
                .lock_recover()
                .insert(invocation, state.clone());
            served.0.ended.notify_waiters();
            completer.complete(Ok(state.receipt()));
        });
        pending
    }

    /// Carry the control `action` out with `args`: here, or at the server `args` names when it is
    /// another of the person's and this call is not itself one another device asked.
    async fn carry_control(
        &self,
        controls: Option<crate::device::keep::operations::Controls>,
        handed: bool,
        (action, args): (&str, &Value),
        confirmation: Option<tilo_actions::remote::Confirmation>,
    ) -> Result<Value, String> {
        match tilo_server_protocol::device_tickets::device_in(args) {
            Ok(Some(device)) if device != self.this() => {
                if handed {
                    return Err("This server carries out only what is asked of itself.".to_owned());
                }
                return self
                    .ask_device(device, (SERVER, action), args, confirmation)
                    .await
                    .map_err(|error| format!("{error:#}"));
            }
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        let controls = controls.ok_or_else(|| {
            "This computer is not a server: it has no machine controls to ask.".to_owned()
        })?;
        let result = controls.act(action, args).await?;
        serde_json::to_value(result).map_err(|error| error.to_string())
    }

    /// Where the device action `id` is, once it was taken.
    pub(crate) fn observe(&self, id: &InvocationId) -> Option<InvocationState> {
        self.0.calls.lock_recover().get(id).cloned()
    }

    /// Whether `request` asks one of the device actions whose answer is a secret
    /// ([`tilo_actions::SECRET_ANSWER_TAG`]), as their catalogue rows say.
    pub(crate) fn answers_a_secret(request: &InvokeRequest) -> bool {
        Self::listed(request).is_some_and(|action| action.contract.answers_a_secret())
    }

    /// Whether `request` asks one of the device actions only the person's root signs
    /// ([`tilo_actions::ROOT_ONLY_TAG`]), as their catalogue rows say.
    ///
    /// Only waiting for the ticket served last to be taken is none of them: it issues nothing,
    /// and answers which computer took a ticket the person already let in ([`Self::only_waits`]).
    pub(crate) fn root_only(request: &InvokeRequest) -> bool {
        Self::listed(request).is_some_and(|action| action.contract.root_only())
            && !Self::only_waits(request)
    }

    /// Whether `request` only waits for the ticket served last to be taken by a computer,
    /// issuing no ticket of its own: what `tilo device add` asks once it printed one.
    pub(crate) fn only_waits(request: &InvokeRequest) -> bool {
        Self::asks_for_a_ticket(request)
            && match &request.argument {
                InvokeArgument::Json(args) => {
                    args.get("wait").and_then(Value::as_bool) == Some(true)
                }
                _ => false,
            }
    }

    /// Whether `request` adds a computer: it asks for a ticket, and not only to wait for one.
    pub(crate) fn adds_a_device(request: &InvokeRequest) -> bool {
        Self::asks_for_a_ticket(request) && !Self::only_waits(request)
    }

    /// Whether `request` is `devices#issue` itself, in either of its forms.
    fn asks_for_a_ticket(request: &InvokeRequest) -> bool {
        Self::answers(request)
            && request.target.address.resource == RESOURCE
            && request.target.address.action == ISSUE
    }

    /// The device action `request` asks, as the catalogue lists it.
    pub(crate) fn listed(request: &InvokeRequest) -> Option<ActionDescriptor> {
        if !Self::answers(request) {
            return None;
        }
        catalogue(&CatalogueQuery::default())
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| action.address == request.target.address)
    }

    /// Serve a ticket for a new computer, approved by the person's root on this computer: the
    /// ticket as the one string the person hands on. The person who runs `tilo device add`
    /// chooses what the computer becomes; the ticket it brings carries nothing of its own. It is
    /// served `beside` the tickets issued before it, or in place of the one printed last for a
    /// computer at hand.
    async fn issue(&self, args: &Value, beside: bool) -> Result<Value> {
        let plan = args
            .get("plan")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
            .map(str::to_owned);
        let role = match plan {
            Some(plan) => Some(Role::Plan(plan)),
            None if args.get("server").and_then(Value::as_bool) == Some(true) => Some(Role::Server),
            None => None,
        };
        let fleet = &self.0.fleet;
        let ticket = fleet
            .issue(role, beside)
            .await
            .map_err(|error| anyhow!(error))?;
        Ok(json!({"ticket": ticket.encode()}))
    }

    /// Wait for the ticket served now to certify a computer, while it lasts, then for that
    /// computer to read its answer: which one it was.
    async fn waited(&self) -> Result<Value> {
        let served = self
            .0
            .fleet
            .last()
            .ok_or_else(|| anyhow!("No ticket is waiting: run `tilo device add` again."))?;
        let mut certified = served.certified();
        let cert = match tokio::time::timeout(
            Duration::from_nanos(TICKET_LIFETIME_NS),
            certified.wait_for(Option::is_some),
        )
        .await
        {
            Ok(Ok(found)) => Option::<DeviceCert>::clone(&found),
            _ => bail!(
                "No computer logged in with the ticket before it ran out: run `tilo device add` \
                 again."
            ),
        }
        .context("the certificate given is there")?;
        let mut told = served.told();
        let _ = tokio::time::timeout(TELL_WAIT, told.wait_for(|told| *told)).await;
        // The computer may ask a lost answer again: the call is answered where it is.
        tokio::time::sleep(TELL_GRACE).await;
        let bytes = tilo_user_identity::canonical::canonical_bytes(&cert)
            .map_err(|error| anyhow!("the certificate given: {error}"))?;
        Ok(json!({
            "device": hex::encode(cert.device_public_key),
            "label": cert.device_label,
            "cert": hex::encode(bytes),
        }))
    }

    /// Log in with the ticket another of the person's devices issued, as [`login_asked`] reads
    /// it: file the certificate it answers, say which device this computer became, and have the
    /// person's part of the daemon start again as it.
    async fn log_in(&self, args: &Value) -> Result<Value> {
        if let Some(line) = args.get("server").and_then(Value::as_str) {
            return self.log_in_at(line).await;
        }
        let (ticket, agent, label) = login_asked(args)?;
        let device = tilo_api::daemon::enter::log_in(
            &self.0.key,
            &self.0.endpoint,
            &self.0.place,
            ticket,
            agent,
            label,
        )
        .await?;
        let said = logged_in(&device);
        self.0.logged_in.send_replace(Some(Box::new(device)));
        Ok(said)
    }

    /// Log this computer, which holds its person's root, in at the server whose line (what it
    /// keeps in `web-host.json`) is `line`: no ticket is asked for, since the root certified this
    /// device already. The server is named among the hosts of the person's own workspace, so
    /// the copy names it from now on and every later start enters through it, and the person's
    /// part starts again through it now.
    async fn log_in_at(&self, line: &str) -> Result<Value> {
        if !self.0.cert.capabilities.can_admit_devices {
            bail!(
                "Only a computer that holds your Tilo identity logs in at a server by itself: log \
                 in here with a ticket from `tilo device add` on one of your devices."
            );
        }
        let host: tilo_server_protocol::web::PageHost =
            serde_json::from_str(line.trim()).map_err(|error| {
                anyhow!("That is not a server's line from its web-host.json: {error}")
            })?;
        let workspaces = self.0.workspaces.as_ref().ok_or_else(|| {
            anyhow!("This computer's workspaces are not open yet: try again in a moment.")
        })?;
        let endpoint = host
            .address
            .as_ref()
            .ok_or_else(|| anyhow!("that server names no address to reach it at"))?
            .endpoint_addr
            .clone();
        // Where this device keeps the person's own workspace, the server takes it in before the
        // login reads the person's list of workspaces, which lives there: named among its hosts,
        // the server answers that list. Where this device does not keep it yet, it takes it in
        // through the server, which names itself there, and the copy names it all the same.
        let copies = workspaces.copies();
        if tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(copies)
            .await
        {
            let person =
                tilo_server_protocol::servers::person_workspace_id(&self.0.cert.user_public_key);
            copies
                .attach_server(
                    &person,
                    host.server_cert.device_public_key,
                    endpoint,
                    tilo_peer_state::CopyRole::KeyHolder,
                )
                .await
                .map_err(|error| anyhow!("the server did not take your own workspace: {error}"))?;
        }
        let device =
            tilo_api::daemon::enter::log_in_at(&self.0.key, &self.0.cert, host, None).await?;
        let said = logged_in(&device);
        self.0.logged_in.send_replace(Some(Box::new(device)));
        Ok(said)
    }

    /// `ticket`'s text, parsed: what a device action names the host and direction by.
    fn ticket_in(args: &Value, direction: Direction) -> Result<Ticket> {
        let text = args
            .get("ticket")
            .and_then(Value::as_str)
            .context("a ticket is needed")?;
        let ticket = Ticket::parse(text).map_err(|why| anyhow!("{why}."))?;
        match ticket.direction {
            Direction::Login if direction == Direction::SignIn => bail!(
                "This ticket logs a computer in: run `tilo login <ticket>` with it, not \
                 `tilo device add`."
            ),
            Direction::SignIn if direction == Direction::Login => bail!(
                "This ticket signs a browser in: run `tilo device add <ticket>` with it, not \
                 `tilo login`."
            ),
            _ => {}
        }
        Ok(ticket)
    }

    /// Call `action` on the host `ticket` names, as this device, in the workspace it enters: what
    /// a device of the person's does on the host that served a ticket.
    async fn at_host(&self, ticket: &Ticket, action: &str, argument: Value) -> Result<Value> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        let here = self
            .here()
            .await
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::NO_SERVER_YET))?;
        let key = self.0.key.clone();
        let issuer = tilo_api::daemon::enter::address_of(&ticket.issuer)?;
        let dialed = tilo_server_client::ServerAddress::at(issuer);
        let service = ActionService::new(key, Arc::new(NoActions));
        let device = Arc::new(service.remote(&self.0.endpoint, &dialed.addr(), &here.workspace));
        let Value::Object(argument) = argument else {
            bail!("a device action asks with its arguments");
        };
        let request = InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let settled = device
            .invoke_until_settled(request, Duration::from_secs(60))
            .await;
        match settled {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                Ok(output.value.unwrap_or(Value::Null))
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => {
                bail!("{}", error.message.trim_end_matches('.'))
            }
            Some(Ok(_)) | None => bail!(tilo_api::daemon::enter::UNREACHED),
            Some(Err(error)) => Err(anyhow!(tilo_api::daemon::enter::certify_refused(&error))),
        }
    }

    /// The browser a sign-in `ticket` waits for, as the host sees it.
    async fn pending(&self, args: &Value) -> Result<Value> {
        let ticket = Self::ticket_in(args, Direction::SignIn)?;
        self.at_host(&ticket, PENDING, json!({"secret": ticket.secret_hex()}))
            .await
    }

    /// Sign the browser's certificate for the sign-in `ticket`, so it is signed in.
    async fn approve(&self, args: &Value) -> Result<Value> {
        let ticket = Self::ticket_in(args, Direction::SignIn)?;
        // The tab the ticket waits with is signed here, by the root this daemon keeps. A daemon
        // without the root hands the signing on: the request is already queued for the person's
        // devices that keep it. A certificate the page signed itself comes as `cert`.
        let tab = args
            .get("tab")
            .cloned()
            .map(serde_json::from_value::<tilo_device_identity::login::Tab>)
            .transpose()
            .map_err(|error| anyhow!("the tab the ticket waits with: {error}"))?;
        let cert: Option<tilo_user_identity::DeviceCert> = match &tab {
            Some(tab) if super::services::device_root_record().is_ok() => {
                let cert = super::signing::sign_tab(self.0.workspaces.as_ref(), tab).await?;
                // The person's own act, done here where their root is: the device it just
                // signed is listed among theirs at the same moment. The new device never
                // writes a workspace it does not hold to appear there.
                let workspaces = self.0.workspaces.as_ref().ok_or_else(|| {
                    anyhow!("This computer keeps no workspaces to list the device in.")
                })?;
                tilo_workspace_service::workspaces::person_workspace::devices::keep(
                    workspaces.copies(),
                    &cert,
                )
                .await
                .map_err(|error| anyhow!("{error}"))?;
                Some(cert)
            }
            Some(tab) if tab.key_for.is_some() => bail!(
                "This computer does not hold your identity, so it cannot approve this key: run \
                 `tilo device add` with the ticket on the one that does."
            ),
            _ => args
                .get("cert")
                .cloned()
                .and_then(|cert| serde_json::from_value(cert).ok()),
        };
        let here = self
            .here()
            .await
            .ok_or_else(|| anyhow!(tilo_api::daemon::enter::NO_SERVER_YET))?;
        let answer = self
            .at_host(
                &ticket,
                APPROVE,
                json!({
                    "secret": ticket.secret_hex(),
                    "cert": cert,
                    "label": here.cert.device_label,
                }),
            )
            .await?;
        Ok(match &cert {
            Some(cert) => json!({
                "label": cert.device_label,
                "device": hex::encode(cert.device_public_key),
                "signed": true,
            }),
            None => answer,
        })
    }

    /// Tell the browser that showed `ticket` what this computer did as it logged in.
    async fn report(&self, args: &Value) -> Result<Value> {
        let ticket = Self::ticket_in(args, Direction::Login)?;
        let report: Report =
            serde_json::from_value(Value::Object(args.as_object().cloned().unwrap_or_default()))
                .map_err(|_| anyhow!("a report names its ticket and step"))?;
        let step = serde_json::to_value(report.step)?;
        self.at_host(
            &ticket,
            REPORTED,
            json!({
                "ticket": ticket.encode(),
                "step": step,
                "agent": report.agent,
                "detail": report.detail,
            }),
        )
        .await?;
        Ok(json!({}))
    }
}

/// What this device is ([`INFO`]): its key, its host name, and the people whose certificates
/// in `place` name it, an empty list before anyone's does.
pub(crate) fn device_info(place: &Place, key: &SigningKey) -> Value {
    let device = key.verifying_key().to_bytes();
    let people: Vec<String> = tilo_device_identity::login::certified_by(place, &device)
        .iter()
        .map(hex::encode)
        .collect();
    json!({
        "device": hex::encode(device),
        "label": tilo_paths::session::host_name(),
        "people": people,
    })
}

/// [`DEV_ID`]'s answer for `args`, when `caller` may ask it: the admission that installs a
/// development build on this device is the one that names the id it runs under.
pub(crate) fn dev_id_answer(
    caller: &tilo_actions::remote::Caller,
    personal: bool,
    args: &Value,
    key: &SigningKey,
) -> Result<Value, ActionCallError> {
    package_actions::dev_build_admitted(caller, personal).map_err(refused)?;
    dev_id(args, key).map_err(refused)
}

/// The id `app`'s development build runs under on this device, as [`DEV_ID`] answers it: the app
/// renamed by the marking `tilo_app_package::pack::pack_dev` gives the build it signs with this
/// device's `key`. The same derivation, so a build compiled for it runs as the device installed
/// it.
fn dev_id(args: &Value, key: &SigningKey) -> Result<Value, String> {
    let app = args
        .get("app")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|app| !app.is_empty())
        .ok_or_else(|| {
            "a development build's id is asked for the app it is a build of (`app`)".to_owned()
        })?;
    Ok(json!({
        "app": tilo_app_package::dev_app_id(app, &key.verifying_key().to_bytes()),
    }))
}

fn refused(reason: impl Into<String>) -> ActionCallError {
    ActionCallError::Refused {
        reason: reason.into(),
    }
}

/// The calling account's root, which signs the device's actions that only it may sign: the
/// device's refusal where it keeps none.
fn rooted_or_refused(
    rooted: Option<(std::path::PathBuf, super::signing::Rooted)>,
) -> Result<(std::path::PathBuf, super::signing::Rooted), ActionCallError> {
    rooted.ok_or_else(|| refused("this device keeps no root to sign the call with"))
}

pub(crate) fn succeeded(invocation: InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

pub(crate) fn failed(invocation: InvocationId, message: &str) -> InvocationUpdate {
    failed_as(invocation, "device-action", message)
}

/// The device action `invocation` failed, with the refusal's stable `code`.
pub(crate) fn failed_as(invocation: InvocationId, code: &str, message: &str) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
            retryable: false,
        },
    }
}

/// The ticket, the agent and the label a `devices#logIn` asks with.
pub(crate) fn login_asked(args: &Value) -> Result<(&str, &str, Option<&str>)> {
    let text = |name: &str| args.get(name).and_then(Value::as_str);
    let ticket = text("ticket").context("a ticket is needed")?;
    let agent = text("agent")
        .filter(|agent| !agent.trim().is_empty())
        .context("the agent that runs on this computer is needed")?;
    Ok((ticket, agent, text("label")))
}

pub(crate) use tilo_api::daemon::enter::logged_in;

/// Whether `endpoint` names where it is reached from another network: one of its relays answers
/// it, or this device has none to wait for.
fn reached(endpoint: &iroh::Endpoint) -> bool {
    endpoint.addr().relay_urls().next().is_some()
        || tilo_endpoint_reachability::relays().relays.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read answers the id the device's own marking derives, to the person on its own
    /// connection, and nothing to whoever may not install a development build.
    #[test]
    fn the_dev_id_read_answers_where_a_development_build_is_admitted() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let app = "org.example.notes";
        let args = json!({ "app": app });
        let expected = tilo_app_package::dev_app_id(app, &key.verifying_key().to_bytes());
        for personal in [true, false] {
            let answer = dev_id_answer(
                &tilo_actions::remote::Caller::own_person(String::from("person")),
                personal,
                &args,
                &key,
            )
            .expect("the person on this device's own connection asks");
            assert_eq!(answer["app"].as_str(), Some(expected.as_str()));
            assert!(
                dev_id_answer(
                    &tilo_actions::remote::Caller::local_agent(
                        String::from("an-agent"),
                        String::from("person"),
                    ),
                    personal,
                    &args,
                    &key,
                )
                .is_err(),
                "an agent asks for no development build's id"
            );
        }
        assert!(
            dev_id_answer(
                &tilo_actions::remote::Caller::local_system(),
                false,
                &args,
                &key,
            )
            .is_err(),
            "the system daemon names no development build's id for an account"
        );
    }
}
