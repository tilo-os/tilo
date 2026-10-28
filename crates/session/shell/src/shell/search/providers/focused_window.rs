//! An app's search, answered by the focused window itself.
//!
//! Scoped to the focused app ([`SearchScope::ActiveApp`]), search shows what
//! that window holds and what can be done in it. The window publishes both
//! (its resources and its search providers, the catalogue its own controls,
//! agents and peers use), so the overlay host reads them from the window
//! for each query and lends them here ([`FocusedWindowProvider::lend`]);
//! the rows are [`tilo_desktop_search::window::WindowSearch`]'s, the same
//! the portable desktop shows. Nothing is kept once the query is answered,
//! except a search still waiting for a provider's late answer.
//!
//! A row chosen here runs inside the window, as its own control would: it
//! is queued for the overlay host ([`WindowControl`]), which hands it to
//! the window's app. While the window answers for itself, the broker's
//! projection of the same app ([`super::live_app_actions`]) gives way to
//! it ([`SearchProvider::gives_way_to_window`]), so no action is listed
//! twice; a window the shell cannot read (another process's) is still
//! searched through the broker.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use tilo_desktop_search::model::{Source, Target};
use tilo_desktop_search::window::WindowSearch;

use crate::shell::chrome_state::{Chrome, SearchScope};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// This provider's id.
pub const PROVIDER_ID: &str = "focused-window";

const ID_SEPARATOR: char = '\u{1f}';

/// What the focused window publishes, read for one query.
pub struct WindowReading {
    /// The tab the window shows.
    pub tab: u64,
    /// The app the tab shows, as its windows announce it.
    pub app: String,
    pub search: WindowSearch,
    /// Identifies what the window published when it was read: the rows
    /// change exactly when it does.
    pub revision: u64,
}

impl WindowReading {
    /// Read what `window` publishes now, for tab `tab` of `app`.
    #[must_use]
    pub fn of(
        window: &ui::widget::TileHost,
        doc: &automerge::AutoCommit,
        tab: u64,
        app: String,
    ) -> Self {
        let ui::widget::WindowPublication {
            resources,
            providers,
        } = window.window_search(doc);
        Self::from_publication(resources, providers, tab, app)
    }

    /// What a window published, for tab `tab` of `app`.
    #[must_use]
    pub fn from_publication(
        resources: Vec<tilo_actions::ResourceSpec>,
        providers: Vec<Box<dyn ui::search::SearchProvider>>,
        tab: u64,
        app: String,
    ) -> Self {
        fn labels(resource: &tilo_actions::ResourceSpec, hasher: &mut impl Hasher) {
            resource.id.as_str().hash(hasher);
            resource.label.hash(hasher);
            for child in &resource.children {
                labels(child, hasher);
            }
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tab.hash(&mut hasher);
        providers.len().hash(&mut hasher);
        for flat in tilo_actions::flatten(&resources) {
            flat.key.hash(&mut hasher);
            flat.resource_label.hash(&mut hasher);
            flat.action.label.hash(&mut hasher);
            flat.action
                .contract
                .availability
                .is_available()
                .hash(&mut hasher);
        }
        for resource in &resources {
            labels(resource, &mut hasher);
        }
        Self {
            tab,
            app,
            search: WindowSearch::new(resources, providers),
            revision: hasher.finish(),
        }
    }
}

/// A row chosen in the focused window's search, waiting for the overlay
/// host to hand it to the window's app.
#[derive(Debug)]
pub struct WindowControl {
    pub tab: u64,
    pub invocation: tilo_actions::ControlInvocation,
}

/// See the module docs.
#[derive(Default)]
pub struct FocusedWindowProvider {
    lent: RefCell<Option<WindowReading>>,
    /// The icon of each row of the last answer.
    icons: RefCell<HashMap<String, ui::icons::Icon>>,
}

/// The focused window lent for one query; taken back when dropped.
pub struct Lent(Rc<FocusedWindowProvider>);

impl Drop for Lent {
    fn drop(&mut self) {
        let mut lent = self.0.lent.borrow_mut();
        // Kept only while a provider's answer is still coming, so it can be
        // asked again ([`FocusedWindowProvider::answer_arrived`]).
        if lent
            .as_ref()
            .is_some_and(|reading| !reading.search.waiting())
        {
            *lent = None;
        }
    }
}

impl FocusedWindowProvider {
    /// Lend `reading` to the queries run until the returned guard drops.
    #[must_use]
    pub fn lend(self: &Rc<Self>, reading: Option<WindowReading>) -> Lent {
        *self.lent.borrow_mut() = reading;
        Lent(Rc::clone(self))
    }

    /// Whether an answer the last query was still waiting for arrived, so
    /// search should be asked again.
    pub fn answer_arrived(&self) -> bool {
        let mut lent = self.lent.borrow_mut();
        let arrived = lent.as_ref().is_some_and(|reading| reading.search.poll());
        if lent
            .as_ref()
            .is_some_and(|reading| !reading.search.waiting())
        {
            *lent = None;
        }
        arrived
    }

    fn row_id(tab: u64, provider: &str, id: &str) -> String {
        format!("{tab}{ID_SEPARATOR}{provider}{ID_SEPARATOR}{id}")
    }
}

/// Queue `invocation` for the window of `tab`.
fn queue(chrome: &Chrome, tab: u64, invocation: tilo_actions::ControlInvocation) {
    let mut state = chrome.borrow_mut();
    state
        .window_controls
        .push(WindowControl { tab, invocation });
    state.bump();
}

impl SearchProvider for FocusedWindowProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "This window"
    }

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-focused-window")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[
            ResourceKind::Record,
            ResourceKind::Note,
            ResourceKind::File,
            ResourceKind::Folder,
            ResourceKind::Video,
            ResourceKind::Playlist,
            ResourceKind::Creator,
            ResourceKind::Action,
        ]
    }

    fn max_score(&self) -> i32 {
        ui::search::system::MAX_PROVIDER_SCORE
    }

    fn narrows_to_active_app(&self) -> bool {
        // Only ever the focused window's own rows, and only in its scope.
        true
    }

    fn speaks_for_active_window(&self, ctx: &SearchContext<'_>) -> bool {
        ctx.effective_scope() == SearchScope::ActiveApp
            && self
                .lent
                .borrow()
                .as_ref()
                .is_some_and(|reading| !reading.search.is_empty())
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        if ctx.effective_scope() != SearchScope::ActiveApp {
            return "unscoped".into();
        }
        self.lent
            .borrow()
            .as_ref()
            .map_or_else(|| "none".into(), |reading| reading.revision.to_string())
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        if let Some(icon) = self.icons.borrow().get(&result.id) {
            return Some(*icon);
        }
        let app = self
            .lent
            .borrow()
            .as_ref()
            .map(|reading| reading.app.clone())?;
        Some(ui::icons::app_glyph(&app, None))
    }

    fn row_image(&self, result: &SearchResult) -> Option<ui::icons::AppImage> {
        // An action without an icon of its own wears its app's picture.
        if result.kind != ResourceKind::Action || self.icons.borrow().contains_key(&result.id) {
            return None;
        }
        let app = self
            .lent
            .borrow()
            .as_ref()
            .map(|reading| reading.app.clone())?;
        crate::app_icons::for_window_app_id(&app)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let lent = self.lent.borrow();
        let Some(reading) = lent.as_ref() else {
            return Vec::new();
        };
        if ctx.effective_scope() != SearchScope::ActiveApp {
            return Vec::new();
        }
        let mut icons = HashMap::new();
        let tab = reading.tab;
        // A kind chosen in the filter band narrows the rows to it.
        let filtered = ctx.wanted.len() < self.resource_kinds().len();
        let rows = reading
            .search
            .query(ctx.needle)
            .into_iter()
            .filter(|hit| !filtered || ctx.wanted.contains(&hit.kind))
            .filter_map(|hit| {
                let Target::Action(target) = hit.target else {
                    return None;
                };
                let id = Self::row_id(tab, &hit.provider, &hit.id);
                let is_action = hit.kind == ResourceKind::Action;
                // The row's own picture; an action without one wears its
                // app's (`row_image`), a hit without one its kind's.
                let icon = hit
                    .icon
                    .or_else(|| (!is_action).then(|| ui::search::system::icon_for_kind(&hit.kind)));
                if let Some(icon) = icon {
                    icons.insert(id.clone(), icon);
                }
                let preset = target.preset.clone();
                let params = target
                    .spec
                    .params()
                    .iter()
                    .filter(|param| {
                        preset
                            .as_ref()
                            .is_none_or(|(name, _)| param.name.as_ref() != name)
                    })
                    .cloned()
                    .collect();
                let address = target.address();
                // What the row is: the item a hit opens at, else what the
                // action acts on.
                let resource = preset
                    .as_ref()
                    .map_or_else(|| address.resource.clone(), |(_, anchor)| anchor.clone());
                Some(SearchResult {
                    kind: hit.kind,
                    operation_kind: target.spec.kind,
                    provider_id: PROVIDER_ID.into(),
                    id,
                    title: hit.title,
                    subtitle: hit.subtitle,
                    facts: Vec::new(),
                    badge: None,
                    shortcut: None,
                    score: hit.score,
                    params,
                    contract: is_action.then(|| target.spec.contract.clone()),
                    on_open: Rc::new(move |_, _, chrome, args| {
                        let mut arguments = args.to_json();
                        if let Some((name, value)) = &preset {
                            arguments
                                .insert(name.clone(), serde_json::Value::String(value.clone()));
                        }
                        let invocation = tilo_actions::ControlInvocation::from_parts(
                            &address.resource,
                            &address.action,
                            arguments,
                        )
                        .ok_or_else(|| {
                            crate::shell::search::action_api::ActionError::Refused(
                                "the window's action has no address".into(),
                            )
                        })?;
                        queue(chrome, tab, invocation);
                        Ok(())
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                    resource: Some(resource),
                })
            })
            .collect();
        *self.icons.borrow_mut() = icons;
        rows
    }
}
