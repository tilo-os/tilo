//! The keys a person approves for their programs (`account/keys#create`), certified where their
//! account is answered: the device that keeps their root.
//!
//! The API that is asked for a key makes its secret and asks this device, in the person's own
//! workspace, to certify the public half ([`tilo_api::keys::KeyAsked`]). The call always waits
//! for the person's yes: the workspace's dispatch keeps it as their question, which only they
//! answer, on a device that holds their identity, as they answer every other question that
//! waits for them. Once they approve, their root certifies the key here, and the API that asked
//! makes the key of it. Declined, nothing is certified.

use std::sync::Arc;

use serde_json::json;
use tilo_actions::remote::asked::{AskedFacts, approved_or_asks};
use tilo_actions::remote::{
    ActionCallError, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument,
    InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionKind, ActionParam};
use tilo_api::keys::{CERTIFY, CERTIFY_PROVIDER, CERTIFY_RESOURCE, KeyAsked};
use tilo_server_protocol::servers::person_workspace_id;
use tilo_workspace_service::workspaces::providers::{
    Answered, Answering, Providers, WorkspaceAnswers, WorkspaceCall,
};

/// The application whose grants decide the calls: the person's keys are their settings'.
const APP: &str = tilo_app_id::apps::SETTINGS;

/// The keys a person approved, as their account publishes them.
#[derive(tilo_resource::Row)]
#[row(kind = "approved-keys", label = "Keys you approved")]
struct KeyRow;

/// What the API asks of the device that answers for the account.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.keys")]
pub enum KeysAction {
    /// Make a key for one of your programs.
    #[effect(kind = "outside")]
    Certify,
}

/// What the device that answers for its person's account certifies of their keys.
pub(crate) struct AccountKeys {
    resources: Vec<tilo_actions::ResourceSpec>,
}

impl AccountKeys {
    pub(crate) fn new() -> Arc<Self> {
        let mut spec = tilo_resource::offer::<KeysAction>(tilo_resource::typed_resource::<KeyRow>(
            CERTIFY_RESOURCE,
            "Your keys",
        ));
        spec.app_id = Some(APP.to_owned());
        Arc::new(Self {
            resources: vec![spec],
        })
    }

    /// Answer in the workspaces this device holds from now on: a key's call reaches it through
    /// the workspace's dispatch, which keeps a call that waits for the person as their question.
    pub(crate) fn register(self: &Arc<Self>, providers: &Providers) {
        providers.answer_in_workspaces(
            CERTIFY_PROVIDER,
            APP,
            Arc::clone(self) as Arc<dyn WorkspaceAnswers>,
        );
    }
}

/// What the person reads before saying yes to `asked`.
fn question(asked: &KeyAsked) -> AskedFacts {
    let reach = if asked.scope.workspace_name().is_some() {
        crate::i18n::t("keys-certify-one-workspace")
    } else {
        crate::i18n::t("keys-certify-every-workspace")
    };
    AskedFacts {
        words: crate::i18n::t_args(
            "keys-certify-question",
            &[
                ("name", asked.name.as_str().into()),
                ("reach", reach.as_str().into()),
            ],
        ),
        on: Some(asked.name.clone()),
        effect: Some(ActionKind::Outside),
        ..AskedFacts::default()
    }
}

/// Certify the key `request` asks for, for a key of the person's own asking in their own
/// workspace, once they said yes: the certificate their root gives it.
async fn certify(
    call: &WorkspaceCall<'_>,
    request: &InvokeRequest,
) -> Result<serde_json::Value, ActionCallError> {
    let address = &request.target.address;
    if address.resource != CERTIFY_RESOURCE || address.action != CERTIFY {
        return Err(ActionCallError::UnknownAction);
    }
    // Only a key of the person's own asks for another, in their own workspace; nobody else's
    // call reaches their root.
    let person = call
        .certificate
        .map(|certificate| certificate.user_public_key)
        .filter(|person| {
            call.caller.user_public_key() == Some(person)
                && call.workspace.map(|workspace| workspace.to_string())
                    == Some(person_workspace_id(person))
        })
        .ok_or(ActionCallError::NotPermitted)?;
    let InvokeArgument::Json(fields) = &request.argument else {
        return Err(ActionCallError::invalid_request(
            "a key's ask names the key",
        ));
    };
    let asked: KeyAsked = serde_json::from_value(serde_json::Value::Object(fields.clone()))
        .map_err(|error| ActionCallError::invalid_request(error.to_string()))?;
    let key: [u8; 32] = hex::decode(&asked.key)
        .ok()
        .and_then(|key| key.try_into().ok())
        .ok_or_else(|| ActionCallError::invalid_request("the key is 32 bytes in hex"))?;
    approved_or_asks(request, || question(&asked))?;
    let rooted = super::services::root_record_for_person(person)
        .and_then(|record| super::signing::Rooted::at(&record))
        .map_err(|error| ActionCallError::refused(error.to_string()))?;
    let cert = tilo_server_client::certification::issue_agent_grant(
        &rooted.root,
        key,
        &asked.name,
        tilo_user_identity::DeviceCapabilities::own_key_grant(asked.scope.clone())
            .with_browser_pages(asked.browser_pages),
        asked.expires_at_ns,
    )
    .await
    .map_err(|error| ActionCallError::refused(error.to_string()))?;
    let cert = tilo_user_identity::canonical::canonical_bytes(&cert)
        .map_err(|error| ActionCallError::refused(error.to_string()))?;
    Ok(json!({ "cert": hex::encode(cert) }))
}

impl WorkspaceAnswers for AccountKeys {
    fn params(&self, target: &RemoteActionTarget) -> Option<Vec<ActionParam>> {
        if target.provider != CERTIFY_PROVIDER {
            return None;
        }
        tilo_actions::api::locate(&self.resources, &target.address)
            .map(|(_, action)| action.params().to_vec())
    }

    fn offer<'a>(
        &'a self,
        _call: WorkspaceCall<'a>,
        _query: &'a CatalogueQuery,
    ) -> Answering<'a, Vec<(CatalogueProvider, Vec<CatalogueRow>)>> {
        // The API asks it for a key of the person's: no catalogue lists it.
        Box::pin(std::future::ready(Vec::new()))
    }

    fn answer<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        request: &'a InvokeRequest,
    ) -> Answering<'a, Result<Answered, ActionCallError>> {
        Box::pin(async move { certify(&call, request).await.map(Answered::Value) })
    }
}
