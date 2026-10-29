//! Search, as the launcher app shows it: the shell mounts the launcher
//! (`apps/launcher`) in its own process, lends it a source over the
//! desktop's search, and shows it where the palette stands.
//!
//! The launcher draws the card and takes the person's input; what search
//! finds and what choosing a row does stay here, over the search registry
//! whose rows act on the shell's own state. Each ask the launcher puts on
//! its link is answered on the next settle, where the host holds the
//! widget document running a row may change: a search runs the registry,
//! a choice runs the row as the palette's Enter always has. What the
//! answer shows is read from the search state that dispatch keeps.
//!
//! Every way the shell opens search (Super+Space, an app's own search, the
//! launchpad, an app chooser) asks the launcher over the link to open, which
//! it does before it handles the next key; agents and scripts open it
//! through its `open` action, which reaches the same search here. When
//! search closes here (a row ran), the launcher is told.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use launcher_app::source::{Answer, Ask, LauncherService, LauncherSource, Link};

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// The record the launcher is mounted on: one per session, never a tab's.
const LAUNCHER_INSTANCE: u64 = u64::MAX - 1;

/// The launcher the shell shows, and its side of the link.
pub(crate) struct LauncherHost {
    link: Link,
    /// The launcher's app, drawn in the card's place.
    pub(super) app: Box<dyn ui::widget::AppWidget>,
    /// The launcher asked for a frame.
    woken: Arc<AtomicBool>,
    /// What the launcher was last shown, as a digest.
    shown: Option<u64>,
    /// The query the launcher last searched for: the shell puts its own in
    /// the field only when it differs.
    asked_query: String,
    /// The launcher was asked to open and has not said it did.
    opening: bool,
}

impl LauncherHost {
    /// Mount the launcher through the session's in-process apps, in `room`,
    /// reaching the session's broker so its actions are published.
    fn mount(apps: &crate::shell::in_process::SharedInProcessApps, room: &str) -> Option<Self> {
        let link = Link::new();
        let lent = LauncherService { link: link.clone() };
        let mut apps = apps.borrow_mut();
        let component = apps
            .mount(
                launcher_app::definition(),
                room,
                LAUNCHER_INSTANCE,
                ui::record::RecordId::room_default(),
                |context| context.lend(lent),
            )
            .inspect_err(|error| tracing::warn!(%error, "the launcher could not be mounted"))
            .ok()?;
        let collaboration = apps.collaboration(&component, room);
        drop(apps);
        let woken = Arc::new(AtomicBool::new(false));
        let wake = {
            let woken = Arc::clone(&woken);
            move |_: ui::WakeEvent| {
                woken.store(true, Ordering::Release);
                crate::shell::wake_compositor();
            }
        };
        let app = tilo_desktop::resolver::app_tile(component, collaboration, wake)
            .inspect_err(|error| tracing::warn!(%error, "the launcher could not be shown"))
            .ok()?;
        Some(Self {
            link,
            app,
            woken,
            shown: None,
            asked_query: String::new(),
            opening: false,
        })
    }

    /// Whether the launcher asked for a frame since the last call.
    pub(super) fn take_woken(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }

    /// Ask the launcher to open in `scope`, as the person asked search to.
    fn ask_open(&mut self, scope: crate::shell::chrome_state::SearchScope) {
        self.link.request_open(scope);
        self.opening = true;
    }
}

/// The shell's answers to the launcher, for one settle.
struct Serving<'a, 'b> {
    host: &'a mut ShellOverlayHost,
    edits: &'a mut ui::widget::Edits<'b>,
    root: &'a ObjId,
}

impl LauncherSource for Serving<'_, '_> {
    fn answer(&mut self, ask: Ask, answer: &mut Answer) {
        self.host.answer_launcher(ask, self.edits, self.root);
        *answer = self.host.launcher_view();
    }
}

impl ShellOverlayHost {
    /// Whether search shows through the launcher app.
    pub(super) fn launcher_shown(&self) -> bool {
        self.launcher.is_some()
    }

    /// Mount the launcher once the session lends the apps it mounts in its
    /// own process. A host without them (a bare test host) shows the
    /// palette it draws itself.
    pub(super) fn mount_launcher(&mut self) {
        if self.launcher.is_some() || self.launcher_unavailable {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let (apps, room) = {
            let state = chrome.borrow();
            let Some(apps) = state.hosted_inputs.in_process().cloned() else {
                self.launcher_unavailable = true;
                return;
            };
            let room = state
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room().to_string());
            (apps, room)
        };
        let Some(room) = room else {
            return;
        };
        self.launcher = LauncherHost::mount(&apps, &room);
        self.launcher_unavailable = self.launcher.is_none();
        self.palette_layout_dirty = true;
    }

    /// Open the launcher when search opened here and it is not open yet.
    pub(super) fn open_launcher_if_asked(&mut self) {
        let scope = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow().search_scope)
            .unwrap_or_default();
        let visible = self.palette_visible;
        if let Some(launcher) = self.launcher.as_mut()
            && visible
            && !launcher.link.is_open()
            && !launcher.opening
        {
            launcher.ask_open(scope);
        }
    }

    /// Answer what the launcher asked so far.
    pub(super) fn serve_launcher(&mut self, edits: &mut ui::widget::Edits<'_>, root: &ObjId) {
        let Some(link) = self.launcher.as_ref().map(|launcher| launcher.link.clone()) else {
            return;
        };
        if link.has_asks() {
            link.serve(&mut Serving {
                host: self,
                edits,
                root,
            });
        }
    }

    /// The layout node the launcher is shown in, while it shows search:
    /// where keyboard focus goes.
    pub(crate) fn launcher_node(&self) -> Option<NodeId> {
        if self.launcher.is_some() && self.palette_visible {
            self.palette_node
        } else {
            None
        }
    }

    /// Answer what the launcher asked, open it when the shell opened
    /// search, tell it when search closed here, and show it what changed.
    pub(super) fn settle_launcher(&mut self, edits: &mut ui::widget::Edits<'_>, root: &ObjId) {
        self.mount_launcher();
        let Some(launcher) = self.launcher.as_ref() else {
            return;
        };
        let link = launcher.link.clone();
        self.serve_launcher(edits, root);
        let Some(launcher) = self.launcher.as_mut() else {
            return;
        };
        if link.is_open() {
            launcher.opening = false;
        }
        if self.palette_visible && !link.is_open() && !launcher.opening {
            let scope = self
                .chrome
                .as_ref()
                .map(|chrome| chrome.borrow().search_scope)
                .unwrap_or_default();
            launcher.ask_open(scope);
            return;
        }
        if !self.palette_visible {
            // Search closed here before the launcher took the shell's ask
            // to open: the ask is withdrawn, or it would open search again.
            link.take_open_request();
            launcher.opening = false;
            if link.is_open() {
                link.publish(Answer {
                    closed: true,
                    ..Answer::default()
                });
                launcher.shown = None;
                self.palette_layout_dirty = true;
            }
            return;
        }
        let answer = self.launcher_view();
        let digest = digest(&answer);
        if let Some(launcher) = self.launcher.as_mut()
            && launcher.shown != Some(digest)
        {
            launcher.shown = Some(digest);
            link.publish(answer);
        }
    }

    /// Carry out `ask` as the palette's own input always has.
    fn answer_launcher(&mut self, ask: Ask, edits: &mut ui::widget::Edits<'_>, root: &ObjId) {
        match ask {
            Ask::Open { scope, query } => {
                // The launcher opening as the shell asked, after search
                // closed here (Escape, the shortcut again): it is told it is
                // closed rather than opening search a second time.
                let withdrawn = self
                    .launcher
                    .as_mut()
                    .is_some_and(|launcher| std::mem::take(&mut launcher.opening));
                if withdrawn && !self.palette_visible {
                    self.palette_layout_dirty = true;
                    return;
                }
                if !self.palette_visible {
                    self.open_palette_with_scope(scope);
                }
                if let Some(query) = query {
                    self.palette.set_query(&query);
                }
                self.palette.set_focus(true);
                self.refresh_search_results(edits, root);
            }
            Ask::Search(search) => {
                if let Some(launcher) = self.launcher.as_mut() {
                    launcher.asked_query.clone_from(&search.query);
                }
                if search.kinds != self.active_kind_filter {
                    self.active_kind_filter = search.kinds;
                    self.last_search_query.clear();
                }
                self.palette.set_query(&search.query);
                self.refresh_search_results(edits, root);
            }
            Ask::Choose { row, modifier } => {
                let at = self
                    .palette
                    .visible_entries()
                    .iter()
                    .position(|entry| entry.id == row);
                if let Some(at) = at {
                    self.palette.set_cursor(at);
                    self.activate_selected_result_with(edits, root, modifier);
                }
            }
            Ask::Adjust { row, delta } => self.step_result(&row, delta, edits, root),
            Ask::Argument(text) => self.submit_param_field(text, edits, root),
            Ask::Back => self.palette_step_back(),
            Ask::Widen => self.widen_search_scope(),
            Ask::Dismissed => {
                if self.palette_visible {
                    self.palette_visible = false;
                    self.palette.close();
                }
            }
        }
        if self.palette_visible {
            self.refresh_open_palette(edits, root);
        }
        self.palette_layout_dirty = true;
    }

    /// What the launcher shows now, read from the search state dispatch
    /// keeps. The query is given only when it is not the one the launcher
    /// last searched for or was given: what the person types stays theirs.
    fn launcher_view(&mut self) -> Answer {
        let palette = &self.palette;
        let query = palette.query().to_owned();
        let asked = self
            .launcher
            .as_ref()
            .map(|launcher| launcher.asked_query.as_str());
        let query = (palette.argument_step().is_none()
            && palette.bypasses_fuzzy()
            && asked != Some(query.as_str()))
        .then_some(query);
        if let (Some(launcher), Some(query)) = (self.launcher.as_mut(), &query) {
            launcher.asked_query.clone_from(query);
        }
        let scope = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow().search_scope)
            .unwrap_or_default();
        let palette = &self.palette;
        Answer {
            rows: palette.entries().to_vec(),
            narrows: !palette.bypasses_fuzzy(),
            kinds: self.search_filter_inputs().kinds,
            band: palette.filter_band_shown(),
            scope,
            scope_token: palette.scope_token().map(str::to_owned),
            scope_token_action: palette.scope_token_action().map(str::to_owned),
            placeholder: palette.query_placeholder().to_owned(),
            field: palette
                .argument_step()
                .map(|(id, spec)| (id.to_owned(), spec.clone())),
            query,
            select: palette.selected_id().map(str::to_owned),
            status: None,
            searching: palette.more_coming(),
            field_only: palette.field_only_while_empty(),
            closed: !self.palette_visible,
        }
    }
}

/// A digest of what `answer` shows, to tell a changed answer from the one
/// on screen without keeping it.
fn digest(answer: &Answer) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for row in &answer.rows {
        row.id.hash(&mut hasher);
        row.label.hash(&mut hasher);
        row.description.hash(&mut hasher);
        row.description_facts.hash(&mut hasher);
        row.badge.hash(&mut hasher);
        row.group.hash(&mut hasher);
        row.group_total.hash(&mut hasher);
        row.section.hash(&mut hasher);
        row.outcome.hash(&mut hasher);
        row.adjustable.hash(&mut hasher);
        row.image.is_some().hash(&mut hasher);
        row.faces.len().hash(&mut hasher);
    }
    answer.narrows.hash(&mut hasher);
    for (_, label) in &answer.kinds {
        label.hash(&mut hasher);
    }
    answer.band.hash(&mut hasher);
    answer.scope_token.hash(&mut hasher);
    answer.scope_token_action.hash(&mut hasher);
    answer.placeholder.hash(&mut hasher);
    if let Some((id, spec)) = &answer.field {
        id.hash(&mut hasher);
        spec.prompt.hash(&mut hasher);
        spec.prefill.hash(&mut hasher);
    }
    answer.query.hash(&mut hasher);
    answer.select.hash(&mut hasher);
    answer.searching.hash(&mut hasher);
    answer.field_only.hash(&mut hasher);
    hasher.finish()
}

impl ShellOverlayHost {
    /// The search card's layout node: the launcher's, as tall as its card
    /// may grow (its list up to `list_max`) and as wide as the room search
    /// has, or the palette's own where the palette draws itself.
    pub(super) fn build_search_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        list_max: f64,
    ) -> NodeId {
        self.mount_launcher();
        if self.launcher.is_none() {
            return self.palette.build_layout(taffy, theme, node_map);
        }
        // Closed, the launcher keeps its place, unseen and out of the
        // focus order ([`Widget::children`] leaves it out): opening it then
        // changes no layout, and the first key finds its field.
        let (frame, _) = self.palette_place(theme);
        let height = self.palette.chrome_height() + list_max;
        let Some(launcher) = self.launcher.as_mut() else {
            return self.palette.build_layout(taffy, theme, node_map);
        };
        launcher.link.set_room(frame.width(), list_max);
        let node = launcher.app.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            style.size = Size {
                width: Dimension::length(f32_of_f64(frame.width())),
                height: Dimension::length(f32_of_f64(height)),
            };
            let _ = taffy.set_style(node, style);
        }
        node
    }
}

impl ShellOverlayHost {
    /// The launcher as the shell sees it: whether it is open, the rows it
    /// was last shown, and its card's place. `None` while search draws
    /// itself.
    #[doc(hidden)]
    pub fn launcher_for_tests(&self) -> Option<(bool, Vec<String>, Rect)> {
        let launcher = self.launcher.as_ref()?;
        let (_, answer) = launcher.link.answer();
        Some((
            launcher.link.is_open(),
            answer.rows.into_iter().map(|row| row.id).collect(),
            launcher.app.get_rect(),
        ))
    }
}
