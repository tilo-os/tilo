//! The window's two views: the root, and what a row or `open(r)` opened beside it.
//!
//! A view is the app's tree for a resource, run in the runtime and held with the rows of its
//! virtual lists and the values its bound controls show. This is what opens one into a lane,
//! keeps the window's copy of it up to date (trees and rows), and closes it: what the person
//! did to it is the instance's ([`super::Instance::event`]), and what it asked for is followed
//! in [`super::Instance`]'s frame.

use std::rc::Rc;

use tilo_render_protocol::patch::{self as ui, Lane, ToUi, Words};
use tilolang_runtime::store::StoreError;

use super::rows::{FIRST_ROWS, ListRows};
use super::wire;
use super::{
    HistoryAsk, Instance, NoNames, Shown, bound_values, convert_with, patch_in_place, plain,
    referenced_resource, rows_message, show_content_rows, sort, virtual_list_paths, virtual_lists,
};
use crate::host::Host;
use crate::runtime::Store as _;
use crate::runtime::{ResourceRef, ViewEvent};

impl Instance {
    /// Open the package's root view.
    pub(super) fn open_root(&mut self, host: &Host) {
        let target = match host.root_of_layer(&self.app, &self.root_view) {
            Ok(target) => target,
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app has no root to open");
                return;
            }
        };
        match host.open_layer(&self.app, &self.root_view) {
            Ok(view) => {
                self.root = Some(Shown::new(target, self.spaced(Lane::Root, view)));
                if self.detail.take().is_some() {
                    self.out.push(ToUi::Close);
                }
                self.send_open(Lane::Root, None);
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Show `target` in the detail view: what `open(r)` asked for, or a row. With `front`, a
    /// narrow window brings it to the front.
    /// Open what the reference `reference` (a `mention` mark's value) in rich text refers to:
    /// a resource of a package here that the viewer holds, in the detail lane.
    pub(super) fn open_reference(&mut self, reference: &str) {
        use crate::runtime::Store as _;
        let Some(reference) = crate::runtime::rich::Reference::of_mark(reference) else {
            return;
        };
        let Some(host) = self.host() else {
            return;
        };
        let Some(mut target) = referenced_resource(&host, &reference) else {
            return;
        };
        let Ok(ty) = host.store().type_of(&target) else {
            return;
        };
        target.ty = ty;
        self.open_detail(&target, None, Some(ui::Front::Input));
    }

    pub(super) fn open_detail(
        &mut self,
        target: &ResourceRef,
        with: Option<&str>,
        front: Option<ui::Front>,
    ) {
        let Some(host) = self.host() else {
            return;
        };
        if self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.target == *target && with.is_none())
        {
            if let Some(front) = front {
                self.out.push(ToUi::Front(front));
            }
            return;
        }
        match host.open_resource(target, with) {
            Ok(view) => {
                let mut view = Shown::new(target.clone(), self.spaced(Lane::Detail, view));
                view.with = with.map(str::to_owned);
                if let Some(shown) = self.detail.replace(view) {
                    self.seen_in_window(&shown.target);
                }
                self.lenses_changed = true;
                self.send_open(Lane::Detail, front);
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(%error, "a resource did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Show `resource` in the detail lane with the view `view` (an id) rather than its type's
    /// window: what `open(r, with: v)` asks for, for a view the system shows on a surface of
    /// its own (a row view as a search result, a proposal view in its card), which
    /// `tilolang layout-check` lays out as it lays out a window.
    pub fn open_with(&mut self, resource: &ui::ResourceRef, view: &str) {
        if let Some(target) = wire::runtime_resource(resource) {
            self.open_detail(&target, Some(view), None);
        }
    }

    /// Show `target` where the window's list shows it: what the root view answers for an item of
    /// its type (`on show`) in its own state, so a list opens the item's row where it stands and
    /// the window follows the item to the place it belongs to. What the view does not open in
    /// place — an item of a type it does not list — opens in the detail lane, as it always has.
    pub(super) fn show_in_window(&mut self, target: &ResourceRef, front: Option<ui::Front>) {
        let shown = self
            .root
            .as_ref()
            .is_some_and(|root| match root.view.borrow().show(target) {
                Ok(shown) => shown,
                Err(error) => {
                    tracing::warn!(%error, "a view did not show an item");
                    false
                }
            });
        if shown {
            if let Some(front) = front {
                self.out.push(ToUi::Front(front));
            }
            return;
        }
        self.open_detail(target, None, front);
    }

    /// Close the detail view: Back.
    pub fn close_detail(&mut self) {
        if let Some(shown) = self.detail.take() {
            self.seen_in_window(&shown.target);
            self.lenses_changed = true;
            self.out.push(ToUi::Close);
        }
    }

    /// The person had `target` open in a window of its own, and goes elsewhere: what it showed
    /// is seen (task-legibility §4). While the window is open its marks stay, so the person
    /// sees what changed where it changed.
    pub(super) fn seen_in_window(&mut self, target: &ResourceRef) {
        let Some(host) = self.host() else {
            return;
        };
        let person = host.viewer().0.clone();
        if let Err(error) = host.runtime().mark_seen(&person, target) {
            tracing::warn!(%error, "what a window showed could not be marked seen");
            self.notice(Words::phrase("tilolang-seen-failed"), ui::NoticeKind::Error);
        }
    }

    /// Send the window the view of `lane` as it is now: its tree, and the first rows of its
    /// virtual lists.
    pub(super) fn send_open(&mut self, lane: Lane, front: Option<ui::Front>) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        view.lists.clear();
        view.bound.clear();
        let tree = {
            let instance = view.view.borrow();
            convert_with(&host, &*names, &instance, |convert| {
                convert.node(instance.tree())
            })
        };
        view.sent = Some(tree.clone());
        let target = wire::resource(&view.target);
        self.out.push(ToUi::Open {
            lane,
            target,
            tree,
            front,
        });
        self.track_lists(lane);
        self.send_bound(lane);
    }

    /// Keep the rows of every virtual list of the view of `lane`, sending the window the first
    /// rows of each new one, and drop those of lists gone.
    pub(super) fn track_lists(&mut self, lane: Lane) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        let lists = virtual_lists(view.view.borrow().tree());
        let handle = Rc::clone(&view.view);
        view.lists
            .retain(|key, rows| lists.contains(key) && rows.reads(&handle, key));
        let mut sent = Vec::new();
        for list in lists {
            if view.lists.contains_key(&list) {
                continue;
            }
            let mut rows = ListRows::of(&handle, &list);
            let first = 0..FIRST_ROWS.min(rows.len());
            let requested = rows.request(first);
            sent.push(rows_message(
                &host, &*names, &handle, lane, &list, requested,
            ));
            view.lists.insert(list, rows);
        }
        self.out.extend(sent);
    }

    /// Whether the view of `lane` has nothing to run over: a resource the record does not
    /// name — what was removed, or an item a change has not brought here yet — has no view, and
    /// the window keeps the tree it has until the record holds it. A view of another app's
    /// resource runs over the reading the session publishes for it, which stands where the
    /// record would: it has something to run over from the first reading, and runs again when
    /// that reading moves.
    fn unheld(&self, host: &Host, lane: Lane) -> bool {
        self.lane(lane).is_some_and(|shown| {
            matches!(
                host.store().type_of(&shown.target),
                Err(StoreError::NotFound(_))
            ) && host.runtime().reading(&shown.target).is_none()
        })
    }

    /// Re-run the views whose reads changed, and send the window what changed: their trees'
    /// patches, and the changes of their virtual lists' rows. Whether anything did.
    pub(super) fn poll_views(&mut self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let names = Rc::clone(&self.names);
        let mut changed = false;
        for lane in [Lane::Root, Lane::Detail] {
            if self.unheld(&host, lane) {
                continue;
            }
            let Some(view) = self.lane_mut(lane) else {
                continue;
            };
            // What the view reads changed: the tree may stand as it was (a row's control reads
            // the item's field it is bound to), and the window still needs the values it shows.
            let stale = view.view.borrow().is_stale();
            let patches = match view.view.borrow_mut().poll_patches() {
                Ok(patches) => patches,
                Err(error) => {
                    // The failing call draws a failed state where it stands (`shell.md` §6,
                    // "Nothing can lock a person out"), and the window keeps the rest of the
                    // frame it has and runs again on what comes next. A test sees the error
                    // instead, so none passes over one.
                    tracing::warn!(%error, "a view failed to run");
                    debug_assert!(false, "a view failed to run: {error}");
                    continue;
                }
            };
            if patches.is_empty() {
                changed |= stale;
                continue;
            }
            changed = true;
            let lists = virtual_list_paths(view.view.borrow().tree());
            let sorted = sort(patches, &lists);
            let mut messages = Vec::new();
            let handle = Rc::clone(&view.view);
            for (list, changes) in sorted.lists {
                let Some(rows) = view.lists.get_mut(&list) else {
                    continue;
                };
                let row_changes = if changes.reread {
                    rows.reread()
                } else {
                    let mut structural = rows.apply(&changes.structure);
                    let changed = rows.changed(&changes.rows);
                    let instance = handle.borrow();
                    convert_with(&host, &*names, &instance, |convert| {
                        structural.extend(changed.iter().map(|row| ui::RowChange::Changed {
                            row: convert.node(row),
                        }));
                    });
                    structural
                };
                if row_changes.is_empty() {
                    continue;
                }
                let len = u32::try_from(rows.len()).unwrap_or(u32::MAX);
                messages.push(ToUi::RowsChanged {
                    lane,
                    list,
                    len,
                    changes: row_changes,
                });
            }
            if sorted.content {
                // A list inside the content may have rows the view has not rendered yet.
                show_content_rows(&mut view.view.borrow_mut());
                // Only what changed is converted, where it can be: a clock's minute converts
                // the clock's text, not the window.
                let in_place = match (&mut view.sent, &sorted.changed) {
                    (Some(sent), Some(changed)) => {
                        let instance = view.view.borrow();
                        convert_with(&host, &*names, &instance, |convert| {
                            patch_in_place(sent, instance.tree(), changed, convert)
                        })
                    }
                    _ => None,
                };
                let patches = in_place.unwrap_or_else(|| {
                    let tree = {
                        let instance = view.view.borrow();
                        convert_with(&host, &*names, &instance, |convert| {
                            convert.node(instance.tree())
                        })
                    };
                    let mut patches = Vec::new();
                    match &view.sent {
                        Some(sent) => wire::diff(sent, &tree, &mut patches),
                        None => patches.push(ui::Patch::SetRoot(tree.clone())),
                    }
                    view.sent = Some(tree);
                    patches
                });
                if !patches.is_empty() {
                    self.out.push(ToUi::Patch { lane, patches });
                }
            }
            self.out.extend(messages);
            if sorted.content {
                self.track_lists(lane);
            }
        }
        if changed {
            self.send_bound(Lane::Root);
            self.send_bound(Lane::Detail);
        }
        changed
    }

    /// The opens the views asked for.
    pub(super) fn follow_opens(&mut self) {
        let mut opens = Vec::new();
        for view in [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
        {
            opens.extend(view.view.borrow_mut().poll_events());
        }
        for event in opens {
            match event {
                ViewEvent::Open {
                    target,
                    view,
                    front,
                } => {
                    // A shell's places show resources on the canvas, never as a detail lane of
                    // the shell's own window: what a view opened is a place the session arranges
                    // (one-app-model.md §2, shell.md §8). Anywhere else, it opens the window's
                    // detail lane.
                    if self.place_opens {
                        self.opens_to_place.push((target, view, front));
                        continue;
                    }
                    // A selection the keyboard moves shows the item beside the list without
                    // bringing it to the front of a narrow window; an activation does.
                    let front = front.then_some(ui::Front::Input);
                    self.open_detail(&target, view.as_deref(), front);
                }
                // The run's log is the system's: the window's host shows it.
                ViewEvent::Review { run, .. } => self.out.push(ToUi::ShowRun { run }),
                // An undo a view applied is the record's, carried out as the window's undo is.
                ViewEvent::Undo(tilolang_runtime::UndoAsk::Step { change, .. }) => {
                    self.history.push(HistoryAsk::UndoStep(change));
                }
                ViewEvent::Undo(tilolang_runtime::UndoAsk::Run { run }) => {
                    self.history.push(HistoryAsk::UndoRun(run));
                }
                // One move marked a catch-up seen: the window says how much, and offers to take
                // it back while it says it.
                ViewEvent::Seen { count } => {
                    self.seen_offer = true;
                    self.out.push(ToUi::Notice {
                        words: Words::Phrase {
                            id: "tilolang-marked-seen".to_owned(),
                            args: vec![("count".to_owned(), count.to_string())],
                        },
                        kind: ui::NoticeKind::Info,
                        undo: true,
                    });
                }
            }
        }
    }

    /// The view of a shell places what it opens on the canvas, rather than opening a detail lane
    /// of the shell's own window: what a `show`'s view opens is a place the session arranges.
    pub fn place_opens(&mut self, place: bool) {
        self.place_opens = place;
    }

    /// What a view of a shell opened since the last call, each a resource and the view it named:
    /// the places the session arranges on the canvas.
    pub fn take_opens_to_place(&mut self) -> Vec<(ResourceRef, Option<String>, bool)> {
        std::mem::take(&mut self.opens_to_place)
    }

    /// Send the window what each bound control of the view of `lane` (and of the rows it
    /// shows) holds now, where that changed.
    pub(super) fn send_bound(&mut self, lane: Lane) {
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        let instance = Rc::clone(&view.view);
        let instance = instance.borrow();
        let mut now: Vec<(String, ui::Value)> = Vec::new();
        bound_values(instance.tree(), &instance, &mut now);
        for rows in view.lists.values() {
            for key in rows.shown() {
                if let Some(row) = instance.find(key) {
                    bound_values(row, &instance, &mut now);
                }
            }
        }
        let mut sent = Vec::new();
        for (key, value) in now {
            if view.bound.get(&key) == Some(&value) {
                continue;
            }
            view.bound.insert(key.clone(), value.clone());
            let acked = view.acked.get(&key).copied().unwrap_or(0);
            sent.push(ToUi::Bound {
                lane,
                key,
                value,
                acked,
            });
        }
        drop(instance);
        self.out.extend(sent);
    }

    /// Whether the detail view's bound controls hold anything, and where: a draft being typed
    /// in a conversation. Only whether, never what it says: presence carries "composing",
    /// never the words.
    #[must_use]
    pub fn composing_in(&self) -> Option<&ResourceRef> {
        let detail = self.detail.as_ref()?;
        // The view's own controls, as the bound values it holds: any text or rich text with
        // something in it is a draft under way.
        let holds = self.lane(Lane::Detail).is_some_and(|shown| {
            shown
                .bound
                .keys()
                .filter_map(|key| shown.view.borrow().bound_value(key))
                .any(|value| match value {
                    crate::runtime::Value::Text(text) => !text.trim().is_empty(),
                    crate::runtime::Value::Rich(rich) => !rich.written_text().trim().is_empty(),
                    _ => false,
                })
        });
        holds.then_some(&detail.target)
    }

    /// Tell the window what the bound control `key` of `lane` holds now that the edit it sent
    /// is applied: the value the app holds is what its next frame draws, and the control shows
    /// it until then only as the person's own touch, which a frame from the app replaces.
    pub(super) fn shown_by_window(&mut self, lane: Lane, key: &str) {
        let Some(value) = self
            .view(lane)
            .and_then(|view| view.borrow().bound_value(key))
        else {
            return;
        };
        let shown = plain(&NoNames).value(&value);
        let Some(view) = self.lane_mut(lane) else {
            return;
        };
        if view.bound.get(key) == Some(&shown) {
            return;
        }
        view.bound.insert(key.to_owned(), shown.clone());
        let acked = view.acked.get(key).copied().unwrap_or(0);
        self.out.push(ToUi::Bound {
            lane,
            key: key.to_owned(),
            value: shown,
            acked,
        });
    }
}
