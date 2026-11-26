//! The window's history: what the window bar's History shows.
//!
//! History is the system's, not an app's: a window shows the history of the resource it shows —
//! the one at its detail level, else the item its list stands open into, else the item its own
//! view shows in place, else its root — through the system's history view (`std.history`), which
//! reads the runtime's `steps` of the resource. The view is a written view
//! ([`WINDOW_HISTORY`]), the person's own as any view they write is, so it reads what they read
//! and takes back what they take back; no app places it on its pages. It stands at the window's
//! detail level, so a narrow window brings it to the front, and Back or a second press of the
//! bar's History closes it and shows again what the level showed before.

use tilo_render_protocol::patch::{self as ui, Level, ToUi, Words};

use super::{Instance, Shown};
use crate::runtime::{ResourceRef, UiNode, Value};

/// The view a window shows the history of what it shows in: the system's, of any resource.
pub const WINDOW_HISTORY: &str = "view history(of: host.Resource) {\n    std.history(of)\n}\n";

/// What the detail level showed before the window's history took its place: the resource and the
/// view it was shown with, shown again when the history closes. `None`: the level was closed.
pub(super) type Before = Option<(ResourceRef, Option<String>)>;

impl Instance {
    /// The window bar's History was pressed: show the history of what the window shows (`open`),
    /// or close it.
    pub(super) fn show_history(&mut self, open: bool) {
        if open {
            self.open_history();
        } else {
            self.close_history();
        }
    }

    fn open_history(&mut self) {
        if self.window_history.is_some() {
            return;
        }
        let Some(host) = self.host() else {
            return;
        };
        let before: Before = self
            .detail
            .as_ref()
            .map(|shown| (shown.target.clone(), shown.with.clone()))
            .or_else(|| {
                self.facts
                    .as_ref()
                    .map(|facts| (facts.target.clone(), None))
            });
        let target = match (&before, &self.root) {
            (Some((target, _)), _) => target.clone(),
            (None, Some(root)) => opened_row(root.view.borrow().tree())
                .or_else(|| root.in_place.clone())
                .unwrap_or_else(|| root.target.clone()),
            (None, None) => return,
        };
        let viewer = host.viewer().clone();
        let opened = host.runtime().open_written_view(
            &viewer,
            WINDOW_HISTORY,
            &viewer,
            vec![Value::resource(&target)],
        );
        match opened {
            Ok(opened) => {
                let shown = Shown::new(target, self.spaced(Level::Detail, opened));
                self.facts = None;
                self.detail = Some(shown);
                self.window_history = Some(before);
                self.lenses_changed = true;
                self.send_open(Level::Detail, Some(ui::Front::Input));
                self.out.push(ToUi::History(true));
            }
            Err(error) => {
                tracing::warn!(%error, "a window's history did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Close the window's history: the detail level shows what it showed before, or closes.
    pub(super) fn close_history(&mut self) {
        let Some(before) = self.window_history.take() else {
            return;
        };
        self.out.push(ToUi::History(false));
        self.detail = None;
        self.lenses_changed = true;
        match before {
            Some((target, with)) => self.open_detail(&target, with.as_deref(), None, 0),
            None => self.out.push(ToUi::Close),
        }
    }

    /// The window goes elsewhere while it shows a history: it shows it no longer, and its bar's
    /// History stands closed. What the detail level showed before it, if it showed one; the level
    /// still holds the history.
    pub(super) fn leave_history(&mut self) -> Option<Before> {
        let before = self.window_history.take()?;
        self.out.push(ToUi::History(false));
        Some(before)
    }
}

/// The item a list of `node` stands open into where its row stands (`std.list`'s open row): what the
/// window shows of it is that item, so its history is the one the window's History shows.
fn opened_row(node: &UiNode) -> Option<ResourceRef> {
    if node.kind == "ui.VirtualList" {
        let open = match node.props.get("open") {
            Some(Value::Opt(Some(open))) => Some(&**open),
            other => other,
        };
        if let Some(Value::Ref(item)) = open {
            return Some((*item.target).clone());
        }
    }
    node.children.iter().find_map(opened_row)
}
