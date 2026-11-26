//! The window's two views: the root, and what a row or `open(r)` opened beside it.
//!
//! A view is the app's tree for a resource, run in the runtime and held with the rows of its
//! virtual lists and the values its bound controls show. This is what opens one into a level,
//! keeps the window's copy of it up to date (trees and rows), and closes it: what the person
//! did to it is the instance's ([`super::Instance::event`]), and what it asked for is followed
//! in [`super::Instance`]'s frame.

use std::rc::Rc;

use tilo_render_protocol::patch::{self as ui, Level, ToUi, Words};
use tilolang_runtime::store::{StoreError, VersionKind};

use super::patches::Sorted;
use super::rows::{FIRST_ROWS, ListRows};
use super::wire;
use super::{
    HistoryAsk, Instance, Mode, Shown, bound_values, convert_with, facts, patch_in_place,
    referenced_resource, rows_message, show_content_rows, sort, virtual_list_paths, window_tree,
};
use crate::host::Host;
use crate::runtime::Store as _;
use crate::runtime::{NameHost, ResourceRef, ViewEvent};

impl Instance {
    /// The facts page, drawn again when what it says changed: the resource's fields, or what
    /// only the window knows (a member's name, a day in the person's format). Whether it did.
    pub(super) fn poll_facts(&mut self) -> bool {
        if !std::mem::take(&mut self.facts_stale) {
            return false;
        }
        let Some(app) = self.host() else {
            return false;
        };
        let Some(facts) = self.facts.as_mut() else {
            return false;
        };
        let details = self.platform.phrase("tilolang-detail", &[]);
        let Some(tree) = facts::page(app.runtime(), &facts.target, &details) else {
            return false;
        };
        let mut patches = Vec::new();
        wire::diff(&facts.sent, &tree, &mut patches);
        facts.sent = tree;
        if patches.is_empty() {
            return false;
        }
        self.out.push(ToUi::Patch {
            level: Level::Detail,
            patches,
        });
        true
    }

    /// Show the view `view` as the window's own, over no resource of its own: a screen a host
    /// shows by name, such as a phone's shell (`view phone()`), which takes nothing. What the
    /// window showed before is replaced, as opening another layer replaces it.
    pub fn open_screen(&mut self, view: &str) {
        self.root_view = crate::host::RootView {
            root: None,
            view: Some(view.to_owned()),
        };
        if let Some(host) = self.host() {
            self.open_root(&host);
        }
    }

    /// Open the package's root view where none is open yet, so what its window would show can be
    /// read ([`Self::shown_now`]): an instance that shows nothing opens no view of its own until
    /// something reads it, and keeps it current from then on as a window's instance does.
    pub fn open_to_read(&mut self) {
        if self.root.is_none()
            && self.open_failed.is_none()
            && let Some(host) = self.host()
        {
            self.open_root(&host);
        }
    }

    /// Why the package's root view did not open, when it did not.
    #[must_use]
    pub fn open_failed(&self) -> Option<&str> {
        self.open_failed.as_deref()
    }

    /// Open the package's root view.
    pub(super) fn open_root(&mut self, host: &Host) {
        match host.open_layer_at(&self.app, &self.root_view) {
            Ok((target, view)) => {
                self.root = Some(Shown::new(target, self.spaced(Level::Root, view)));
                self.leave_history();
                let closed = self.detail.take().is_some() | self.facts.take().is_some();
                if closed {
                    self.out.push(ToUi::Close);
                }
                self.send_open(Level::Root, None);
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(app = self.app, %error, "the app did not open");
                self.open_failed = Some(error.to_string());
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Show `target` in the detail view: what `open(r)` asked for, or a row. With `front`, a
    /// narrow window brings it to the front.
    /// Open what the reference `reference` (a `mention` mark's value) in rich text refers to:
    /// a resource of a package here that the viewer holds, in the detail level.
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
        // A place of the window's own view is no view of its own: the window goes there.
        if let Some(place) = with
            && self.show_place(target, place, front)
        {
            return;
        }
        let Some(app) = self.host() else {
            return;
        };
        // A place of what the window opens (a comparison's merge, a session's tab): the detail
        // opens on its own view, then goes there.
        if let Some(place) = with
            && app.runtime().has_place(&target.ty, place)
        {
            self.open_detail(target, None, front);
            self.show_place(target, place, None);
            return;
        }
        // Opening something else closes the window's history: the level shows what it opens.
        if self.leave_history().is_some() {
            self.detail = None;
            self.out.push(ToUi::Close);
        }
        if self
            .detail
            .as_ref()
            .is_some_and(|detail| detail.target == *target && with.is_none())
            || (with.is_none()
                && self
                    .facts
                    .as_ref()
                    .is_some_and(|facts| facts.target == *target))
        {
            if let Some(front) = front {
                self.out.push(ToUi::Front { front, shows: None });
            }
            return;
        }
        let view = match with {
            Some(view) => Some(view.to_owned()),
            None => app.runtime().default_view(&target.ty),
        };
        let Some(view) = view else {
            // The type declares no view: the platform's facts page says what the resource is
            // (a tag, a table's row, a card's item), rather than a press that shows nothing.
            let details = self.platform.phrase("tilolang-detail", &[]);
            let Some(tree) = facts::page(app.runtime(), target, &details) else {
                tracing::warn!(ty = %target.ty, "a resource did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
                return;
            };
            self.open_facts(target, tree, front);
            self.run_opened(target);
            self.commit_planned();
            return;
        };
        match app.open_resource(target, Some(&view)) {
            Ok(view) => {
                let mut view = Shown::new(target.clone(), self.spaced(Level::Detail, view));
                view.with = with.map(str::to_owned);
                if let Some(shown) = self.facts.take() {
                    self.seen_in_window(&shown.target);
                }
                if let Some(shown) = self.detail.replace(view) {
                    self.seen_in_window(&shown.target);
                }
                self.lenses_changed = true;
                self.send_open(Level::Detail, front);
                self.run_opened(target);
                self.commit_planned();
            }
            Err(error) => {
                tracing::warn!(%error, "a resource did not open");
                self.notice(Words::phrase("tilolang-open-failed"), ui::NoticeKind::Error);
            }
        }
    }

    /// Show `target` in the platform's facts page: its name, its type and its fields as facts
    /// ([`crate::instance::facts`]). The level's page, so Back leaves it as any page.
    fn open_facts(&mut self, target: &ResourceRef, tree: ui::Node, front: Option<ui::Front>) {
        if let Some(shown) = self.detail.take() {
            self.seen_in_window(&shown.target);
        }
        if let Some(shown) = self.facts.replace(facts::Facts {
            target: target.clone(),
            sent: tree.clone(),
        }) {
            self.seen_in_window(&shown.target);
        }
        self.lenses_changed = true;
        self.out.push(ToUi::Open {
            level: Level::Detail,
            target: wire::resource(target),
            tree,
            front,
            // The facts page shows the system's words for a resource, never one of the app's
            // views.
            capture_protected: false,
        });
    }

    /// Show `resource` in the detail level with the view `view` (an id) rather than its type's
    /// window: what `open(r, with: v)` asks for, for a view the system shows on a surface of
    /// its own (a row view as a search result), which
    /// `tilo-layout-check` lays out as it lays out a window.
    pub fn open_with(&mut self, resource: &ui::ResourceRef, view: &str) {
        if let Some(target) = wire::runtime_resource(resource) {
            // A page comes to the front of a narrow window as `open(r, view: v)` does, without
            // the keyboard a person's press takes.
            self.open_detail(&target, Some(view), Some(ui::Front::Action));
        }
    }

    /// Show `target` where the window's list shows it: what the root view answers for an item of
    /// its type (`on show`) in its own state, so a list opens the item's row where it stands and
    /// the window follows the item to the place it belongs to. What the view does not open in
    /// place — an item of a type it does not list — opens in the detail level, as it always has.
    pub(super) fn show_in_window(
        &mut self,
        target: &ResourceRef,
        front: Option<ui::Front>,
        press: u64,
    ) {
        if self.show_in_place(target) {
            self.close_history();
            if let Some(front) = front {
                self.out.push(ToUi::Front {
                    front,
                    shows: Some((press, wire::resource(target))),
                });
            }
            self.run_opened(target);
            return;
        }
        self.open_detail(target, None, front);
    }

    /// Run what opening `target` does, its type's `opened action`, as the person who opened it:
    /// wherever it was opened from (a list, search, a link, an agent, another app), since every
    /// opening comes here. A person who may not change it opens it all the same, without the
    /// change. A past version or a scenario's capture is nobody opening anything, and changes
    /// nothing. The caller commits what it planned.
    fn run_opened(&self, target: &ResourceRef) {
        if self.mode != Mode::Live {
            return;
        }
        let Some(host) = self.host() else {
            return;
        };
        let Some(action) = host.runtime().opened_action(&target.ty) else {
            return;
        };
        let opened = host.runtime().invoke_action(
            &self.me_actor(),
            (
                action.as_str(),
                vec![tilolang_runtime::Value::resource(target)],
            ),
        );
        if let Err(error) = opened {
            tracing::debug!(%error, ty = %target.ty, "opening a resource did not change it");
        }
    }

    /// Show `target` where the window's list shows it: what the root view answers for an item of
    /// its type (`on show`) in its own state, so a list opens the item's row where it stands and
    /// the window follows the item to the place it belongs to. Whether the view opened it there;
    /// an item of a kind it does not list opens nowhere.
    pub(super) fn show_in_place(&mut self, target: &ResourceRef) -> bool {
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
        if shown && let Some(root) = self.root.as_mut() {
            root.in_place = Some(target.clone());
        }
        shown
    }

    /// Take the window to the place `place` (a name or an id) its own view declares (`on show
    /// week: ...`), when `target` is what that view shows: the view applies what the place makes
    /// in this person's state of this window, so the workspace and everyone else's windows stay
    /// as they were. A page over the window closes, since the person asked for the window's own
    /// place. Whether the view declares that place.
    pub(super) fn show_place(
        &mut self,
        target: &ResourceRef,
        place: &str,
        front: Option<ui::Front>,
    ) -> bool {
        let goes = |shown: &Shown| {
            shown.target.doc == target.doc
                && shown.target.item == target.item
                && match shown.view.borrow().show_place(place) {
                    Ok(shown) => shown,
                    Err(error) => {
                        tracing::warn!(%error, place, "a view did not go to its place");
                        false
                    }
                }
        };
        // The detail level shows it: that page goes to its place and stays.
        if self.detail.as_ref().is_some_and(goes) {
            self.leave_history();
            if let Some(front) = front {
                self.out.push(ToUi::Front { front, shows: None });
            }
            return true;
        }
        if !self.root.as_ref().is_some_and(goes) {
            return false;
        }
        self.leave_history();
        let detail = self.detail.take();
        let facts = self.facts.take();
        if detail.is_some() || facts.is_some() {
            self.lenses_changed = true;
            self.out.push(ToUi::Close);
        }
        if let Some(front) = front {
            self.out.push(ToUi::Front { front, shows: None });
        }
        true
    }

    /// Close the detail level: Back.
    pub fn close_detail(&mut self) {
        self.leave_history();
        if let Some(shown) = self.detail.take() {
            self.seen_in_window(&shown.target);
            self.lenses_changed = true;
            self.out.push(ToUi::Close);
        }
        if let Some(shown) = self.facts.take() {
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
        let person = host.viewer().clone();
        if let Err(error) = host.runtime().mark_seen(&person, target) {
            tracing::warn!(%error, "what a window showed could not be marked seen");
            self.notice(Words::phrase("tilolang-seen-failed"), ui::NoticeKind::Error);
        }
    }

    /// Send the window the view of `level` as it is now: its tree, and the first rows of its
    /// virtual lists.
    pub(super) fn send_open(&mut self, level: Level, front: Option<ui::Front>) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.level_mut(level) else {
            return;
        };
        view.lists.clear();
        view.bound.clear();
        view.spaces = None;
        let tree = window_tree(&host, &*names, &view.view, view.with.is_some());
        view.sent = Some(tree.clone());
        // The item the window draws its page for, when its view lists it there.
        view.in_place = in_place_of(view.view.borrow().tree());
        let target = wire::resource(&view.target);
        let capture_protected = view.view.borrow().capture_protected();
        self.out.push(ToUi::Open {
            level,
            target,
            tree,
            front,
            capture_protected,
        });
        self.track_lists(level);
        self.tell_spaces(level);
        self.send_bound(level);
    }

    /// This instance's views as they stand, as a window opened now would be sent them: each
    /// level's tree, the rows its lists show and the values its bound controls hold, what the
    /// person typed included. Nothing the instance keeps of its own window moves: this is how
    /// its window is read as the lines an agent reads.
    #[must_use]
    pub fn shown_now(&self) -> Vec<ToUi> {
        let Some(host) = self.host() else {
            return Vec::new();
        };
        let names = &*self.names;
        let mut out = Vec::new();
        for level in [Level::Root, Level::Detail] {
            let Some(view) = self.level(level) else {
                continue;
            };
            out.push(ToUi::Open {
                level,
                target: wire::resource(&view.target),
                tree: window_tree(&host, names, &view.view, view.with.is_some()),
                front: None,
                capture_protected: view.view.borrow().capture_protected(),
            });
            for (list, rows) in &view.lists {
                out.push(rows_message(
                    &host,
                    names,
                    &view.view,
                    level,
                    list,
                    rows.as_shown(),
                ));
            }
            let mut bound: Vec<(String, ui::Value)> = Vec::new();
            convert_with(&host, names, &view.view, |convert, instance| {
                bound_values(instance.tree(), instance, convert, &mut bound);
                for rows in view.lists.values() {
                    for key in rows.shown() {
                        if let Some(row) = instance.find(key) {
                            bound_values(row, instance, convert, &mut bound);
                        }
                    }
                }
            });
            out.extend(bound.into_iter().map(|(key, value)| ToUi::Bound {
                level,
                acked: view.acked.get(&key).copied().unwrap_or(0),
                key,
                value,
            }));
        }
        // A resource whose type declares no view shows its facts page in the detail level.
        if self.detail.is_none()
            && let Some(facts) = &self.facts
        {
            out.push(ToUi::Open {
                level: Level::Detail,
                target: wire::resource(&facts.target),
                tree: facts.sent.clone(),
                front: None,
                capture_protected: false,
            });
        }
        out
    }

    /// Keep the rows of every virtual list of the view of `level`, sending the window the first
    /// rows of each new one, and drop those of lists gone.
    pub(super) fn track_lists(&mut self, level: Level) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.level_mut(level) else {
            return;
        };
        let lists = virtual_list_paths(view.view.borrow().tree());
        let handle = Rc::clone(&view.view);
        view.lists.retain(|key, rows| {
            lists.iter().any(|(list, _)| list == key) && rows.reads(&handle, key)
        });
        let mut sent = Vec::new();
        for (list, _) in lists {
            if view.lists.contains_key(&list) {
                continue;
            }
            let mut rows = ListRows::of(&handle, &list);
            let first = 0..FIRST_ROWS.min(rows.len());
            // Which row a list shows open is its list's to say: the host asks for it with the
            // rows it shows, and the open a view kept arrives with that ask.
            let requested = rows.request(first, (None, None));
            sent.push(rows_message(
                &host, &*names, &handle, level, &list, requested,
            ));
            view.lists.insert(list, rows);
        }
        self.out.extend(sent);
    }

    /// Tell the window which view calls of `level` read the space layout gives them, when that
    /// set changed: it measures those and tells each the size it was given (`ToApp::Space`), so
    /// a view that adapts to its own slot follows a resize and no view that reads no space costs
    /// a measure.
    pub(super) fn tell_spaces(&mut self, level: Level) {
        let Some(now) = self
            .level(level)
            .map(|shown| shown.view.borrow().space_readers())
        else {
            return;
        };
        let Some(shown) = self.level_mut(level) else {
            return;
        };
        if shown.spaces.as_ref() == Some(&now) {
            return;
        }
        shown.spaces = Some(now.clone());
        self.out.push(ToUi::Spaces { level, keys: now });
    }

    /// Whether the view of `level` has nothing to run over: a resource the document does not
    /// name — what was removed, or an item a change has not brought here yet — has no view, and
    /// the window keeps the tree it has until the document holds it. A view of another app's
    /// resource runs over the reading the session publishes for it, which stands where the
    /// document would: it has something to run over from the first reading, and runs again when
    /// that reading moves.
    fn unheld(&self, host: &Host, level: Level) -> bool {
        self.level(level).is_some_and(|shown| {
            matches!(
                host.store().type_of(&shown.target),
                Err(StoreError::NotFound(_))
            ) && host.runtime().reading(&shown.target).is_none()
        })
    }

    /// Whether the resource `target` was removed from the workspace, rather than never having
    /// reached this device: the store's last version of it is its removal. A view of another
    /// app's resource runs over the reading its app published, which stands where the document
    /// would, so it is there while the reading is.
    fn removed(host: &Host, target: &ResourceRef) -> bool {
        if !matches!(host.store().type_of(target), Err(StoreError::NotFound(_)))
            || host.runtime().reading(target).is_some()
        {
            return false;
        }
        host.store().versions(target).is_ok_and(|versions| {
            versions
                .last()
                .is_some_and(|last| last.kind == VersionKind::Removed)
        })
    }

    /// Close the page of a level whose resource the workspace no longer holds, whoever removed
    /// it: an action at this window or another surface, an agent, or a change another device
    /// made. A page shown beside the window's own closes, and the window goes back where it came
    /// from; a level with nowhere to go back to — the window's own root — says in the platform's
    /// words that what it showed is gone. Whether a page closed.
    pub(super) fn poll_gone(&mut self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let mut changed = false;
        // The history the window shows is the system's page about a resource, its removal
        // included: it says what became of what it shows, so it stays.
        let detail_gone = self.window_history.is_none()
            && self
                .detail_target()
                .is_some_and(|target| Self::removed(&host, target));
        if detail_gone {
            if self.root.is_some() {
                self.close_detail();
            } else {
                // Nothing stands behind the page: the level itself says it is gone.
                self.show_gone(Level::Detail);
                self.detail = None;
                self.facts = None;
            }
            changed = true;
        }
        if self
            .root
            .as_ref()
            .is_some_and(|root| Self::removed(&host, &root.target))
        {
            self.show_gone(Level::Root);
            self.root = None;
            changed = true;
        }
        changed
    }

    /// Show the platform's words for what the window showed and the workspace no longer holds,
    /// where the level's page stood: what it is gone in the system's words, never the app's.
    fn show_gone(&mut self, level: Level) {
        let Some(target) = self.level(level).map(|shown| wire::resource(&shown.target)) else {
            return;
        };
        let words = self.platform.phrase("tilolang-gone", &[]);
        self.out.push(ToUi::Open {
            level,
            target,
            tree: gone_page(&words),
            front: None,
            capture_protected: false,
        });
    }

    /// Close the pages whose resource the workspace removed, re-run the views whose reads
    /// changed, and send the window what changed: their trees' patches, and the changes of their
    /// virtual lists' rows. Whether anything did.
    pub(super) fn poll_views(&mut self) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let names = Rc::clone(&self.names);
        let mut changed = self.poll_gone();
        for level in [Level::Root, Level::Detail] {
            changed |= self.poll_level(&host, &names, level);
        }
        if changed {
            let _phase = tilo_resource::profiling::Phase::start("instance.views.bound");
            self.send_bound(Level::Root);
            self.send_bound(Level::Detail);
        }
        let selected = self.select_revealed();
        self.tell_reveal() || selected || changed
    }

    /// Re-run the view of `level` when what it reads changed, and send the window what it now
    /// shows: whether anything did.
    fn poll_level(&mut self, host: &Host, names: &Rc<dyn NameHost>, level: Level) -> bool {
        if !self
            .level(level)
            .is_some_and(|shown| shown.view.borrow().woken())
            || self.unheld(host, level)
        {
            return false;
        }
        let Some(shown) = self.level_mut(level) else {
            return false;
        };
        // What the view reads changed: the tree may stand as it was (a row's control reads the
        // item's field it is bound to), and the window still needs the values it shows.
        let stale = shown.view.borrow_mut().is_stale();
        let polled = {
            let _phase = tilo_resource::profiling::Phase::start("instance.views.evaluate");
            shown.view.borrow_mut().poll_patches()
        };
        let patches = match polled {
            Ok(patches) => patches,
            Err(error) => {
                view_failed(&error);
                return false;
            }
        };
        if patches.is_empty() {
            self.tell_spaces(level);
            return stale;
        }
        // The item its page draws, when it lists the items it shows: a press that selects a row
        // moves the page, and what is typed into it is that item's. A tree no patch changed
        // draws the page it drew.
        shown.in_place = in_place_of(shown.view.borrow().tree());
        let lists = virtual_list_paths(shown.view.borrow().tree());
        let sorted = sort(patches, &lists);
        let messages = changed_rows(host, names, shown, level, &sorted);
        if sorted.content {
            self.send_content(host, names, level, &sorted);
        }
        self.out.extend(messages);
        // A row that changed can hold a list it did not (the document of a row that opened),
        // and the content one.
        self.track_lists(level);
        self.tell_spaces(level);
        true
    }

    /// Send the window the content of `level`'s view as `sorted` changed it: only what changed,
    /// where it can be converted alone, the whole tree where not.
    fn send_content(
        &mut self,
        host: &Host,
        names: &Rc<dyn NameHost>,
        level: Level,
        sorted: &Sorted,
    ) {
        let Some(shown) = self.level_mut(level) else {
            return;
        };
        let _phase = tilo_resource::profiling::Phase::start("instance.views.convert");
        // A list inside the content may have rows the view has not rendered yet.
        show_content_rows(&mut shown.view.borrow_mut());
        // Only what changed is converted, where it can be: a clock's minute converts the clock's
        // text, not the window. A page's tree is converted whole, since the mark the window reads
        // to give it Back (`mark_page`) belongs to no view's own props: a patch that replaced its
        // root would take the mark away.
        let in_place = if shown.with.is_some() {
            None
        } else {
            shown
                .sent
                .as_mut()
                .zip(sorted.changed.as_ref())
                .and_then(|(sent, changed)| {
                    convert_with(host, &**names, &shown.view, |convert, instance| {
                        patch_in_place(sent, instance.tree(), changed, convert)
                    })
                })
        };
        let patches = in_place.unwrap_or_else(|| {
            let tree = window_tree(host, &**names, &shown.view, shown.with.is_some());
            let mut patches = Vec::new();
            match &shown.sent {
                Some(sent) => wire::diff(sent, &tree, &mut patches),
                None => patches.push(ui::Patch::SetRoot(tree.clone())),
            }
            shown.sent = Some(tree);
            patches
        });
        if !patches.is_empty() {
            self.out.push(ToUi::Patch { level, patches });
        }
    }

    /// Select the row of the item a search hit named inside what the window shows
    /// ([`super::Instance::selecting`]), as a person's press of that row does: the list that
    /// lists it (a document's pages) is told its `select`, so the view turns to it as it does for
    /// the person. An item no list lists (a mark) selects the row of an item it refers to (the
    /// page it is on). The ask waits while the window draws no view, and goes once one is drawn
    /// that lists neither. Whether a row was selected.
    fn select_revealed(&mut self) -> bool {
        let Some(item) = self.selecting.take() else {
            return false;
        };
        let Some(host) = self.host() else {
            return false;
        };
        let mut wanted = vec![item.clone()];
        wanted.extend(referred_items(&host, &item));
        for want in &wanted {
            let key = want.id_string();
            for level in [Level::Root, Level::Detail] {
                let list = self.level_mut(level).and_then(|view| {
                    view.lists
                        .iter()
                        .find(|(_, rows)| rows.holds(&key))
                        .map(|(list, _)| list.clone())
                });
                let Some(list) = list else {
                    continue;
                };
                let value = ui::Value::Ref(wire::resource(want));
                let mut ran = None;
                if let Err(why) = self.fire(level, &list, "select", Some(&value), None, &mut ran) {
                    tracing::debug!(?why, %key, "a search hit's row was not selected");
                }
                return true;
            }
        }
        let drawn = [Level::Root, Level::Detail].into_iter().any(|level| {
            self.level_mut(level)
                .is_some_and(|view| view.sent.is_some())
        });
        if !drawn {
            self.selecting = Some(item);
        }
        false
    }

    /// Tell the editors of the text that holds the place a search hit asked for to show it
    /// ([`super::Revealing`]): each editor bound to the field of that resource holding it, which
    /// the window draws now, asked for the place in that text (a paragraph at its block place
    /// `¶<place>:<count>`, or a line at its number `line:1:<count>`). The ask waits while the
    /// window draws none, as it does before the view it opened runs. What the view says of the
    /// editor's `reveal` afterwards is its own ask again, which the editor tells apart from a
    /// block's. Whether an editor was told.
    fn tell_reveal(&mut self) -> bool {
        let Some(revealing) = self.revealing.take() else {
            return false;
        };
        let count = self.reveals.wrapping_add(1);
        let block_ask = format!("{BLOCK_PLACE}{}:{count}", revealing.block.place);
        // A code editor shows a line counted from 1: its reveal takes a place, a column and the
        // count that tells a repeat from the same place.
        let line_ask = format!("{}:1:{count}", revealing.block.place.saturating_add(1));
        let mut told = Vec::new();
        for level in [Level::Root, Level::Detail] {
            let Some(sent) = self.level_mut(level).and_then(|view| view.sent.as_mut()) else {
                continue;
            };
            let mut editors = Vec::new();
            editors_of(sent, &revealing.resource, &mut Vec::new(), &mut editors);
            let patches: Vec<ui::Patch> = editors
                .into_iter()
                .filter(|(_, field, _)| *field == revealing.block.field)
                .map(|(path, _, blocks)| ui::Patch::SetProp {
                    path,
                    name: REVEAL.to_owned(),
                    value: Some(ui::Value::Opt(Some(Box::new(ui::Value::Text(if blocks {
                        block_ask.clone()
                    } else {
                        line_ask.clone()
                    }))))),
                })
                .collect();
            if patches.is_empty() {
                continue;
            }
            for patch in &patches {
                // Kept as the window holds it: later patches are against it.
                if patch.clone().apply(sent).is_err() {
                    tracing::debug!("an editor's reveal did not apply to the tree sent");
                }
            }
            told.push(ToUi::Patch { level, patches });
        }
        if told.is_empty() {
            self.revealing = Some(revealing);
            return false;
        }
        self.reveals = count;
        self.out.extend(told);
        true
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
                    // A shell's places show resources on the canvas, never as a detail level of
                    // the shell's own window: what a view opened is a place the session arranges
                    // (one-app-model.md §2, shell.md §8). Anywhere else, it opens the window's
                    // detail level.
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
                // An undo a view applied is the document's, carried out as the window's undo is.
                ViewEvent::Undo(tilolang_runtime::UndoAsk::Step { change, .. }) => {
                    self.history.push(HistoryAsk::UndoStep(change));
                }
                ViewEvent::Undo(tilolang_runtime::UndoAsk::Run { run }) => {
                    self.history.push(HistoryAsk::UndoRun(run));
                }
                // One move marked a catch-up seen: the window says how much, and offers to take
                // it back while it says it.
                // A step in a call is the system's call service's to carry out: the host takes it.
                ViewEvent::Call { target, step } => self.call_steps.push((target, step)),
                // A line the person ran is the session's program host's to run: the host takes it.
                ViewEvent::RunLine { target, line } => self.line_runs.push((target, line)),
                // Copy link is the system's: the link is minted here and copied to the person.
                ViewEvent::CopyLink { target, app } => self.copy_link(&target, &app),
                // Export is the system's too: the resource is written out here as a file.
                ViewEvent::Export { target, .. } => self.export(&target),
                // A view ended the keyboard's place in an editor: the window lets the field go,
                // as closing the editor does.
                ViewEvent::EndEdit => self.out.push(ToUi::Focus { key: None }),
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

    /// Put the link of `target`, a resource of `app`, on the person's clipboard, minted as other
    /// devices and people open it ([`Self::share_link`]), and say that it is there, or why not.
    fn copy_link(&mut self, target: &ResourceRef, app: &str) {
        let resource = crate::publish::resource_id(app, &target.id_string());
        let url = self.share_link(&resource).and_then(|output| {
            output
                .value
                .as_ref()
                .and_then(|value| value.get("url"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "the host answered no link".to_owned())
        });
        match url {
            Ok(url) if self.platform.copy_clipboard("text/plain", url.as_bytes()) => {
                self.notice(Words::phrase("tilolang-link-copied"), ui::NoticeKind::Info);
            }
            Ok(_) => self.notice(
                Words::phrase("tilolang-link-not-copied"),
                ui::NoticeKind::Error,
            ),
            Err(why) => self.notice(Words::Plain(why), ui::NoticeKind::Error),
        }
    }

    /// Save `target` as a file where the person says: the Markdown its app says it exports as,
    /// or its document body ([`tilolang_runtime::Runtime::export_markdown`]), else everything it
    /// holds that the person may read, as JSON ([`Host::export_reading`]). Named by its title.
    fn export(&mut self, target: &ResourceRef) {
        let Some(host) = self.host() else {
            return;
        };
        let runtime = host.runtime();
        let made = runtime
            .export_markdown(target)
            .map_err(|error| error.to_string())
            .and_then(|markdown| {
                if let Some(markdown) = markdown {
                    return Ok(("md", "text/markdown", markdown));
                }
                let me = self.me_actor();
                let reading = host.export_reading(target, &host.reads_for(&me))?;
                let json =
                    serde_json::to_string_pretty(&reading).map_err(|error| error.to_string())?;
                Ok(("json", "application/json", json))
            });
        let title = runtime
            .title(target)
            .unwrap_or_default()
            .replace(['/', '\\'], "-");
        let saved = made.and_then(|(extension, media, content)| {
            let request = tilolang_runtime::blob::SaveRequest {
                name: format!("{}.{extension}", title.trim()),
                media: media.to_owned(),
            };
            self.platform.save_file(&request, content.as_bytes())
        });
        if let Err(why) = saved {
            self.notice(Words::Plain(why), ui::NoticeKind::Error);
        }
    }

    /// The view of a shell places what it opens on the canvas, rather than opening a detail level
    /// of the shell's own window: what a `show`'s view opens is a place the session arranges.
    pub fn place_opens(&mut self, place: bool) {
        self.place_opens = place;
    }

    /// What a view of a shell opened since the last call, each a resource and the view it named:
    /// the places the session arranges on the canvas.
    pub fn take_opens_to_place(&mut self) -> Vec<(ResourceRef, Option<String>, bool)> {
        std::mem::take(&mut self.opens_to_place)
    }

    /// Send the window what each bound control of the view of `level` (and of the rows it
    /// shows) holds now, where that changed.
    pub(super) fn send_bound(&mut self, level: Level) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(view) = self.level_mut(level) else {
            return;
        };
        let handle = Rc::clone(&view.view);
        let mut now: Vec<(String, ui::Value)> = Vec::new();
        convert_with(&host, &*names, &handle, |convert, instance| {
            bound_values(instance.tree(), instance, convert, &mut now);
            for rows in view.lists.values() {
                for key in rows.shown() {
                    if let Some(row) = instance.find(key) {
                        bound_values(row, instance, convert, &mut now);
                    }
                }
            }
        });
        let mut sent = Vec::new();
        for (key, value) in now {
            if view.bound.get(&key) == Some(&value) {
                continue;
            }
            view.bound.insert(key.clone(), value.clone());
            let acked = view.acked.get(&key).copied().unwrap_or(0);
            sent.push(ToUi::Bound {
                level,
                key,
                value,
                acked,
            });
        }
        self.out.extend(sent);
    }

    /// Whether the detail view's bound controls hold anything, and where: a draft being typed
    /// in a conversation, with a number that changes as the person types it. Only whether,
    /// never what it says: presence carries "composing", never the words.
    #[must_use]
    pub fn composing_in(&self) -> Option<(&ResourceRef, u64)> {
        use std::hash::{Hash as _, Hasher as _};
        let detail = self.detail.as_ref()?;
        let shown = self.level(Level::Detail)?;
        // The view's own controls, as the bound values it holds: any text or rich text with
        // something in it is a draft under way. The revision sums each control's, so the order
        // the controls are held in does not change it.
        let mut revision = 0_u64;
        let mut holds = false;
        for key in shown.bound.keys() {
            let text = match shown.view.borrow().bound_value(key) {
                Some(crate::runtime::Value::Text(text)) => text.clone(),
                Some(crate::runtime::Value::Rich(rich)) => rich.written_text(),
                _ => continue,
            };
            holds |= !text.trim().is_empty();
            let mut hasher = std::hash::DefaultHasher::new();
            (key, &text).hash(&mut hasher);
            revision = revision.wrapping_add(hasher.finish());
        }
        holds.then_some((&detail.target, revision))
    }

    /// Tell the window what the bound control `key` of `level` holds now that the edit it sent
    /// is applied: the value the app holds is what its next frame draws, and the control shows
    /// it until then only as the person's own touch, which a frame from the app replaces.
    pub(super) fn shown_by_window(&mut self, level: Level, key: &str) {
        let Some(host) = self.host() else {
            return;
        };
        let names = Rc::clone(&self.names);
        let Some(handle) = self.level(level).map(|view| Rc::clone(&view.view)) else {
            return;
        };
        let Some(value) = handle.borrow().bound_value(key) else {
            return;
        };
        let mut converted = None;
        convert_with(&host, &*names, &handle, |convert, _| {
            converted = Some(convert.value(&value));
        });
        let Some(shown) = converted else {
            return;
        };
        let Some(view) = self.level_mut(level) else {
            return;
        };
        if view.bound.get(key) == Some(&shown) {
            return;
        }
        view.bound.insert(key.to_owned(), shown.clone());
        let acked = view.acked.get(key).copied().unwrap_or(0);
        self.out.push(ToUi::Bound {
            level,
            key: key.to_owned(),
            value: shown,
            acked,
        });
    }
}

/// The key prefix of the page a level shows when what it showed is gone.
const GONE: &str = "gone~";

/// The window's messages for the rows `sorted` changed of `shown`'s virtual lists.
fn changed_rows(
    host: &Host,
    names: &Rc<dyn NameHost>,
    shown: &mut Shown,
    level: Level,
    sorted: &Sorted,
) -> Vec<ToUi> {
    let mut messages = Vec::new();
    let handle = Rc::clone(&shown.view);
    for (list, changes) in &sorted.lists {
        let Some(rows) = shown.lists.get_mut(list) else {
            continue;
        };
        if changes.reread {
            // A list read anew is a new list to the window: its first rows come with it, as they
            // do with a list the view shows for the first time ([`Instance::track_lists`]), so
            // showing it takes no round of the window asking for them.
            let reset = rows.reread();
            let len = u32::try_from(rows.len()).unwrap_or(u32::MAX);
            messages.push(ToUi::RowsChanged {
                level,
                list: list.clone(),
                len,
                changes: reset,
            });
            let first = rows.request(0..FIRST_ROWS.min(rows.len()), (None, None));
            messages.push(rows_message(host, &**names, &handle, level, list, first));
            continue;
        }
        let row_changes = {
            let mut structural = rows.apply(&changes.structure);
            let changed = rows.changed(&changes.rows);
            convert_with(host, &**names, &handle, |convert, _| {
                structural.extend(changed.iter().map(|row| ui::RowChange::Changed {
                    row: Box::new(convert.node(row)),
                }));
            });
            structural
        };
        if row_changes.is_empty() {
            continue;
        }
        let len = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        messages.push(ToUi::RowsChanged {
            level,
            list: list.clone(),
            len,
            changes: row_changes,
        });
    }
    messages
}

/// The page a level shows when the workspace no longer holds what it showed: the platform's
/// words, in the page's own column where the view stood.
fn gone_page(words: &str) -> ui::Node {
    let mut state = ui::Node::empty("ui.EmptyState", &format!("{GONE}state"));
    state
        .props
        .insert("title".to_owned(), ui::Value::text(words));
    let mut content = ui::Node::empty("part", &format!("{GONE}content"));
    content
        .props
        .insert("name".to_owned(), ui::Value::text("content"));
    content.children.push(state);
    let mut shell = ui::Node::empty("ui.NavShell", &format!("{GONE}shell"));
    shell.props.insert(
        "content_kind".to_owned(),
        ui::Value::Enum("page".to_owned()),
    );
    shell.children.push(content);
    shell
}

/// The item a tree of `node` says it selected, when a window draws its page for it beside the
/// list: the resource of its selected row.
fn in_place_of(node: &crate::runtime::UiNode) -> Option<ResourceRef> {
    use crate::runtime::Value;
    fn walk(node: &crate::runtime::UiNode, out: &mut Option<crate::runtime::Value>) -> bool {
        if let Some(Value::List(items)) = node.props.get("selection")
            && let Some(Value::Ref(chosen)) = items.first()
        {
            *out = Some(Value::Ref(chosen.clone()));
            return true;
        }
        node.children.iter().any(|child| walk(child, out))
    }
    let mut chosen = None;
    walk(node, &mut chosen);
    let Some(Value::Ref(chosen)) = chosen else {
        return None;
    };
    let resource = ui::ResourceRef {
        id: chosen.target.id_string(),
        ty: chosen.target.ty.clone(),
    };
    wire::runtime_resource(&resource)
}

/// The items `item`'s own fields refer to, as its package declares them: the page a mark is on.
fn referred_items(host: &Host, item: &ResourceRef) -> Vec<ResourceRef> {
    use crate::runtime::store::{FieldSpec, FieldValue, Scalar};
    let Some(meta) = host.find_in_packages(|package| {
        package
            .meta
            .types
            .iter()
            .any(|ty| ty.id == item.ty.as_str())
            .then(|| package.meta.clone())
    }) else {
        return Vec::new();
    };
    let Some(declared) = meta.types.iter().find(|ty| ty.id == item.ty.as_str()) else {
        return Vec::new();
    };
    let specs = host.runtime().field_specs(&declared.id).unwrap_or_default();
    declared
        .fields
        .iter()
        .filter_map(|field| {
            let referred = meta.type_named(&field.ty)?;
            let spec: &FieldSpec = specs.iter().find(|spec| spec.id == field.id)?;
            match host.store().read(item, spec).ok()? {
                FieldValue::Scalar(Scalar::Text(id)) => {
                    ResourceRef::parse_id(&id, referred.id.clone())
                }
                _ => None,
            }
        })
        .collect()
}

/// The state or prop of an editor that names the place it shows.
const REVEAL: &str = "reveal";

/// What an editor's `reveal` that names a block by its place in the text starts with; a
/// heading's place among the headings is the bare number, and a code editor's is `line:column`.
const BLOCK_PLACE: char = '¶';

/// The editors under `node` (at `path` from the root) bound to a field of the resource published
/// as `resource` that show a place they are asked for (`reveal`): each one's path, the field, and
/// whether it takes a place in blocks (`true`, a text editor's state) or a line (`false`, a code
/// editor's prop). A place is a paragraph of a rich text, or a line of plain text.
fn editors_of(
    node: &ui::Node,
    resource: &str,
    path: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, String, bool)>,
) {
    if let Some((target, field)) = node.binding.as_ref().and_then(ui::Binding::field)
        && target == resource
        && let Some(described) = ui::description(&node.kind)
        && (described.state(REVEAL).is_some() || described.prop(REVEAL).is_some())
    {
        out.push((
            path.clone(),
            field.to_owned(),
            described.state(REVEAL).is_some(),
        ));
    }
    for child in &node.children {
        path.push(child.key.clone());
        editors_of(child, resource, path, out);
        path.pop();
    }
}

/// A view's call failed: it draws a failed state where it stands (`shell.md` §6, "Nothing can
/// lock a person out"), and the window keeps the rest of the frame it has and runs again on what
/// comes next. A test sees the error instead, so none passes over one.
fn view_failed(error: &tilolang_runtime::RuntimeError) {
    tracing::warn!(%error, "a view failed to run");
    debug_assert!(false, "a view failed to run: {error}");
}
