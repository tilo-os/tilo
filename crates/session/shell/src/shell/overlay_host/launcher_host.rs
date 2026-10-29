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

/// The card's height less its list until the launcher measured it: a field
/// and its padding.
const SEARCH_CHROME_HEIGHT: f64 = 72.0;

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
    /// Where the launcher's card stands, in this host's coordinates;
    /// nothing while there is no launcher.
    pub(crate) fn search_card(&self) -> Rect {
        let Some(launcher) = self.launcher.as_ref() else {
            return Rect::ZERO;
        };
        let origin = launcher.app.get_rect().origin();
        launcher
            .link
            .fit()
            .map_or(Rect::ZERO, |fit| on_screen(fit.card, origin))
    }

    /// The place laid out for the launcher: its card, and the room below it
    /// its list and its filters' menus may take.
    pub(super) fn search_place(&self) -> Rect {
        self.launcher
            .as_ref()
            .map_or(Rect::ZERO, |launcher| launcher.app.get_rect())
    }

    /// The row of the launcher's card at `point`, by id.
    pub(super) fn search_row_at(&self, point: Point) -> Option<String> {
        let launcher = self.launcher.as_ref().filter(|_| self.palette_visible)?;
        let origin = launcher.app.get_rect().origin();
        launcher
            .link
            .fit()?
            .shown
            .into_iter()
            .find(|(_, rect)| on_screen(*rect, origin).contains(point))
            .map(|(id, _)| id)
    }

    /// The modifiers held changed: the launcher's selected row previews
    /// what choosing it with them does. Whether that needs a new frame.
    pub fn note_held_modifiers(&mut self, modifiers: winit::keyboard::ModifiersState) -> bool {
        self.launcher
            .as_ref()
            .is_some_and(|launcher| launcher.link.hold_modifiers(modifiers))
            && self.palette_visible
    }

    /// The height of the card less its list, as the launcher measured it.
    pub(super) fn search_chrome_height(&self) -> f64 {
        self.launcher
            .as_ref()
            .and_then(|launcher| launcher.link.fit())
            .map_or(SEARCH_CHROME_HEIGHT, |fit| fit.chrome_height)
    }

    /// Mount the launcher once the session lends the apps it mounts in its
    /// own process. A host without them (a bare test host) keeps search's
    /// state and shows no card.
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
        // The rows a full list holds decide how many each group shows.
        if let Some(fit) = link.fit()
            && self.search.set_row_capacity(fit.rows)
        {
            self.last_search_revision.clear();
            self.palette_layout_dirty = true;
        }
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
                    self.search.set_query(&query);
                }
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
                self.search.set_query(&search.query);
                self.refresh_search_results(edits, root);
            }
            Ask::Choose { row, modifier } => {
                if self.search.select(&row) {
                    self.activate_selected_result_with(edits, root, modifier);
                }
            }
            Ask::Select { row } => {
                // The person's choice: rows changing under it keep it on
                // the same result.
                self.search.select(&row);
            }
            Ask::Adjust { row, delta } => self.step_result(&row, delta, edits, root),
            Ask::Argument(text) => self.submit_param_field(text, edits, root),
            Ask::Back => self.palette_step_back(),
            Ask::Widen => self.widen_search_scope(),
            Ask::Dismissed => {
                if self.palette_visible {
                    self.palette_visible = false;
                    self.search.close();
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
        let search = &self.search;
        let query = search.query().to_owned();
        let asked = self
            .launcher
            .as_ref()
            .map(|launcher| launcher.asked_query.as_str());
        let query = (search.argument_step().is_none()
            && search.bypasses_fuzzy()
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
        let search = &self.search;
        Answer {
            rows: search.entries().to_vec(),
            narrows: !search.bypasses_fuzzy(),
            kinds: self.search_kinds(),
            band: search.filter_band_shown(),
            scope,
            scope_token: search.scope_token().map(str::to_owned),
            scope_token_action: search.scope_token_action().map(str::to_owned),
            placeholder: search.query_placeholder().to_owned(),
            field: search
                .argument_step()
                .map(|(id, spec)| (id.to_owned(), spec.clone())),
            query,
            select: search.selected_id().map(str::to_owned),
            status: search
                .selected_status()
                .map(|(title, detail)| (title.to_owned(), detail.to_owned())),
            searching: false,
            field_only: search.field_only_while_empty(),
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
    answer.status.hash(&mut hasher);
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
        // Closed, the launcher keeps its place, unseen and out of the
        // focus order ([`Widget::children`] leaves it out): opening it then
        // changes no layout, and the first key finds its field.
        let (frame, _) = self.palette_place(theme);
        let height = self.search_chrome_height() + list_max;
        let Some(launcher) = self.launcher.as_mut() else {
            // No launcher: an empty place keeps the host's children where
            // they are.
            return taffy
                .new_leaf(Style::default())
                .unwrap_or_else(|_| NodeId::new(0));
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

#[cfg(test)]
impl ShellOverlayHost {
    /// Put `ask` to search as the launcher puts it, and answer it.
    pub(crate) fn ask_as_launcher(&mut self, ask: Ask) {
        let mut doc = AutoCommit::new();
        let mut edits = ui::widget::Edits::lend(&mut doc);
        self.answer_launcher(ask, &mut edits, &automerge::ROOT);
    }

    /// Choose the selected row with `modifier` held, as Enter in the
    /// launcher does.
    pub(crate) fn choose_selected_as_launcher(
        &mut self,
        modifier: Option<launcher_app::source::ActivationModifier>,
    ) {
        if let Some(row) = self.search.selected_id().map(str::to_owned) {
            self.ask_as_launcher(Ask::Choose { row, modifier });
        }
    }
}

/// `rect`, laid out in the launcher's own space, where it shows on screen
/// with the launcher placed at `origin`.
fn on_screen(rect: Rect, origin: Point) -> Rect {
    Rect::new(
        rect.x0 + origin.x,
        rect.y0 + origin.y,
        rect.x1 + origin.x,
        rect.y1 + origin.y,
    )
}
