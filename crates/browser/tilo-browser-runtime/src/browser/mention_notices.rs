//! Telling this page's person when someone mentions them, wherever in the workspace.
//!
//! The records are what this page's replication took in from the workspace
//! ([`crate::replication::take_touched`], [`crate::replication::record_document`]), so a record
//! open in a window or not is looked at as the change reaches it, and the notices are the
//! session's own role ([`tilo_session::mentions`]) shown through this host's notifications
//! ([`tilo_session::platform::notice::Toolkit`]). Choosing one opens the place the mention is in,
//! as any open goes: through the desktop's session, as this person.

use std::rc::Rc;

use tilo_actions::remote::{ActionHost as _, InvokeArgument, InvokeRequest};
use tilo_session::mentions::Mentions;
use tilo_session::platform::notice::{Place, Toolkit, open_target};
use tilo_session::platform::records::{Changed, Records};
use ui::automerge::AutoCommit;

use super::Desktop;

/// The records this page holds from the workspace it replicated, as the mention role reads them.
struct Taken {
    /// The workspace's peer on this page, as replication names it.
    peer: u32,
    /// The kind of the records an app keeps, as the page's own bundle declares it.
    kind: Rc<dyn Fn(&str) -> String>,
}

impl Records for Taken {
    fn take_changed(&self) -> (bool, Vec<Changed>) {
        let (ready, touched) = crate::replication::take_touched(self.peer);
        (
            // What reached the record before the workspace finished replaying what it held is
            // what the device already had.
            !ready,
            touched
                .into_iter()
                .map(|(app, record)| Changed {
                    workspace: String::new(),
                    app,
                    record,
                })
                .collect(),
        )
    }

    fn document(&self, app: &str, _workspace: &str, record: &str) -> Option<AutoCommit> {
        crate::replication::record_document(self.peer, app, record)
    }

    fn kind(&self, app: &str, _workspace: &str) -> String {
        (self.kind)(app)
    }
}

/// This page's mentions of `person` in the workspace `peer` replicated into `store`, drawn by the
/// frame `wake` asks for.
pub(super) fn role(
    store: tilo_session::platform::Store,
    peer: u32,
    person: &str,
    name: Rc<dyn Fn(&str) -> String>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Mentions {
    Mentions::over(tilo_session::mentions::Given {
        person: vec![person.to_owned()],
        store: Some(store),
        records: Some(Rc::new(Taken {
            peer,
            kind: Rc::new(|app| {
                crate::desktop::application(app)
                    .map(|definition| definition.record_kind.as_str().to_owned())
                    .unwrap_or_default()
            }),
        })),
        notices: Some(Rc::new(Toolkit::new(
            ui::host::current().notifications,
            ui::mentions::TAGS,
        ))),
        name,
        wake: Some(wake),
    })
}

impl Desktop {
    /// Look for new mentions of this person in what reached the page, and open what they chose
    /// from a notice.
    pub(super) fn tell_mentions(&self) {
        self.mentions.borrow().step();
        self.open_chosen_mentions();
        self.show_asked_in_apps();
    }

    /// Open, as this person, each place they chose from a mention's notice: the app on the record,
    /// showing the item the mention is in.
    fn open_chosen_mentions(&self) {
        for place in self.mentions.borrow().take_chosen() {
            self.open(open_target(&self.workspace, &place));
        }
    }

    /// Show, as this person, what the desktop asked to see in an app's window
    /// ([`crate::shell::HostRequest::ShowInApp`]): the app's `open` of the resource, through the
    /// session, which opens the window and shows it there.
    fn show_asked_in_apps(&self) {
        let mut asked = Vec::new();
        self.requests.borrow_mut().retain(|request| match request {
            crate::shell::HostRequest::ShowInApp { app, resource } => {
                asked.push((app.clone(), resource.clone()));
                false
            }
            _ => true,
        });
        for (app, resource) in asked {
            let place = Place {
                app,
                record: String::new(),
                resource,
            };
            self.open(open_target(&self.workspace, &place));
        }
    }

    /// Ask the session to open `target` as this person, and draw the frame that shows what it
    /// opened. A refused open is the session's to report.
    fn open(&self, target: tilo_actions::remote::RemoteActionTarget) {
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target,
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        drop(self.session.invoke(&self.caller(), request));
        self.shared.wake();
    }
}
