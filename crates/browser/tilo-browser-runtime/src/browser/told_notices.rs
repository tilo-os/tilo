//! What the workspace's server told the person, on this page: each mention of them, wherever in
//! the workspace and whichever window the page shows.
//!
//! The server keeps one log a person of what it told them
//! ([`tilo_actions::remote::person_telling`]) and tells into it whatever is addressed to them, so
//! every device of theirs reads the same things. This page follows that log over the session its
//! peer holds with the server: what was told while it was away waits under the bell, what is told
//! while it is open comes as a banner too, and what the person opened or another device of theirs
//! settled is taken back. Choosing one opens the place it is about, as any open goes, and tells
//! the server it was opened, which takes it back on every other device.

use std::cell::RefCell;
use std::collections::HashMap;

use tilo_actions::remote::person_telling::{
    self, Acknowledge, BY_OPENED, Kind, ListAfter, MAX_LIST_WAIT_MS, Told, ToldList,
};
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget, UpdateMark,
    settle_with,
};
use tilo_desktop_surface::DesktopHandle;
use tilo_service::notice::{Place, TAGS};
use tilo_session_actions::SessionActions;

use super::Desktop;
use crate::browser::sleep_ms;
use tilo_desktop_search::InvokeTarget;

/// The longest wait before a server that did not answer is asked again.
const RETRY_MAX_MS: u32 = 30_000;

/// One thing shown, by the activation that opens it.
struct Shown {
    peer: u32,
    workspace: String,
    person: String,
    told: String,
}

thread_local! {
    /// The logs this page follows, by peer and workspace: each once.
    static FOLLOWED: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
    /// What the page shows of the logs, by the activation of the place each opens.
    static SHOWN: RefCell<HashMap<String, Shown>> = RefCell::default();
}

/// Follow what the server the page's peer `peer` holds `workspace` with tells `person`, showing it
/// through the page's notifications; a page with no peer follows nothing. Once per peer and
/// workspace.
pub(crate) fn follow(peer: Option<u32>, workspace: &str, person: &str, shell: &DesktopHandle) {
    let Some(peer) = peer else {
        return;
    };
    let Ok(id) = crate::peer::workspace_id(workspace) else {
        return;
    };
    let key = (peer, workspace.to_owned());
    if FOLLOWED.with_borrow(|followed| followed.contains(&key)) {
        return;
    }
    FOLLOWED.with_borrow_mut(|followed| followed.push(key.clone()));
    // The same server renders the person's views where no device of theirs does: it is told
    // the formats and the zone they read dates in.
    super::person_formats::follow(peer, id, workspace, person);
    // What a notice opens comes back through the page's notices.
    let _ = crate::host_services::notices();
    let (workspace, person, shell) = (workspace.to_owned(), person.to_owned(), shell.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let caller = Caller::local_person(person.clone()).in_workspace(workspace.clone());
        let (mut after, mut first, mut retry) = (0, true, 1_000);
        while let Some(api) = crate::peer_sync::actions_of(peer, id) {
            let asked = ListAfter {
                after,
                wait_ms: (!first).then_some(MAX_LIST_WAIT_MS),
                device: String::new(),
            };
            let Some(list) =
                ask::<ToldList>(&api, &caller, telling(person_telling::LIST), &asked).await
            else {
                sleep_ms(retry).await;
                retry = retry.saturating_mul(2).min(RETRY_MAX_MS);
                continue;
            };
            retry = 1_000;
            for told in list.told.iter().filter(|told| about_workspace(told, id)) {
                show(told, first, &shell, (peer, &workspace, &person));
            }
            after = after.max(list.newest);
            first = false;
        }
        FOLLOWED.with_borrow_mut(|followed| followed.retain(|followed| *followed != key));
    });
}

/// The person chose `place` from a notice: what told it learns it was opened.
pub(crate) fn opened(place: &Place) {
    let Some(shown) = SHOWN.with_borrow_mut(|shown| shown.remove(&place.activation())) else {
        return;
    };
    let Ok(id) = crate::peer::workspace_id(&shown.workspace) else {
        return;
    };
    let Some(api) = crate::peer_sync::actions_of(shown.peer, id) else {
        return;
    };
    let caller = Caller::local_person(shown.person).in_workspace(shown.workspace);
    let opened = Acknowledge {
        told: shown.told,
        device: String::new(),
        by: BY_OPENED.to_owned(),
    };
    wasm_bindgen_futures::spawn_local(async move {
        let _ =
            ask::<serde_json::Value>(&api, &caller, telling(person_telling::ACKNOWLEDGE), &opened)
                .await;
    });
}

/// Whether `told` is about the workspace `id`: what is about another opens from that one's page.
fn about_workspace(told: &Told, id: tilo_server_protocol::WorkspaceId) -> bool {
    told.about
        .as_ref()
        .is_some_and(|about| tilo_server_protocol::WorkspaceId::parse(&about.workspace) == Some(id))
}

/// Where opening `told` leads: the item it is about, through its record's resource.
fn place_of(told: &Told) -> Option<Place> {
    let about = told.about.as_ref()?;
    let kind = about.resource.split('/').next().unwrap_or_default();
    Some(Place {
        app: about.app.clone(),
        record: about.record.clone(),
        resource: kind.to_owned(),
        anchor: about.resource.clone(),
    })
}

/// Show `told`, a mention, or take it back once it waits no more. What was told before the page
/// came waits under the bell; what arrives while it is open comes as a banner too, unless a
/// window shows the very item it is about.
fn show(
    told: &Told,
    before: bool,
    shell: &DesktopHandle,
    (peer, workspace, person): (u32, &str, &str),
) {
    if told.kind != Kind::Mention {
        return;
    }
    let tag = format!("{TAGS}told/{}", told.id);
    let notifications = ui::process::current().notifications;
    let Some(place) = place_of(told) else {
        return;
    };
    let activation = place.activation();
    if !told.waits() {
        SHOWN.with_borrow_mut(|shown| shown.remove(&activation));
        if let Err(error) = notifications.withdraw(&tag) {
            tracing::debug!(%error, "a settled mention was not taken back");
        }
        return;
    }
    let seen = shell.shows_item(&place.app, &place.anchor);
    if seen && !before {
        return;
    }
    let mut notice = ui::notify::Notification::new(&tag, &told.title, &told.words)
        .from_app(&tilo_desktop_search::names::app_name(&place.app))
        .with_activation(activation.clone())
        .with_action(tilo_desktop_surface::i18n::t("notification-open"));
    if before {
        notice = notice.quietly();
    }
    if let Err(error) = notifications.show(&notice) {
        tracing::debug!(%error, "a mention was not shown");
        return;
    }
    SHOWN.with_borrow_mut(|shown| {
        shown.insert(
            activation,
            Shown {
                peer,
                workspace: workspace.to_owned(),
                person: person.to_owned(),
                told: told.id.clone(),
            },
        )
    });
}

/// The person's log's `action`.
fn telling(action: &str) -> RemoteActionTarget {
    person_telling::target(person_telling::TOLD, action)
}

/// Ask what the server keeps for the person `target` with `argument`, as the person, and read
/// what it answers.
pub(super) async fn ask<T: serde::de::DeserializeOwned>(
    api: &SessionActions,
    caller: &Caller,
    target: RemoteActionTarget,
    argument: &impl serde::Serialize,
) -> Option<T> {
    let serde_json::Value::Object(argument) = serde_json::to_value(argument).ok()? else {
        return None;
    };
    let invocation = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let mut receipt = api.invoke(caller, request).await?.ok()?;
    let now = tilo_clock::now();
    let until = now
        .checked_add(std::time::Duration::from_millis(MAX_LIST_WAIT_MS + 20_000))
        .unwrap_or(now);
    let mut seen = UpdateMark::Accepted;
    while matches!(receipt.status, ReceiptStatus::Accepted) {
        if tilo_clock::now() >= until {
            return None;
        }
        let state = settle_with(api, caller, invocation, Some(seen), until)
            .await
            .flatten()?;
        seen = UpdateMark::of(&state.update);
        receipt = state.receipt();
    }
    let ReceiptStatus::Completed { output } = receipt.status else {
        return None;
    };
    serde_json::from_value(output.value.unwrap_or_default()).ok()
}

impl Desktop {
    /// Follow what the person is told, once the page's first frame is up and the code that
    /// follows it is in ([`crate::TOLD_NOTICES`]); open what they chose from a notice, and what
    /// the desktop asked to see. What was told before the code came waits in the person's log.
    pub(super) fn tell_mentions(&self) {
        if super::first_frame_presented()
            && let Some(told) = crate::TOLD_NOTICES.get()
        {
            told.follow(
                self.asks.peer(),
                &self.workspace,
                &self.author,
                &self.handle,
            );
        }
        self.open_chosen_mentions();
        self.show_asked_in_apps();
    }

    /// Open, as this person, each place they chose from a mention's notice: the app on the record,
    /// showing the item the mention is in.
    fn open_chosen_mentions(&self) {
        use tilo_session::platform::notice::Notices as _;
        for place in crate::host_services::notices().take_chosen() {
            if let Some(told) = crate::TOLD_NOTICES.get() {
                told.opened(&place);
            }
            self.open(&place);
        }
    }

    /// Show, as this person, what the desktop asked to see in an app's window
    /// ([`crate::shell::HostRequest::ShowInApp`]): through the session, which opens the window and
    /// shows it there, as a notice's place; and each link it asked an app to show
    /// ([`crate::shell::HostRequest::OpenLink`]): the app's `open-link`, the same way.
    fn show_asked_in_apps(&self) {
        let mut asked = Vec::new();
        let mut links = Vec::new();
        self.requests.borrow_mut().retain(|request| match request {
            crate::shell::HostRequest::ShowInApp(place) => {
                asked.push(place.clone());
                false
            }
            crate::shell::HostRequest::OpenLink {
                app,
                resource,
                link,
            } => {
                links.push(
                    InvokeTarget::action(
                        app.clone(),
                        self.workspace.as_str(),
                        resource.clone(),
                        tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
                    )
                    .with_argument(tilo_actions::remote::OPEN_LINK_PARAM, link.clone()),
                );
                false
            }
            _ => true,
        });
        for place in asked {
            self.open(&place);
        }
        for link in links {
            self.invoke_as_person(&link);
        }
    }

    /// Ask the session to open `place` as this person, as a search hit opens what it found.
    pub(super) fn open(&self, place: &Place) {
        self.invoke_as_person(&InvokeTarget::place(self.workspace.as_str(), place));
    }
}
