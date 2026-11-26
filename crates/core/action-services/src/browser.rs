//! A workspace's Browser, started on the member device that runs
//! the workspace's VM (`docs/hosted-browser.md`, "Where it runs").
//!
//! A workspace hosted on a server opens Browser like any app, and the page has
//! to run somewhere: in the workspace's own VM, on the host that
//! keeps that VM. The server asks that host's member device over
//! the device action service, as the device it is, the way it asks for a shell command
//! ([`environment_commands`](crate::environment_commands)):
//!
//! - [`target`] names a Browser open, an action of the workspace's
//!   environment ([`RESOURCE`]); the argument is an [`OpenBrowser`].
//! - It succeeds with an [`OpenedBrowser`] once the VM's daemon logged in with
//!   the ticket the material carries: what starting it took, whether this call
//!   started it or found it running, and the VM's device. The window itself is
//!   opened and read on that device ([`crate::environment_window`]). A host with
//!   no room answers
//!   [`Unavailable`](tilo_actions::remote::ActionCallError::Unavailable) with the words to
//!   wait on.
//! - [`STOP`] ends the VM's daemon and its sessions; the VM then sleeps like any
//!   idle VM.
//!
//! [`MARK`] is set in the environment of the session the daemon starts for
//! Browser, so the session knows it holds a hosted Browser.
//!
//! Only the workspace's host asks, as a device of the operator the member's
//! own identity belongs to: the workspace it opens is the caller's, never one
//! the request names.

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;

/// The provider a member answers the workspaces' Browser under.
pub const PROVIDER: &str = "tilo.environment";
/// The resource opening one is an action of: the workspace's environment.
pub const RESOURCE: &str = "environment";
/// Start the browser session in a workspace's VM, or attach to it.
pub const OPEN: &str = "open-browser";
/// The page action that lets a held request leave, by the id the window
/// published it with: what a departure nothing asked for is answered with.
pub const SEND_HELD: &str = "send-held-request";
/// The page action that drops a held request, leaving the page as it was.
pub const KEEP_HELD: &str = "keep-held-request";
/// The prefix of the context facets a held departure is reported under:
/// `browser.departure.page`, `.kind`, `.method`, `.destination` and
/// `.picture`. The window states them on the action it publishes and the
/// session holding it reports them, so the person asked about a departure
/// sees what leaves and from where, not only a label for it.
pub const DEPARTURE_FACET: &str = "browser.departure";

/// The facet stating one part of a departure: `name` as a person's question
/// reads it (`page`, `kind`, `method`, `destination`, `picture`) and its
/// `value`.
#[must_use]
pub fn departure_facet(name: &str, value: impl Into<String>) -> tilo_actions::ActionFacet {
    tilo_actions::ActionFacet::new(format!("{DEPARTURE_FACET}.{name}"), value)
}

/// Every departure fact `facets` state, by the name each is stated under, in
/// the order the facets are written.
#[must_use]
pub fn stated_facts(facets: &[tilo_actions::ActionFacet]) -> Vec<(String, String)> {
    let prefix = format!("{DEPARTURE_FACET}.");
    facets
        .iter()
        .filter_map(|facet| {
            Some((
                facet.key.strip_prefix(&prefix)?.to_owned(),
                facet.value.clone(),
            ))
        })
        .collect()
}

/// Every departure facet `context` carries, by the name it states it under,
/// in the order the facets are written.
#[must_use]
pub fn departure_facts(context: Option<&tilo_actions::ActionContext>) -> Vec<(String, String)> {
    context.map_or_else(Vec::new, |context| stated_facts(&context.facets))
}

/// What a person is shown about a departure, from the facts it states
/// ([`stated_facts`]): each by the name it is stated under.
#[must_use]
pub fn departure_asked(facts: Vec<(String, String)>) -> tilo_actions::remote::asked::AskedFacts {
    // The page it leaves from is what the question is about, as the resource a call names.
    let on = facts
        .iter()
        .find(|(name, _)| name == "page")
        .map(|(_, value)| value.clone());
    tilo_actions::remote::asked::AskedFacts {
        on,
        facts: facts
            .into_iter()
            .map(|(label, value)| tilo_actions::remote::asked::Fact { label, value })
            .collect(),
        ..tilo_actions::remote::asked::AskedFacts::default()
    }
}

/// Set in the environment of the session a workspace VM's daemon starts for Browser.
pub const MARK: &str = "TILO_HOSTED_BROWSER";

/// What [`OPEN`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, OPEN),
    }
}

/// A workspace's Browser to open, and what its session joins the
/// workspace with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenBrowser {
    /// The workspace whose Browser it opens: the caller's.
    pub workspace: String,
    /// Who the machine's time is booked to, as the server knows the account.
    #[serde(default)]
    pub account: String,
    /// The host's placement (`tilo_server_protocol::hosted::Material`): the
    /// workspace, the record to open and the ticket the VM's daemon logs in
    /// with, in JSON.
    pub material: String,
}

/// A workspace VM's daemon, logged in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedBrowser {
    /// What starting it took, when this call started it; absent when it was
    /// running already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_ms: Option<u64>,
    /// The VM's device, its key in hex: where its sessions are launched and
    /// read ([`crate::environment_window`]).
    pub device: String,
}

/// The host that placed a window, as the window's own session was handed it: the device that
/// certified the window's device and holds the link page actions reach it over, and the
/// workspace it placed the window in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The placing host's device key.
    pub host: [u8; 32],
    /// The workspace the window was placed in.
    pub workspace: String,
}

/// Whether `confirmation`, forwarded with `caller`'s call to a window `placed` by its host, is
/// the person's answer for the call. A key its person let operate web pages is answered for by the host that
/// placed the window: that host hands the key's own signed call over its link with
/// [`Confirmation::Operated`](tilo_actions::remote::Confirmation::Operated), and only that host, for only
/// the workspace it placed the window in, answers so. `Ok(false)` where the caller carries its
/// own word, which the action's policy decides on.
///
/// # Errors
/// [`ActionCallError::NotPermitted`](tilo_actions::remote::ActionCallError::NotPermitted) for a confirmation
/// another device forwarded, one forwarded for another workspace, or one reaching a window no host
/// placed: nobody else answers for the person here.
pub fn host_answered(
    placed: Option<&Placement>,
    caller: &tilo_actions::remote::Caller,
    confirmation: Option<&tilo_actions::remote::Confirmation>,
) -> Result<bool, tilo_actions::remote::ActionCallError> {
    let (Some(tilo_actions::remote::Confirmation::Operated), Some(forwarder)) =
        (confirmation, caller.forwarder())
    else {
        return Ok(false);
    };
    let Some(placed) = placed else {
        tracing::warn!(
            forwarder = %hex::encode(forwarder),
            "a confirmation another device forwarded reached a device no host placed, so it is refused"
        );
        return Err(tilo_actions::remote::ActionCallError::NotPermitted);
    };
    let by_host = *forwarder == placed.host;
    let in_workspace = caller.workspace() == Some(placed.workspace.as_str());
    if !(by_host && in_workspace) {
        tracing::warn!(
            forwarder = %hex::encode(forwarder),
            by_host,
            in_workspace,
            "a page action's confirmation came from another device than the host that placed this window, or for another workspace, so it is refused"
        );
        return Err(tilo_actions::remote::ActionCallError::NotPermitted);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The placing host's word, for the workspace it placed the window in, answers for the
    /// person; the same word from any other device, for any other workspace, or reaching a
    /// window no host placed, is refused. A caller's own word is its policy's to decide.
    #[test]
    fn only_the_placing_host_answers_for_the_person_at_its_window() {
        use {
            tilo_actions::remote::ActionCallError, tilo_actions::remote::Caller,
            tilo_actions::remote::Confirmation,
        };
        let host = [1; 32];
        let placed = Placement {
            host,
            workspace: "ws".into(),
        };
        let agent = Caller::authenticated([7; 32], [8; 32]).in_workspace("ws");
        let confirmed = Some(&Confirmation::Operated);
        assert_eq!(
            host_answered(Some(&placed), &agent.clone().forwarded_by(host), confirmed),
            Ok(true)
        );
        for (caller, placed) in [
            (agent.clone().forwarded_by([2; 32]), Some(&placed)),
            (
                Caller::authenticated([7; 32], [8; 32])
                    .in_workspace("other")
                    .forwarded_by(host),
                Some(&placed),
            ),
            (agent.clone().forwarded_by(host), None),
        ] {
            assert_eq!(
                host_answered(placed, &caller, confirmed),
                Err(ActionCallError::NotPermitted),
                "{caller:?}"
            );
        }
        assert_eq!(host_answered(Some(&placed), &agent, confirmed), Ok(false));
        assert_eq!(
            host_answered(Some(&placed), &agent.forwarded_by(host), None),
            Ok(false)
        );
    }

    #[test]
    fn an_opened_vm_names_its_device() {
        // A daemon that was running says nothing about time, but always names its device.
        assert_eq!(
            serde_json::to_string(&OpenedBrowser {
                opened_ms: None,
                device: "ab".into()
            })
            .unwrap(),
            r#"{"device":"ab"}"#
        );
        assert_eq!(
            serde_json::to_string(&OpenedBrowser {
                opened_ms: Some(900),
                device: "ab".into()
            })
            .unwrap(),
            r#"{"opened_ms":900,"device":"ab"}"#
        );
    }
}
