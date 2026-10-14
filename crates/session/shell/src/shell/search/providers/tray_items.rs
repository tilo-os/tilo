//! Tray items and their menus as actions.
//!
//! Every status-notifier item the shell hosts ([`crate::tray`]) is one
//! "Open" action, and every entry of its menu another — so "Quit Steam",
//! "Lock database" or "Pause syncing" is reachable from the palette, the
//! control socket, an agent or a screen reader, not only from a click on a
//! 22-pixel icon.
//!
//! Ids: `activate:<key>` and `menu:<dbusmenu id>:<key>`, where `<key>` is
//! the item's bus name and path. A query naming an id finds that row,
//! which is how an exact address resolves.

use std::rc::Rc;
use std::sync::Arc;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::tray::{Tray, TrayItem, TrayRequest};

pub const PROVIDER_ID: &str = "tray";

pub struct TrayItemsProvider {
    tray: Arc<Tray>,
    /// The rows' names, indexed while they stay the same.
    ranker: ui::search::ListIndex,
}

impl TrayItemsProvider {
    pub fn new(tray: Arc<Tray>) -> Self {
        Self {
            tray,
            ranker: ui::search::ListIndex::default(),
        }
    }

    /// The session's tray.
    pub fn global() -> Self {
        Self::new(Arc::clone(Tray::global()))
    }
}

pub fn activate_id(key: &str) -> String {
    format!("activate:{key}")
}

pub fn menu_id(key: &str, id: i32) -> String {
    format!("menu:{id}:{key}")
}

/// A tray row's names, each of which names it.
fn names(names: &[&str]) -> Vec<(String, ui::search::FieldWeight)> {
    names
        .iter()
        .map(|name| ((*name).to_owned(), ui::search::FieldWeight::TITLE))
        .collect()
}

fn row(
    tray: &Arc<Tray>,
    item: &TrayItem,
    id: String,
    title: String,
    score: i32,
    request: TrayRequest,
) -> SearchResult {
    let tray = Arc::clone(tray);
    let key = item.key.clone();
    SearchResult {
        kind: ResourceKind::Action,
        operation_kind: tilo_actions::OperationKind::Effect,
        provider_id: PROVIDER_ID.into(),
        id,
        title,
        subtitle: Some(t_args(
            "search-tray-in-top-bar",
            &[("item", item.title.as_str().into())],
        )),
        badge: Some(t("search-badge-tray")),
        score,
        params: Vec::new(),
        contract: Some(tilo_actions::ActionContract {
            tags: vec!["desktop".into(), "tray".into()],
            ..tilo_actions::ActionContract::default()
        }),
        on_open: Rc::new(move |_doc, _root, _chrome: &Chrome, _args| {
            tray.request(&key, request).map_err(ActionError::Refused)
        }),
        on_invoke: None,
        on_begin_invoke: None,
    }
}

impl SearchProvider for TrayItemsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Tray"
    }

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-tray")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut fields = Vec::new();
        for item in self.tray.snapshot() {
            let open_id = activate_id(&item.key);
            let open_title = t_args("search-tray-open", &[("item", item.title.as_str().into())]);
            if !item.item_is_menu {
                fields.push(names(&[
                    &item.title,
                    &item.id,
                    "open",
                    &open_title,
                    &open_id,
                ]));
                out.push(row(
                    &self.tray,
                    &item,
                    open_id,
                    open_title,
                    0,
                    TrayRequest::Activate { x: 0, y: 0 },
                ));
            }
            for entry in item.menu.iter().filter(|entry| entry.enabled) {
                let entry_id = menu_id(&item.key, entry.id);
                fields.push(names(&[&item.title, &entry.label, &entry_id]));
                let args: [(&str, ui::i18n::ArgValue); 2] = [
                    ("item", item.title.as_str().into()),
                    ("entry", entry.label.as_str().into()),
                ];
                let title = match entry.toggled {
                    Some(true) => t_args("search-tray-entry-on", &args),
                    Some(false) => t_args("search-tray-entry-off", &args),
                    None => t_args("search-tray-entry", &args),
                };
                out.push(row(
                    &self.tray,
                    &item,
                    entry_id,
                    title,
                    0,
                    TrayRequest::Menu { id: entry.id },
                ));
            }
        }
        crate::shell::search::rank_rows(&self.ranker, ctx.needle.trim(), out, &fields)
    }
}
