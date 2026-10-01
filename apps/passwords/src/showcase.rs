use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::scrollbar::{ScrollMetrics, Scrollbar};
use ui::widget::{
    Button, ButtonFace, SegmentedFace, SegmentedStyle, SidebarDrawer, SidebarNav, SidebarNavItem,
    SidebarNavSection, TextInput, ToggleGroup, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::Zeroizing;

use crate::actions;
use crate::transfer::ImportLogin;
use accounts::{CredentialGrant, GeneratedPasswordPolicy, SecretId, SecretItem, SecretItemKind};

enum TransferDialog {
    ImportPreview {
        rows: Vec<ImportLogin>,
        conflicts: usize,
    },
    ExportPlaintext {
        acknowledged: bool,
    },
    Backup,
    Restore {
        bytes: Zeroizing<Vec<u8>>,
    },
    DeleteConfirm {
        id: SecretId,
        title: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ActiveField {
    Search,
    Name,
    Username,
    Origin,
    Password,
    Notes,
    Tags,
    CustomFields,
}

struct EditorInputs {
    name: TextInput,
    username: TextInput,
    origin: TextInput,
    password: TextInput,
    notes: TextInput,
    tags: TextInput,
    custom_fields: TextInput,
}

fn backup_input(label: &str) -> TextInput {
    TextInput::local()
        .with_kind(ui::widget::TextInputKind::Password)
        .with_accessible_name(label)
        .with_placeholder(label)
        .with_secret(true)
}

fn search_input() -> TextInput {
    TextInput::local()
        .with_id("passwords.search")
        .with_accessible_name("Search passwords")
        .with_kind(ui::widget::TextInputKind::Search)
        .with_plain_chrome()
        .with_font_size(14.0)
        .with_placeholder("Search passwords")
}

impl EditorInputs {
    fn new(name: &str, username: &str, origin: &str, password: &str, notes: &str) -> Self {
        Self {
            name: TextInput::local()
                .with_id("passwords.field.name")
                .with_accessible_name("Name")
                .with_initial_value(name),
            username: TextInput::local()
                .with_id("passwords.field.username")
                .with_accessible_name("Username or label")
                .with_initial_value(username),
            origin: TextInput::local()
                .with_id("passwords.field.origin")
                .with_accessible_name("Sign-in addresses")
                .with_initial_value(origin),
            password: TextInput::local()
                .with_id("passwords.field.password")
                .with_accessible_name("Password or secret")
                .with_secret(true)
                .with_initial_value(password),
            notes: TextInput::local()
                .with_id("passwords.field.notes")
                .with_accessible_name("Notes")
                .multiline(true)
                .with_initial_value(notes),
            tags: TextInput::local()
                .with_id("passwords.field.tags")
                .with_accessible_name("Tags")
                .with_placeholder("Tags separated by commas"),
            custom_fields: TextInput::local()
                .with_id("passwords.field.custom-fields")
                .with_accessible_name("Custom fields")
                .with_placeholder("One field per line: label = value")
                .multiline(true),
        }
    }

    fn get_mut(&mut self, field: ActiveField) -> Option<&mut TextInput> {
        match field {
            ActiveField::Name => Some(&mut self.name),
            ActiveField::Username => Some(&mut self.username),
            ActiveField::Origin => Some(&mut self.origin),
            ActiveField::Password => Some(&mut self.password),
            ActiveField::Notes => Some(&mut self.notes),
            ActiveField::Tags => Some(&mut self.tags),
            ActiveField::CustomFields => Some(&mut self.custom_fields),
            ActiveField::Search => None,
        }
    }

    fn get(&self, field: ActiveField) -> Option<&TextInput> {
        match field {
            ActiveField::Name => Some(&self.name),
            ActiveField::Username => Some(&self.username),
            ActiveField::Origin => Some(&self.origin),
            ActiveField::Password => Some(&self.password),
            ActiveField::Notes => Some(&self.notes),
            ActiveField::Tags => Some(&self.tags),
            ActiveField::CustomFields => Some(&self.custom_fields),
            ActiveField::Search => None,
        }
    }

    fn for_each_mut(&mut self, mut visit: impl FnMut(ActiveField, &mut TextInput)) {
        visit(ActiveField::Name, &mut self.name);
        visit(ActiveField::Username, &mut self.username);
        visit(ActiveField::Origin, &mut self.origin);
        visit(ActiveField::Password, &mut self.password);
        visit(ActiveField::Notes, &mut self.notes);
        visit(ActiveField::Tags, &mut self.tags);
        visit(ActiveField::CustomFields, &mut self.custom_fields);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowcaseMode {
    Vault,
    Editor,
    Health,
    AppAccess,
    Import,
    Export,
    DeleteConfirm,
}

fn compact_content_rect(mode: ShowcaseMode, main: Rect) -> Rect {
    if matches!(
        mode,
        ShowcaseMode::Editor
            | ShowcaseMode::Health
            | ShowcaseMode::AppAccess
            | ShowcaseMode::DeleteConfirm
    ) {
        main
    } else {
        Rect::new(main.x0 + 60.0, main.y0, main.x1, main.y1)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum VaultFilter {
    #[default]
    All,
    Favorites,
    Kind(SecretItemKind),
}

impl VaultFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Favorites => "favorites",
            Self::Kind(SecretItemKind::Login) => "login",
            Self::Kind(SecretItemKind::ApiKey) => "api-key",
            Self::Kind(SecretItemKind::SecureNote) => "note",
            Self::Kind(SecretItemKind::Wifi) => "wifi",
            Self::Kind(SecretItemKind::Other) => "other",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "favorites" => Self::Favorites,
            "login" => Self::Kind(SecretItemKind::Login),
            "api-key" => Self::Kind(SecretItemKind::ApiKey),
            "note" => Self::Kind(SecretItemKind::SecureNote),
            "wifi" => Self::Kind(SecretItemKind::Wifi),
            "other" => Self::Kind(SecretItemKind::Other),
            _ => Self::All,
        }
    }
}

pub struct PasswordsSurface {
    rect: Rect,
    model: Option<crate::PasswordsModel>,
    items: Vec<SecretItem>,
    selected: usize,
    mode: ShowcaseMode,
    vault_filter: VaultFilter,
    last_pointer: Point,
    drawer: SidebarDrawer,
    /// Text at 200% needs the same single-pane path as a narrow viewport.  A
    /// split list/detail layout otherwise leaves neither pane usable.
    compact_for_large_text: bool,
    /// Retained navigation position for short/landscape phone drawers.
    sidebar_scroll: f64,
    sidebar_touch: Option<(u64, Point, f64)>,
    /// First filtered vault row shown in the retained list viewport.
    list_scroll: usize,
    list_touch: Option<(u64, Point, usize)>,
    editor_scroll: f64,
    editor_touch: Option<(u64, Point, f64)>,
    app_access_scroll: f64,
    app_access_touch: Option<(u64, Point, f64)>,
    query: String,
    search_input: TextInput,
    editing: Option<SecretId>,
    draft_title: String,
    draft_kind: SecretItemKind,
    draft_username: String,
    draft_origin: String,
    draft_password: Zeroizing<String>,
    draft_notes: Zeroizing<String>,
    draft_tags: String,
    draft_custom_fields: Zeroizing<String>,
    editor_inputs: EditorInputs,
    active_field: Option<ActiveField>,
    /// Explicit, short-lived disclosure. The value zeroizes when it expires,
    /// when focus leaves the window, or when another item is revealed.
    revealed: Option<(SecretId, Zeroizing<String>, ui::time::Instant)>,
    status: String,
    mobile_detail: bool,
    health: crate::VaultHealth,
    app_grants: Vec<CredentialGrant>,
    import_portal: Option<ui::portal::OpenFileChannel>,
    import_parse: Option<std::sync::mpsc::Receiver<Result<Vec<ImportLogin>, String>>>,
    export_portal: Option<ui::portal::PortalChannel>,
    backup_portal: Option<ui::portal::PortalChannel>,
    restore_portal: Option<ui::portal::OpenFileChannel>,
    transfer_poll_deadline: std::cell::Cell<Option<ui::time::Instant>>,
    backup_password: TextInput,
    backup_confirmation: TextInput,
    backup_focus: usize,
    transfer_dialog: Option<TransferDialog>,
    /// Document actions: local host standalone; framework dispatcher when hosted.
    actions: crate::resource::ActionSink,
}

impl PasswordsSurface {
    const DRAWER_BREAKPOINT: f64 = 960.0;
    const LARGE_TEXT_DRAWER_BREAKPOINT: f64 = 10_000.0;

    fn resolve_drawer(&mut self, content: Rect) -> Rect {
        self.drawer.set_breakpoint(if self.compact_for_large_text {
            Self::LARGE_TEXT_DRAWER_BREAKPOINT
        } else {
            Self::DRAWER_BREAKPOINT
        });
        self.drawer.resolve(content)
    }

    fn adapt_layout_for_text_scale(&mut self, theme: &Theme) {
        let compact = theme.text_scale >= 1.5;
        if compact != self.compact_for_large_text {
            self.compact_for_large_text = compact;
            // Returning to the list prevents a stale detail pane from being
            // stranded behind the large-text navigation drawer.
            self.mobile_detail = false;
        }
    }

    fn request_delete(&mut self, id: SecretId) {
        let title = self
            .items
            .iter()
            .find(|item| item.id == id)
            .map_or_else(|| "this item".to_owned(), |item| item.title.clone());
        self.transfer_dialog = Some(TransferDialog::DeleteConfirm { id, title });
    }

    fn confirm_delete(&mut self) {
        let Some(TransferDialog::DeleteConfirm { id, .. }) = self.transfer_dialog.take() else {
            return;
        };
        if let Some(model) = &mut self.model {
            match model.remove(&id) {
                Ok(true) => {
                    self.refresh();
                    self.cancel_edit();
                    self.status = "Item deleted".into();
                }
                Ok(false) => self.status = "That item is no longer in your vault".into(),
                Err(error) => self.status = format!("Could not delete item: {error}"),
            }
        }
    }

    fn kind_label(kind: SecretItemKind) -> &'static str {
        match kind {
            SecretItemKind::Login => "Login",
            SecretItemKind::ApiKey => "API key",
            SecretItemKind::SecureNote => "Secure note",
            SecretItemKind::Wifi => "Wi-Fi password",
            SecretItemKind::Other => "Secret",
        }
    }

    fn item_summary(item: &SecretItem) -> String {
        match item.kind {
            SecretItemKind::Login => item
                .origins
                .iter()
                .next()
                .cloned()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| item.username.clone()),
            SecretItemKind::ApiKey => {
                if item.username.is_empty() {
                    "API credential".into()
                } else {
                    item.username.clone()
                }
            }
            SecretItemKind::SecureNote => "Note".into(),
            SecretItemKind::Wifi => item
                .fields
                .get("network")
                .cloned()
                .unwrap_or_else(|| "Wi-Fi network".into()),
            SecretItemKind::Other => item
                .tags
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "Stored secret".into()),
        }
    }

    /// Determine whether a non-secret item has a private note without moving
    /// that note into the render/a11y/resource cache.
    fn has_private_note(&self, item: &SecretItem) -> bool {
        self.model.as_ref().is_some_and(|model| {
            model
                .items()
                .into_iter()
                .find(|candidate| candidate.id == item.id)
                .is_some_and(|candidate| !candidate.notes.trim().is_empty())
        })
    }

    fn is_favorite(item: &SecretItem) -> bool {
        item.tags.contains("favorite")
    }

    fn filter_matches(&self, item: &SecretItem) -> bool {
        match self.vault_filter {
            VaultFilter::All => true,
            VaultFilter::Favorites => Self::is_favorite(item),
            VaultFilter::Kind(kind) => item.kind == kind,
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| self.filter_matches(item).then_some(index))
            .collect()
    }

    fn list_capacity(rect: Rect) -> usize {
        let first_row = rect.y0 + 168.0;
        ui::num::usize_of_f64(((rect.y1 - first_row) / 76.0).floor().max(0.0))
    }

    fn max_list_scroll(&self, rect: Rect) -> usize {
        self.visible_indices()
            .len()
            .saturating_sub(Self::list_capacity(rect).max(1))
    }

    fn scroll_list_to(&mut self, rect: Rect, offset: usize) -> bool {
        let next = offset.min(self.max_list_scroll(rect));
        let changed = next != self.list_scroll;
        self.list_scroll = next;
        changed
    }

    fn ensure_selected_visible(&mut self, rect: Rect) {
        let visible = self.visible_indices();
        let Some(position) = visible.iter().position(|index| *index == self.selected) else {
            self.list_scroll = 0;
            return;
        };
        let capacity = Self::list_capacity(rect).max(1);
        if position < self.list_scroll {
            self.list_scroll = position;
        } else if position >= self.list_scroll.saturating_add(capacity) {
            self.list_scroll = position.saturating_add(1).saturating_sub(capacity);
        }
        self.list_scroll = self.list_scroll.min(self.max_list_scroll(rect));
    }

    fn current_list_rect(&self) -> Rect {
        let content = Rect::new(
            self.rect.x0 + 24.0,
            self.rect.y0 + 24.0,
            self.rect.x1 - 24.0,
            self.rect.y1 - 24.0,
        );
        let main = self.drawer.content_rect();
        if self.drawer.is_docked() {
            Rect::new(main.x0, content.y0 + 4.0, main.x0 + 330.0, content.y1)
        } else {
            Rect::new(main.x0 + 60.0, main.y0, main.x1, main.y1)
        }
    }

    fn current_panel_rect(&self) -> Rect {
        let content = Rect::new(
            self.rect.x0 + 24.0,
            self.rect.y0 + 24.0,
            self.rect.x1 - 24.0,
            self.rect.y1 - 24.0,
        );
        let main = self.drawer.content_rect();
        if self.drawer.is_docked() {
            Rect::new(main.x0 + 354.0, content.y0, content.x1, content.y1)
        } else {
            self.current_list_rect()
        }
    }

    fn move_list_selection(&mut self, doc: &mut AutoCommit, forward: bool) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let current = visible
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let last = visible.len().saturating_sub(1);
        let next = if forward {
            current.saturating_add(1).min(last)
        } else {
            current.saturating_sub(1)
        };
        if let Some(selected) = visible.get(next).copied() {
            self.select_index(doc, selected);
        }
        self.ensure_selected_visible(self.current_list_rect());
    }

    fn select_filter(&mut self, doc: &mut AutoCommit, filter: VaultFilter) {
        self.commit_view(
            doc,
            &crate::resource::PasswordsAction::SetFilter {
                filter: filter.as_str().into(),
            },
        );
        self.mode = ShowcaseMode::Vault;
        self.mobile_detail = false;
        self.list_scroll = 0;
        if let Some(first) = self.visible_indices().first().copied() {
            self.select_index(doc, first);
        }
    }

    fn select_index(&mut self, doc: &mut AutoCommit, index: usize) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let id = item.id.0.clone();
        self.selected = index;
        self.commit_view(doc, &crate::resource::PasswordsAction::SelectItem { id });
    }

    fn toggle_favorite(&mut self, index: usize) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let id = item.id.clone();
        let was_favorite = Self::is_favorite(item);
        if let Some(model) = &mut self.model {
            let Some(mut full_item) = model.items().into_iter().find(|item| item.id == id) else {
                return;
            };
            if was_favorite {
                full_item.tags.remove("favorite");
            } else {
                full_item.tags.insert("favorite".into());
            }
            if let Err(error) = model.update(&mut full_item) {
                self.status = error;
                return;
            }
            self.refresh();
        } else if let Some(item) = self.items.get_mut(index) {
            if was_favorite {
                item.tags.remove("favorite");
            } else {
                item.tags.insert("favorite".into());
            }
        }
        self.status = if was_favorite {
            "Removed from favorites".into()
        } else {
            "Added to favorites".into()
        };
    }

    pub fn live() -> Self {
        Self {
            rect: Rect::ZERO,
            model: Some(crate::PasswordsModel::open()),
            items: Vec::new(),
            selected: 0,
            mode: ShowcaseMode::Vault,
            vault_filter: VaultFilter::All,
            last_pointer: Point::ZERO,
            drawer: SidebarDrawer::new()
                .docked_width(214.0)
                .overlay_width(292.0)
                .breakpoint(Self::DRAWER_BREAKPOINT),
            compact_for_large_text: false,
            sidebar_scroll: 0.0,
            sidebar_touch: None,
            list_scroll: 0,
            list_touch: None,
            editor_scroll: 0.0,
            editor_touch: None,
            app_access_scroll: 0.0,
            app_access_touch: None,
            query: String::new(),
            search_input: search_input(),
            editing: None,
            draft_title: String::new(),
            draft_kind: SecretItemKind::Login,
            draft_username: String::new(),
            draft_origin: String::new(),
            draft_password: Zeroizing::new(String::new()),
            draft_notes: Zeroizing::new(String::new()),
            draft_tags: String::new(),
            draft_custom_fields: Zeroizing::new(String::new()),
            editor_inputs: EditorInputs::new("", "", "", "", ""),
            active_field: None,
            revealed: None,
            status: String::new(),
            mobile_detail: false,
            health: crate::VaultHealth::default(),
            app_grants: Vec::new(),
            import_portal: None,
            import_parse: None,
            export_portal: None,
            backup_portal: None,
            restore_portal: None,
            transfer_poll_deadline: std::cell::Cell::new(None),
            backup_password: backup_input("Backup password"),
            backup_confirmation: backup_input("Confirm password"),
            backup_focus: 0,
            transfer_dialog: None,
            actions: crate::resource::ActionSink::default(),
        }
        .loaded()
    }
    pub fn demo(mode: ShowcaseMode) -> Self {
        let editing = matches!(mode, ShowcaseMode::Editor | ShowcaseMode::DeleteConfirm)
            .then(|| SecretId("demo-0".into()));
        let items = demo_items();
        let health = crate::analyze_health(&items);
        let transfer_dialog = match mode {
            ShowcaseMode::Import => Some(TransferDialog::ImportPreview {
                rows: vec![
                    ImportLogin {
                        title: Zeroizing::new("Work account".into()),
                        username: Zeroizing::new("ada@work.example".into()),
                        origin: Zeroizing::new("https://work.example".into()),
                        password: Zeroizing::new("not-rendered".into()),
                        notes: Zeroizing::new(String::new()),
                    },
                    ImportLogin {
                        title: Zeroizing::new("Personal mail".into()),
                        username: Zeroizing::new("ada@example.com".into()),
                        origin: Zeroizing::new("https://mail.example".into()),
                        password: Zeroizing::new("not-rendered".into()),
                        notes: Zeroizing::new(String::new()),
                    },
                ],
                conflicts: 1,
            }),
            ShowcaseMode::Export => Some(TransferDialog::ExportPlaintext {
                acknowledged: false,
            }),
            ShowcaseMode::DeleteConfirm => Some(TransferDialog::DeleteConfirm {
                id: SecretId("demo-0".into()),
                title: "Northstar Bank".into(),
            }),
            _ => None,
        };
        Self {
            rect: Rect::ZERO,
            model: None,
            items,
            selected: 0,
            mode,
            vault_filter: VaultFilter::All,
            last_pointer: Point::ZERO,
            drawer: SidebarDrawer::new()
                .docked_width(214.0)
                .overlay_width(292.0)
                .breakpoint(Self::DRAWER_BREAKPOINT),
            compact_for_large_text: false,
            sidebar_scroll: 0.0,
            sidebar_touch: None,
            list_scroll: 0,
            list_touch: None,
            editor_scroll: 0.0,
            editor_touch: None,
            app_access_scroll: 0.0,
            app_access_touch: None,
            query: String::new(),
            search_input: search_input(),
            editing,
            draft_title: "Northstar Bank".into(),
            draft_kind: SecretItemKind::Login,
            draft_username: "ada@example.com".into(),
            draft_origin: "https://bank.example, https://login.bank.example".into(),
            draft_password: Zeroizing::new(String::new()),
            draft_notes: Zeroizing::new(String::new()),
            draft_tags: String::new(),
            draft_custom_fields: Zeroizing::new(String::new()),
            editor_inputs: EditorInputs::new(
                "Northstar Bank",
                "ada@example.com",
                "https://bank.example, https://login.bank.example",
                "",
                "",
            ),
            active_field: None,
            revealed: None,
            status: String::new(),
            mobile_detail: false,
            health,
            app_grants: Vec::new(),
            import_portal: None,
            import_parse: None,
            export_portal: None,
            backup_portal: None,
            restore_portal: None,
            transfer_poll_deadline: std::cell::Cell::new(None),
            backup_password: backup_input("Backup password"),
            backup_confirmation: backup_input("Confirm password"),
            backup_focus: 0,
            transfer_dialog,
            actions: crate::resource::ActionSink::default(),
        }
    }

    /// Deterministic overflow fixture for responsive screenshot and scrollbar
    /// review. Values remain synthetic and are never backed by the live vault.
    pub fn demo_long_list() -> Self {
        let mut app = Self::demo(ShowcaseMode::Vault);
        let seed = app.items.clone();
        for cycle in 1..=3 {
            for item in &seed {
                let mut copy = item.clone();
                copy.id = SecretId(format!("{}-{cycle}", copy.id.0));
                copy.title = format!("{} {cycle}", copy.title);
                app.items.push(copy);
            }
        }
        app.list_scroll = 4;
        app
    }

    fn loaded(mut self) -> Self {
        self.refresh();
        self
    }

    fn refresh(&mut self) {
        if let Some(model) = &self.model {
            if model.recovery_error().is_some() {
                self.status = "Vault unavailable. Restore key or storage access, then restart Passwords before saving.".into();
            }
            self.health = model.health();
            self.app_grants = model.app_grants();
            self.items = model
                .search(&self.query)
                .into_iter()
                .map(|mut item| {
                    item.password.clear();
                    // The list/detail cache feeds rendering, resources and
                    // accessibility. Keep it a metadata-only projection: a
                    // note or arbitrary field value can be secret too.
                    item.notes.clear();
                    item.fields.clear();
                    item
                })
                .collect();
            self.selected = self.selected.min(self.items.len().saturating_sub(1));
        }
    }

    fn sync_editor_inputs(&mut self) {
        self.editor_inputs.name.set_text(&self.draft_title);
        self.editor_inputs.username.set_text(&self.draft_username);
        self.editor_inputs.origin.set_text(&self.draft_origin);
        self.editor_inputs.password.set_text(&self.draft_password);
        self.editor_inputs.notes.set_text(&self.draft_notes);
        self.editor_inputs.tags.set_text(&self.draft_tags);
        self.editor_inputs
            .custom_fields
            .set_text(&self.draft_custom_fields);
    }

    fn update_draft_from_input(&mut self, field: ActiveField, value: String) {
        match field {
            ActiveField::Name => self.draft_title = value,
            ActiveField::Username => self.draft_username = value,
            ActiveField::Origin => self.draft_origin = value,
            ActiveField::Password => self.draft_password = Zeroizing::new(value),
            ActiveField::Notes => self.draft_notes = Zeroizing::new(value),
            ActiveField::Tags => self.draft_tags = value,
            ActiveField::CustomFields => self.draft_custom_fields = Zeroizing::new(value),
            ActiveField::Search => {}
        }
    }

    fn set_search_query(&mut self, value: &str) {
        self.query.clear();
        self.query.push_str(value);
        self.search_input.set_text(value);
        self.list_scroll = 0;
        self.refresh();
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Passwords>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Adopt collaborative view prefs; never clear the local vault cache.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::VaultState) {
        self.vault_filter = state.filter;
        if self.query != state.search {
            self.set_search_query(&state.search);
        }
        if let Some(id) = &state.selected_id {
            if let Some(index) = self.items.iter().position(|item| item.id.0 == *id) {
                self.selected = index;
            }
        }
    }

    pub(crate) fn commit_view(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::resource::PasswordsAction,
    ) {
        self.apply_passwords_action_locally(action);
        self.actions.dispatch(doc, action);
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    #[cfg(test)]
    pub(crate) fn item_count(&self) -> usize {
        self.items.len()
    }

    fn apply_passwords_action_locally(&mut self, action: &crate::resource::PasswordsAction) {
        match action {
            crate::resource::PasswordsAction::SetFilter { filter } => {
                self.vault_filter = VaultFilter::parse(filter);
            }
            crate::resource::PasswordsAction::SetSearch { value } => {
                self.set_search_query(value);
            }
            crate::resource::PasswordsAction::SelectItem { id } => {
                if let Some(index) = self.items.iter().position(|item| item.id.0 == *id) {
                    self.selected = index;
                }
            }
        }
    }

    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`]
    /// with live vault item children overlaid.
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Passwords::read(doc).unwrap_or_default();
        actions::standalone_resources(&self.items, self.mode, &state)
    }

    /// Live vault item rows (and editor-session ops) without a vault root.
    pub(crate) fn live_item_extras(&self) -> Vec<ui::tilo_actions::Resource> {
        let mut extras = actions::live_item_resources(&self.items);
        if matches!(
            self.mode,
            ShowcaseMode::Editor | ShowcaseMode::DeleteConfirm
        ) {
            let mut session = ui::tilo_actions::Resource::new(
                "passwords:session",
                "passwords.session",
                "Editing session",
            );
            for spec in actions::editor_session_specs() {
                session = session.with_action(spec);
            }
            extras.insert(0, session);
        }
        extras
    }

    fn begin_add(&mut self) {
        self.mobile_detail = false;
        self.editing = None;
        self.draft_title.clear();
        self.draft_kind = SecretItemKind::Login;
        self.draft_username.clear();
        self.draft_origin.clear();
        self.draft_password.clear();
        self.draft_notes.clear();
        self.draft_tags.clear();
        self.draft_custom_fields.clear();
        self.active_field = Some(ActiveField::Name);
        self.status.clear();
        self.editor_scroll = 0.0;
        self.mode = ShowcaseMode::Editor;
        self.sync_editor_inputs();
    }

    fn begin_edit(&mut self) {
        let Some(cached) = self.items.get(self.selected) else {
            return;
        };
        let item = self
            .model
            .as_ref()
            .and_then(|model| model.items().into_iter().find(|item| item.id == cached.id))
            .unwrap_or_else(|| cached.clone());
        self.editing = Some(item.id.clone());
        self.draft_kind = item.kind;
        self.draft_title.clone_from(&item.title);
        self.draft_username.clone_from(&item.username);
        self.draft_origin = item.origins.iter().cloned().collect::<Vec<_>>().join(", ");
        self.draft_password.clear();
        self.draft_notes = Zeroizing::new(item.notes);
        self.draft_tags = item
            .tags
            .iter()
            .filter(|tag| tag.as_str() != "favorite")
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let mut custom_fields = item
            .fields
            .iter()
            .filter(|(label, _)| label.as_str() != "network")
            .collect::<Vec<_>>();
        custom_fields.sort_by_key(|(left, _)| *left);
        self.draft_custom_fields = Zeroizing::new(
            custom_fields
                .into_iter()
                .map(|(label, value)| format!("{label} = {}", Self::escape_custom_value(value)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.active_field = Some(ActiveField::Name);
        self.status = "Leave password blank to keep it".into();
        self.editor_scroll = 0.0;
        self.mode = ShowcaseMode::Editor;
        self.sync_editor_inputs();
    }

    fn cancel_edit(&mut self) {
        self.mode = ShowcaseMode::Vault;
        self.mobile_detail = self.drawer.is_overlay() && self.editing.is_some();
        self.active_field = None;
        self.clear_sensitive_editor_text();
        self.status.clear();
    }

    fn clear_sensitive_editor_text(&mut self) {
        self.draft_password.clear();
        self.draft_notes.clear();
        self.draft_tags.clear();
        self.draft_custom_fields.clear();
        // The shared inputs retain their own editing buffers. Clear those as
        // well so a saved/cancelled secret cannot survive in an inactive
        // widget and reappear when the editor is opened again.
        self.editor_inputs.password.set_text("");
        self.editor_inputs.notes.set_text("");
        self.editor_inputs.tags.set_text("");
        self.editor_inputs.custom_fields.set_text("");
    }

    #[expect(
        clippy::too_many_lines,
        reason = "saving a draft validates and persists every item kind in one sequential flow"
    )]
    fn save_draft(&mut self) {
        let Some(model) = self.model.as_mut() else {
            self.mode = ShowcaseMode::Vault;
            self.active_field = None;
            self.clear_sensitive_editor_text();
            return;
        };
        if self.draft_title.trim().is_empty() {
            self.status = "Add a name so you can find this item".into();
            self.active_field = Some(ActiveField::Name);
            return;
        }
        if self.draft_kind == SecretItemKind::SecureNote && self.draft_notes.trim().is_empty() {
            self.status = "Add something to the note".into();
            self.active_field = Some(ActiveField::Notes);
            return;
        }
        if self.editing.is_none()
            && self.draft_kind != SecretItemKind::SecureNote
            && self.draft_password.is_empty()
            && self.draft_kind != SecretItemKind::Login
        {
            self.status = format!(
                "Add the {}",
                Self::secret_field_label(self.draft_kind).to_lowercase()
            );
            self.active_field = Some(ActiveField::Password);
            return;
        }
        let existing = self
            .editing
            .as_ref()
            .and_then(|id| model.items().into_iter().find(|item| &item.id == id));
        if self.editing.is_some() && existing.is_none() {
            self.status = "Item no longer exists".into();
            return;
        }
        let password = if self.draft_password.is_empty() {
            existing
                .as_ref()
                .map_or_else(String::new, |item| item.password.clone())
        } else {
            self.draft_password.to_string()
        };
        let password = if password.is_empty() && self.draft_kind == SecretItemKind::Login {
            GeneratedPasswordPolicy::default()
                .generate()
                .map(|value| value.to_string())
                .unwrap_or_default()
        } else {
            password
        };
        let mut item = existing.unwrap_or_else(|| SecretItem::login("", "", ""));
        item.kind = self.draft_kind;
        item.title = self.draft_title.trim().into();
        item.username = self.draft_username.trim().into();
        item.password = if self.draft_kind == SecretItemKind::SecureNote {
            String::new()
        } else {
            password
        };
        item.notes = self.draft_notes.to_string();
        let custom_fields = match Self::parse_custom_fields(&self.draft_custom_fields) {
            Ok(fields) => fields,
            Err(error) => {
                self.status = error;
                self.active_field = Some(ActiveField::CustomFields);
                return;
            }
        };
        let favorite = item.tags.contains("favorite");
        item.tags.clear();
        if favorite {
            item.tags.insert("favorite".into());
        }
        for tag in self
            .draft_tags
            .split([',', '\n'])
            .map(str::trim)
            .filter(|tag| !tag.is_empty() && *tag != "favorite")
        {
            item.tags.insert(tag.to_string());
        }
        item.fields.retain(|label, _| label == "network");
        item.fields.extend(custom_fields);
        item.origins.clear();
        if self.draft_kind == SecretItemKind::Wifi {
            item.fields
                .insert("network".into(), self.draft_username.trim().into());
        } else {
            // `network` is a Passwords-owned projection of the Wi-Fi name;
            // remove it when changing kinds, but retain every app-specific
            // custom field we do not own or understand.
            item.fields.remove("network");
        }
        let mut origins = self
            .draft_origin
            .split([',', '\n', ';'])
            .map(str::trim)
            .filter(|origin| !origin.is_empty());
        let valid_origin = if matches!(
            self.draft_kind,
            SecretItemKind::Login | SecretItemKind::ApiKey
        ) {
            origins.try_for_each(|origin| {
                item.add_origin(origin)
                    .map_err(|_| format!("{origin} is not a full sign-in address"))
            })
        } else {
            Ok(())
        };
        let result = valid_origin.and_then(|()| model.save_item(&mut item).map(|_| ()));
        match result {
            Ok(()) => {
                self.query.clear();
                self.refresh();
                self.mode = ShowcaseMode::Vault;
                self.active_field = None;
                self.clear_sensitive_editor_text();
                self.status = "Saved".into();
            }
            Err(error) => self.status = error,
        }
    }

    fn generate_password(&mut self) {
        match GeneratedPasswordPolicy::default().generate() {
            Ok(value) => {
                self.draft_password = Zeroizing::new(value.to_string());
                self.editor_inputs.password.set_text(&self.draft_password);
                self.status = format!(
                    "Generated {}",
                    Self::secret_field_label(self.draft_kind).to_lowercase()
                );
            }
            Err(error) => self.status = format!("Password generation failed: {error:?}"),
        }
    }

    fn secret_field_label(kind: SecretItemKind) -> &'static str {
        match kind {
            SecretItemKind::ApiKey => "API key",
            SecretItemKind::Wifi | SecretItemKind::Login => "Password",
            SecretItemKind::Other => "Secret",
            SecretItemKind::SecureNote => "Note",
        }
    }

    fn parse_custom_fields(source: &str) -> Result<Vec<(String, String)>, String> {
        let mut fields = Vec::new();
        for (index, line) in source.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((label, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
                return Err(format!(
                    "Custom field {} needs a label and value separated by =",
                    index.saturating_add(1)
                ));
            };
            let label = label.trim();
            if label.is_empty() || label.eq_ignore_ascii_case("network") {
                return Err(format!(
                    "Choose another label for custom field {}",
                    index.saturating_add(1)
                ));
            }
            if fields
                .iter()
                .any(|(existing, _): &(String, String)| existing.eq_ignore_ascii_case(label))
            {
                return Err(format!("Custom field “{label}” appears more than once"));
            }
            fields.push((
                label.to_string(),
                Self::unescape_custom_value(value.trim())?,
            ));
        }
        Ok(fields)
    }

    fn escape_custom_value(value: &str) -> String {
        value
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    }

    fn unescape_custom_value(value: &str) -> Result<String, String> {
        let mut output = String::new();
        let mut chars = value.chars();
        while let Some(character) = chars.next() {
            if character != '\\' {
                output.push(character);
                continue;
            }
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('r') => output.push('\r'),
                Some('\\') => output.push('\\'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => return Err("A custom field value ends with an unfinished escape".into()),
            }
        }
        Ok(output)
    }

    fn editor_field_order(&self) -> Vec<ActiveField> {
        let mut fields = vec![ActiveField::Name];
        match self.draft_kind {
            SecretItemKind::Login | SecretItemKind::ApiKey => fields.extend([
                ActiveField::Username,
                ActiveField::Origin,
                ActiveField::Password,
            ]),
            SecretItemKind::Wifi | SecretItemKind::Other => {
                fields.extend([ActiveField::Username, ActiveField::Password]);
            }
            SecretItemKind::SecureNote => {}
        }
        fields.push(ActiveField::Notes);
        fields.push(ActiveField::Tags);
        fields.push(ActiveField::CustomFields);
        fields
    }

    fn editor_note_y(&self, panel: Rect) -> f64 {
        let ordinary = self
            .editor_field_order()
            .into_iter()
            .filter(|field| {
                !matches!(
                    field,
                    ActiveField::Notes | ActiveField::Tags | ActiveField::CustomFields
                )
            })
            .count();
        self.editor_fields_top(panel)
            + f64::from(u32::try_from(ordinary).unwrap_or(0)) * Self::editor_field_step(panel)
            + if self.draft_kind == SecretItemKind::SecureNote {
                0.0
            } else {
                Self::editor_generate_height(panel)
            }
    }

    fn editor_tags_y(&self, panel: Rect) -> f64 {
        self.editor_note_y(panel)
            + if Self::editor_is_compact(panel) {
                82.0
            } else {
                104.0
            }
    }

    fn editor_custom_fields_y(&self, panel: Rect) -> f64 {
        self.editor_tags_y(panel) + 72.0
    }

    fn editor_is_compact(panel: Rect) -> bool {
        panel.width() - 72.0 < 500.0
    }
    fn editor_fields_top(&self, panel: Rect) -> f64 {
        panel.y0
            + if Self::editor_is_compact(panel) {
                if panel.width() - 72.0 >= 252.0 {
                    132.0
                } else {
                    184.0
                }
            } else {
                148.0
            }
            - self.editor_scroll
    }
    fn editor_field_step(panel: Rect) -> f64 {
        if Self::editor_is_compact(panel) {
            72.0
        } else {
            68.0
        }
    }
    fn editor_generate_height(_panel: Rect) -> f64 {
        52.0
    }

    fn editor_body_viewport(&self, panel: Rect) -> Rect {
        let (_, cancel, _) = self.editor_footer_rects(panel);
        Rect::new(panel.x0, panel.y0 + 58.0, panel.x1, cancel.y0 - 8.0)
    }

    fn max_editor_scroll(&self, panel: Rect) -> f64 {
        let note_bottom = self.editor_custom_fields_y(panel) + self.editor_scroll + 112.0;
        (note_bottom - self.editor_body_viewport(panel).y1).max(0.0)
    }

    fn scroll_editor_by(&mut self, panel: Rect, delta: f64) -> bool {
        let previous = self.editor_scroll;
        self.editor_scroll = (self.editor_scroll + delta).clamp(0.0, self.max_editor_scroll(panel));
        (self.editor_scroll - previous).abs() > f64::EPSILON
    }

    /// Keep a non-pointer focus target inside the clipped editor body.
    fn ensure_editor_field_visible(&mut self, panel: Rect, field: ActiveField) {
        if field == ActiveField::Name {
            // Returning to the first field should also restore the item-kind
            // choices which precede it in the same scrolling body.
            self.editor_scroll = 0.0;
            return;
        }

        let ordinary: Vec<_> = self
            .editor_field_order()
            .into_iter()
            .filter(|candidate| {
                !matches!(
                    candidate,
                    ActiveField::Notes | ActiveField::Tags | ActiveField::CustomFields
                )
            })
            .collect();
        let bounds = if let Some(index) = ordinary.iter().position(|candidate| *candidate == field)
        {
            let top = self.editor_fields_top(panel)
                + f64::from(u32::try_from(index).unwrap_or(0)) * Self::editor_field_step(panel)
                + 16.0;
            (top, top + 52.0)
        } else {
            match field {
                ActiveField::Notes => {
                    let top = self.editor_note_y(panel) + 16.0;
                    (
                        top,
                        top + if Self::editor_is_compact(panel) {
                            58.0
                        } else {
                            72.0
                        },
                    )
                }
                ActiveField::Tags => {
                    let top = self.editor_tags_y(panel) + 16.0;
                    (top, top + 52.0)
                }
                ActiveField::CustomFields => {
                    let top = self.editor_custom_fields_y(panel) + 16.0;
                    (top, top + 88.0)
                }
                ActiveField::Search
                | ActiveField::Name
                | ActiveField::Username
                | ActiveField::Origin
                | ActiveField::Password => return,
            }
        };
        let viewport = self.editor_body_viewport(panel);
        if bounds.0 < viewport.y0 + 4.0 {
            let _ = self.scroll_editor_by(panel, bounds.0 - viewport.y0 - 4.0);
        } else if bounds.1 > viewport.y1 - 4.0 {
            let _ = self.scroll_editor_by(panel, bounds.1 - viewport.y1 + 4.0);
        }
    }

    fn kind_segments(&self, panel: Rect) -> Vec<Rect> {
        let x = panel.x0 + 36.0;
        let width = panel.width() - 72.0;
        if !Self::editor_is_compact(panel) || width >= 252.0 {
            let segment = (width - 32.0) / 5.0;
            (0_u32..5)
                .map(|index| {
                    let left = x + f64::from(index) * (segment + 8.0);
                    let top = if Self::editor_is_compact(panel) {
                        panel.y0 + 70.0
                    } else {
                        panel.y0 + 82.0
                    } - self.editor_scroll;
                    Rect::new(left, top, left + segment, top + 44.0)
                })
                .collect()
        } else {
            let first = (width - 16.0) / 3.0;
            let second = (width - 8.0) / 2.0;
            (0_u32..5)
                .map(|index| {
                    if index < 3 {
                        let left = x + f64::from(index) * (first + 8.0);
                        Rect::new(
                            left,
                            panel.y0 + 70.0 - self.editor_scroll,
                            left + first,
                            panel.y0 + 114.0 - self.editor_scroll,
                        )
                    } else {
                        let left = x + f64::from(index.saturating_sub(3)) * (second + 8.0);
                        Rect::new(
                            left,
                            panel.y0 + 122.0 - self.editor_scroll,
                            left + second,
                            panel.y0 + 166.0 - self.editor_scroll,
                        )
                    }
                })
                .collect()
        }
    }

    fn editor_footer_rects(&self, panel: Rect) -> (Option<Rect>, Rect, Rect) {
        let y = panel.y1 - 68.0;
        if Self::editor_is_compact(panel) {
            let save = Rect::new(panel.x1 - 98.0, y, panel.x1 - 24.0, y + 44.0);
            let cancel = Rect::new(save.x0 - 61.0, y, save.x0 - 6.0, y + 44.0);
            (
                self.editing.is_some().then(|| {
                    Rect::new(
                        panel.x0 + 24.0,
                        y,
                        (panel.x0 + 112.0).min(cancel.x0 - 8.0),
                        y + 44.0,
                    )
                }),
                cancel,
                save,
            )
        } else {
            (
                self.editing
                    .is_some()
                    .then(|| Rect::new(panel.x0 + 36.0, y, panel.x0 + 118.0, y + 44.0)),
                Rect::new(panel.x1 - 238.0, y, panel.x1 - 148.0, y + 44.0),
                Rect::new(panel.x1 - 136.0, y, panel.x1 - 24.0, y + 44.0),
            )
        }
    }

    fn select_kind(&mut self, kind: SecretItemKind) {
        self.draft_kind = kind;
        self.active_field = Some(if kind == SecretItemKind::SecureNote {
            ActiveField::Notes
        } else {
            ActiveField::Name
        });
        self.status.clear();
    }

    fn copy_selected(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let Some(model) = &self.model else { return };
        match model.reveal_value(&item.id) {
            Ok(secret) => {
                if !ui::try_clipboard_copy(&secret) {
                    self.status = "Could not copy to the system clipboard. Try again.".into();
                    return;
                }
                let lease =
                    accounts::ClipboardLease::new(secret, std::time::Duration::from_secs(30));
                let clearing = ui::rt::spawn_thread("passwords-clipboard-clear", move || {
                    let _ = ui::rt::sleep_thread(std::time::Duration::from_secs(30));
                    let current = ui::text_selection::clipboard_paste();
                    if lease.should_clear(current.as_deref()) {
                        let _ = ui::try_clipboard_copy("");
                    }
                });
                let label = Self::secret_field_label(item.kind).to_lowercase();
                self.status = if clearing.is_ok() {
                    format!("Copied {label}; clipboard clears in 30 seconds")
                } else {
                    format!("Copied {label}")
                };
            }
            Err(error) => self.status = error,
        }
    }

    fn reveal_selected(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let Some(model) = &self.model else { return };
        match model.reveal_value(&item.id) {
            Ok(secret) => {
                self.revealed = Some((
                    item.id.clone(),
                    secret,
                    ui::time::now()
                        .checked_add(std::time::Duration::from_secs(10))
                        .unwrap_or_else(ui::time::now),
                ));
            }
            Err(error) => self.status = error,
        }
    }

    fn clear_revealed(&mut self) {
        self.revealed = None;
    }

    fn custody_notice(&self) -> Option<&'static str> {
        self.model.as_ref().map(|model| model.custody().label())
    }

    fn copy_identity_selected(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let full_item = self.model.as_ref().and_then(|model| {
            model
                .items()
                .into_iter()
                .find(|candidate| candidate.id == item.id)
        });
        let value = match item.kind {
            SecretItemKind::Wifi => full_item
                .as_ref()
                .and_then(|candidate| candidate.fields.get("network"))
                .map_or(item.title.as_str(), String::as_str),
            _ => item.username.as_str(),
        };
        if value.is_empty() {
            return;
        }
        if !ui::try_clipboard_copy(value) {
            self.status = "Could not copy to the system clipboard. Try again.".into();
            return;
        }
        self.status = match item.kind {
            SecretItemKind::Wifi => "Copied network name".into(),
            SecretItemKind::ApiKey => "Copied account".into(),
            _ => "Copied username".into(),
        };
    }

    fn copy_origin_selected(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        if item.origins.is_empty() {
            return;
        }
        let origins = item.origins.iter().cloned().collect::<Vec<_>>().join("\n");
        if !ui::try_clipboard_copy(&origins) {
            self.status = "Could not copy to the system clipboard. Try again.".into();
            return;
        }
        self.status = if item.origins.len() == 1 {
            "Copied address".into()
        } else {
            format!("Copied {} addresses", item.origins.len())
        };
    }

    fn activate_detail_row(&mut self, row: usize) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        match item.kind {
            SecretItemKind::Login | SecretItemKind::ApiKey | SecretItemKind::Wifi if row == 0 => {
                self.copy_identity_selected();
            }
            SecretItemKind::Login | SecretItemKind::ApiKey | SecretItemKind::Wifi if row == 1 => {
                self.copy_selected();
            }
            SecretItemKind::Login | SecretItemKind::ApiKey if row == 2 => {
                self.copy_origin_selected();
            }
            SecretItemKind::SecureNote | SecretItemKind::Other if row == 0 => self.copy_selected(),
            _ => {}
        }
    }

    fn detail_row_is_secret(&self, row: usize) -> bool {
        self.items
            .get(self.selected)
            .is_some_and(|item| match item.kind {
                SecretItemKind::Login | SecretItemKind::ApiKey | SecretItemKind::Wifi => row == 1,
                SecretItemKind::SecureNote | SecretItemKind::Other => row == 0,
            })
    }

    fn activate_detail_row_at(&mut self, row: usize, x: f64, panel_right: f64) {
        if self.detail_row_is_secret(row) && x >= panel_right - 72.0 {
            self.reveal_selected();
        } else {
            self.activate_detail_row(row);
        }
    }

    fn route_editor_inputs(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.mode != ShowcaseMode::Editor {
            return false;
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && (matches!(event.logical_key, Key::Named(NamedKey::Tab | NamedKey::Escape))
                        || (event.logical_key == Key::Named(NamedKey::Enter)
                            && self.active_field != Some(ActiveField::Notes)))
        ) {
            return false;
        }

        let point = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => Some(self.last_pointer),
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => Some(Point::new(
                touch.location.x / ctx.scale_factor,
                touch.location.y / ctx.scale_factor,
            )),
            _ => None,
        };
        if let Some(point) = point {
            let mut hit = None;
            self.editor_inputs.for_each_mut(|field, input| {
                if input.get_rect().contains(point) {
                    hit = Some(field);
                }
            });
            if hit.is_some() {
                self.active_field = hit;
            }
        }

        let active = self.active_field;
        self.editor_inputs
            .for_each_mut(|field, input| Widget::set_focus(input, active == Some(field)));
        let mut input_handled = false;
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.editor_inputs.for_each_mut(|_, input| {
                input.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        handled: &mut input_handled,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            });
        } else if let Some(field) = active
            && let Some(input) = self.editor_inputs.get_mut(field)
        {
            input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut input_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }

        let mut updates = Vec::new();
        self.editor_inputs.for_each_mut(|field, input| {
            if let Some(value) = input.take_dirty() {
                updates.push((field, value));
            }
        });
        for (field, value) in updates {
            self.update_draft_from_input(field, value);
        }
        input_handled
    }

    fn request_import(&mut self) {
        if self.model.is_none() || self.import_portal.is_some() {
            return;
        }
        self.status = "Choose a browser password CSV".into();
        self.import_portal = Some(crate::transfer::import_request().send_open_file());
    }

    fn request_export_confirmation(&mut self) {
        if self.model.is_some() {
            self.transfer_dialog = Some(TransferDialog::ExportPlaintext {
                acknowledged: false,
            });
        }
    }

    fn clear_backup_form(&mut self) {
        self.backup_password.set_text("");
        self.backup_confirmation.set_text("");
        self.backup_focus = 0;
    }

    fn request_backup(&mut self) {
        if self.model.is_some() {
            self.clear_backup_form();
            self.transfer_dialog = Some(TransferDialog::Backup);
            self.backup_password.set_focus(true);
        }
    }

    fn request_restore(&mut self) {
        if self.model.is_some() && self.restore_portal.is_none() {
            self.status = "Choose an encrypted Passwords backup".into();
            self.restore_portal = Some(crate::transfer::restore_request().send_open_file());
        }
    }

    fn backup_form_error(&self, confirmation: bool) -> Option<&'static str> {
        if self.backup_password.value().chars().count() < 12 {
            Some("Use at least 12 characters")
        } else if confirmation && self.backup_password.value() != self.backup_confirmation.value() {
            Some("Passwords do not match")
        } else {
            None
        }
    }

    fn begin_encrypted_backup(&mut self) {
        if !matches!(self.transfer_dialog, Some(TransferDialog::Backup))
            || self.backup_form_error(true).is_some()
            || self.backup_portal.is_some()
        {
            return;
        }
        self.backup_portal = Some(crate::transfer::backup_request().send());
    }

    fn apply_encrypted_restore(&mut self) {
        let Some(TransferDialog::Restore { bytes }) = self.transfer_dialog.take() else {
            return;
        };
        let passphrase = Zeroizing::new(self.backup_password.value().to_owned());
        let result = crate::transfer::import_encrypted_backup(&bytes, &passphrase);
        match result {
            Ok(items) => {
                let count = items.len();
                let Some(model) = &mut self.model else { return };
                let mut restored = 0_usize;
                for mut item in items {
                    if model.update(&mut item).is_ok() {
                        restored = restored.saturating_add(1);
                    }
                }
                self.refresh();
                self.status = if restored == count {
                    format!("Restored {restored} vault items")
                } else {
                    format!("Restored {restored} of {count} vault items")
                };
                self.clear_backup_form();
            }
            Err(error) => {
                self.transfer_dialog = Some(TransferDialog::Restore { bytes });
                self.status = error;
            }
        }
    }

    fn begin_plaintext_export(&mut self) {
        let acknowledged = matches!(
            self.transfer_dialog,
            Some(TransferDialog::ExportPlaintext { acknowledged: true })
        );
        if !acknowledged || self.export_portal.is_some() {
            return;
        }
        self.transfer_dialog = None;
        self.export_portal = Some(crate::transfer::export_request().send());
    }

    #[expect(
        clippy::too_many_lines,
        reason = "drains each transfer channel in turn; keeping the states together keeps the vault update ordering visible"
    )]
    fn poll_transfers(&mut self) {
        if let Some(rx) = self.restore_portal.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::OpenFileOutcome::Opened(opened)) => {
                    self.restore_portal = None;
                    match crate::transfer::read_encrypted_backup(opened.into_file()) {
                        Ok(bytes) => {
                            self.clear_backup_form();
                            self.transfer_dialog = Some(TransferDialog::Restore { bytes });
                            self.backup_password.set_focus(true);
                            self.status.clear();
                        }
                        Err(error) => self.status = error,
                    }
                }
                Ok(ui::portal::OpenFileOutcome::Cancelled) => {
                    self.restore_portal = None;
                    self.status.clear();
                }
                Ok(ui::portal::OpenFileOutcome::Failed(error)) => {
                    self.restore_portal = None;
                    self.status = format!("Could not open the file chooser: {error}");
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.restore_portal = None;
                    self.status = "The file chooser disconnected".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = self.import_portal.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::OpenFileOutcome::Opened(opened)) => {
                    self.import_portal = None;
                    let file = opened.into_file();
                    let (tx, rx) = std::sync::mpsc::channel();
                    let spawned = ui::rt::spawn_thread("passwords-csv-parser", move || {
                        let _ = tx.send(crate::transfer::parse_browser_csv(file));
                    });
                    match spawned {
                        Ok(_) => self.import_parse = Some(rx),
                        Err(error) => {
                            self.status = format!("Could not start the CSV parser: {error}");
                        }
                    }
                }
                Ok(ui::portal::OpenFileOutcome::Cancelled) => {
                    self.import_portal = None;
                    self.status.clear();
                }
                Ok(ui::portal::OpenFileOutcome::Failed(error)) => {
                    self.import_portal = None;
                    self.status = format!("Could not open the file chooser: {error}");
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.import_portal = None;
                    self.status = "The file chooser disconnected".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = self.import_parse.as_ref() {
            match rx.try_recv() {
                Ok(Ok(rows)) => {
                    self.import_parse = None;
                    let conflicts = self.import_conflicts(&rows);
                    self.transfer_dialog = Some(TransferDialog::ImportPreview { rows, conflicts });
                }
                Ok(Err(error)) => {
                    self.import_parse = None;
                    self.status = error;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.import_parse = None;
                    self.status = "The CSV parser stopped".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = self.export_portal.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(mut path)) => {
                    self.export_portal = None;
                    if path
                        .extension()
                        .and_then(|v| v.to_str())
                        .is_none_or(|ext| !ext.eq_ignore_ascii_case("csv"))
                    {
                        path.set_extension("csv");
                    }
                    let result = self.write_plaintext_export(&path);
                    self.status = result
                        .map_or_else(|error| error, |count| format!("Exported {count} logins"));
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => {
                    self.export_portal = None;
                    self.status.clear();
                }
                Ok(ui::portal::PortalOutcome::Failed(error)) => {
                    self.export_portal = None;
                    self.status = format!("Could not open the save dialog: {error}");
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.export_portal = None;
                    self.status = "The save dialog disconnected".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = self.backup_portal.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(mut path)) => {
                    self.backup_portal = None;
                    if path
                        .extension()
                        .and_then(|v| v.to_str())
                        .is_none_or(|ext| !ext.eq_ignore_ascii_case("tilopw"))
                    {
                        path.set_extension("tilopw");
                    }
                    let passphrase = Zeroizing::new(self.backup_password.value().to_owned());
                    let result = self.write_encrypted_backup(&path, &passphrase);
                    self.status = result.map_or_else(
                        |error| error,
                        |count| format!("Backed up {count} vault items"),
                    );
                    self.clear_backup_form();
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => self.backup_portal = None,
                Ok(ui::portal::PortalOutcome::Failed(error)) => {
                    self.backup_portal = None;
                    self.status = format!("Could not open the save dialog: {error}");
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.backup_portal = None;
                    self.status = "The save dialog disconnected".into();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    fn import_conflicts(&self, rows: &[ImportLogin]) -> usize {
        let existing = self
            .model
            .as_ref()
            .map_or_else(Vec::new, crate::PasswordsModel::items);
        rows.iter()
            .filter(|row| {
                existing.iter().any(|item| {
                    item.title.eq_ignore_ascii_case(&row.title)
                        && item.username.eq_ignore_ascii_case(&row.username)
                        && origins_match(item, &row.origin)
                })
            })
            .count()
    }

    fn apply_import(&mut self) {
        let Some(TransferDialog::ImportPreview { rows, .. }) = self.transfer_dialog.take() else {
            return;
        };
        let existing = self
            .model
            .as_ref()
            .map_or_else(Vec::new, crate::PasswordsModel::items);
        let Some(model) = &mut self.model else { return };
        let mut imported = 0usize;
        let mut skipped = 0usize;
        for row in rows {
            if existing.iter().any(|item| {
                item.title.eq_ignore_ascii_case(&row.title)
                    && item.username.eq_ignore_ascii_case(&row.username)
                    && origins_match(item, &row.origin)
            }) {
                skipped = skipped.saturating_add(1);
                continue;
            }
            let mut item = SecretItem::login(
                row.title.to_string(),
                row.username.to_string(),
                row.password.to_string(),
            );
            item.notes = row.notes.to_string();
            if !row.origin.is_empty() && item.add_origin(&row.origin).is_err() {
                skipped = skipped.saturating_add(1);
                continue;
            }
            if model.update(&mut item).is_ok() {
                imported = imported.saturating_add(1);
            } else {
                skipped = skipped.saturating_add(1);
            }
        }
        self.refresh();
        self.status = format!("Imported {imported}; skipped {skipped}");
    }

    fn write_plaintext_export(&self, path: &std::path::Path) -> Result<usize, String> {
        let Some(model) = &self.model else {
            return Err("Export is unavailable in this preview".into());
        };
        let items = model.items();
        let csv = crate::transfer::export_browser_csv(&items);
        tilo_files::native::atomic::write_with_mode(path, csv.as_bytes(), 0o600)
            .map_err(|_| "Could not finish writing the selected file".to_string())?;
        Ok(items.len())
    }

    fn write_encrypted_backup(
        &self,
        path: &std::path::Path,
        passphrase: &str,
    ) -> Result<usize, String> {
        let Some(model) = &self.model else {
            return Err("Backup is unavailable in this preview".into());
        };
        let items = model.items();
        let backup = crate::transfer::export_encrypted_backup(&items, passphrase)?;
        tilo_files::native::atomic::write_with_mode(path, &backup, 0o600)
            .map_err(|_| "Could not finish writing the selected backup".to_string())?;
        Ok(items.len())
    }

    fn activate_sidebar(&mut self, doc: &mut AutoCommit, point: Point) -> bool {
        if !self.drawer.sidebar_contains(point) {
            return false;
        }
        let rect = self.sidebar_viewport();
        let Some(id) = self.sidebar_nav(rect).hit_test(point).map(str::to_owned) else {
            return true;
        };
        match id.as_str() {
            "all" => {
                self.select_filter(doc, VaultFilter::All);
            }
            "favorites" => self.select_filter(doc, VaultFilter::Favorites),
            "logins" => self.select_filter(doc, VaultFilter::Kind(SecretItemKind::Login)),
            "api-keys" => self.select_filter(doc, VaultFilter::Kind(SecretItemKind::ApiKey)),
            "notes" => self.select_filter(doc, VaultFilter::Kind(SecretItemKind::SecureNote)),
            "wifi" => self.select_filter(doc, VaultFilter::Kind(SecretItemKind::Wifi)),
            "other" => self.select_filter(doc, VaultFilter::Kind(SecretItemKind::Other)),
            "review" => {
                self.mode = ShowcaseMode::Health;
                self.mobile_detail = false;
            }
            "app-access" => {
                self.mode = ShowcaseMode::AppAccess;
                self.mobile_detail = false;
            }
            "import" => self.request_import(),
            "export" => self.request_export_confirmation(),
            "backup" => self.request_backup(),
            "restore" => self.request_restore(),
            _ => return true,
        }
        if self.drawer.is_overlay() {
            self.drawer.close();
        }
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "hit-testing for the transfer dialog follows the drawn layout top-to-bottom"
    )]
    fn activate_transfer(&mut self, point: Point) -> bool {
        let rect = self.transfer_rect();
        let Some(dialog) = &mut self.transfer_dialog else {
            return false;
        };
        if !rect.contains(point) {
            return true;
        }
        match dialog {
            TransferDialog::ImportPreview { .. } => {
                if Rect::new(
                    rect.x1 - 220.0,
                    rect.y1 - 64.0,
                    rect.x1 - 126.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                {
                    self.transfer_dialog = None;
                } else if Rect::new(
                    rect.x1 - 124.0,
                    rect.y1 - 64.0,
                    rect.x1 - 12.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                {
                    self.apply_import();
                }
            }
            TransferDialog::ExportPlaintext { acknowledged } => {
                if Rect::new(
                    rect.x0 + 20.0,
                    rect.y0 + 140.0,
                    rect.x1 - 20.0,
                    rect.y0 + 216.0,
                )
                .contains(point)
                {
                    *acknowledged = !*acknowledged;
                } else if Rect::new(
                    rect.x1 - 236.0,
                    rect.y1 - 64.0,
                    rect.x1 - 142.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                {
                    self.transfer_dialog = None;
                } else if *acknowledged
                    && Rect::new(
                        rect.x1 - 140.0,
                        rect.y1 - 64.0,
                        rect.x1 - 12.0,
                        rect.y1 - 12.0,
                    )
                    .contains(point)
                {
                    self.begin_plaintext_export();
                }
            }
            TransferDialog::DeleteConfirm { .. } => {
                if Rect::new(
                    rect.x1 - 236.0,
                    rect.y1 - 64.0,
                    rect.x1 - 142.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                {
                    self.transfer_dialog = None;
                } else if Rect::new(
                    rect.x1 - 140.0,
                    rect.y1 - 64.0,
                    rect.x1 - 12.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                {
                    self.confirm_delete();
                }
            }
            TransferDialog::Backup | TransferDialog::Restore { .. } => {
                let restoring = matches!(dialog, TransferDialog::Restore { .. });
                let x = rect.x0 + 28.0;
                let first = Rect::new(x, rect.y0 + 106.0, rect.x1 - 28.0, rect.y0 + 154.0);
                let second = Rect::new(x, rect.y0 + 170.0, rect.x1 - 28.0, rect.y0 + 218.0);
                if first.contains(point) {
                    self.backup_focus = 0;
                    self.backup_password.set_focus(true);
                    self.backup_confirmation.set_focus(false);
                } else if !restoring && second.contains(point) {
                    self.backup_focus = 1;
                    self.backup_password.set_focus(false);
                    self.backup_confirmation.set_focus(true);
                } else if Rect::new(
                    rect.x1 - 236.0,
                    rect.y1 - 64.0,
                    rect.x1 - 142.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                {
                    self.transfer_dialog = None;
                    self.clear_backup_form();
                } else if Rect::new(
                    rect.x1 - 140.0,
                    rect.y1 - 64.0,
                    rect.x1 - 12.0,
                    rect.y1 - 12.0,
                )
                .contains(point)
                    && self.backup_form_error(!restoring).is_none()
                {
                    if restoring {
                        self.apply_encrypted_restore();
                    } else {
                        self.begin_encrypted_backup();
                    }
                }
            }
        }
        true
    }

    /// Touch does not synthesize a mouse click on every compositor. Route the
    /// primary controls explicitly, using the same generous visual regions.
    #[expect(
        clippy::too_many_lines,
        reason = "touch hit-testing mirrors every drawn control in paint order"
    )]
    fn activate_touch(&mut self, doc: &mut AutoCommit, point: Point) -> bool {
        if self.transfer_dialog.is_some() {
            return self.activate_transfer(point);
        }
        let content = Rect::new(
            self.rect.x0 + 24.0,
            self.rect.y0 + 24.0,
            self.rect.x1 - 24.0,
            self.rect.y1 - 24.0,
        );
        self.resolve_drawer(content);
        if self.drawer.is_overlay()
            && Rect::new(content.x0, content.y0, content.x0 + 48.0, content.y0 + 48.0)
                .contains(point)
        {
            self.drawer.toggle();
            return true;
        }
        if self.activate_sidebar(doc, point) {
            return true;
        }
        if self.drawer.press_dismisses(point) {
            return true;
        }
        let main = self.drawer.content_rect();
        let list = if self.drawer.is_docked() {
            Rect::new(main.x0, content.y0 + 4.0, main.x0 + 330.0, content.y1)
        } else if self.mode == ShowcaseMode::Editor {
            main
        } else {
            Rect::new(main.x0 + 60.0, main.y0, main.x1, main.y1)
        };
        if self.drawer.is_overlay() && self.mode == ShowcaseMode::Vault && self.mobile_detail {
            if Rect::new(list.x0, list.y0 + 18.0, list.x0 + 62.0, list.y0 + 76.0).contains(point) {
                self.mobile_detail = false;
                return true;
            }
            if Rect::new(
                list.x1 - 110.0,
                list.y0 + 20.0,
                list.x1 - 10.0,
                list.y0 + 84.0,
            )
            .contains(point)
            {
                self.begin_edit();
                return true;
            }
            if point.x >= list.x1 - 150.0 {
                for row in 0_u32..3 {
                    let y0 = list.y0 + 116.0 + f64::from(row) * 78.0;
                    if (y0..y0 + 78.0).contains(&point.y) {
                        self.activate_detail_row_at(row as usize, point.x, list.x1);
                        return true;
                    }
                }
            }
            return false;
        }
        if self.transfer_dialog.is_none()
            && (self.mode == ShowcaseMode::Vault || self.drawer.is_docked())
        {
            if Rect::new(
                list.x1 - 122.0,
                list.y0 - 10.0,
                list.x1 + 4.0,
                list.y0 + 46.0,
            )
            .contains(point)
            {
                self.begin_add();
                return true;
            }
            if Rect::new(list.x0, list.y0 + 48.0, list.x1, list.y0 + 106.0).contains(point) {
                self.active_field = Some(ActiveField::Search);
                return true;
            }
            let first_y = list.y0 + 168.0;
            let visible = self.visible_indices();
            let scroll = self.list_scroll.min(self.max_list_scroll(list));
            for (display_index, item_index) in visible
                .into_iter()
                .skip(scroll)
                .take(Self::list_capacity(list))
                .enumerate()
            {
                let y = first_y + f64::from(u32::try_from(display_index).unwrap_or(0)) * 76.0;
                if Rect::new(list.x0, y, list.x1, y + 68.0).contains(point) {
                    if Rect::new(list.x1 - 52.0, y, list.x1, y + 68.0).contains(point) {
                        self.toggle_favorite(item_index);
                        return true;
                    }
                    self.select_index(doc, item_index);
                    if self.drawer.is_overlay() {
                        self.mobile_detail = true;
                    }
                    return true;
                }
            }
        }
        let panel = if self.drawer.is_docked() {
            Rect::new(list.x1 + 24.0, content.y0, content.x1, content.y1)
        } else {
            list
        };
        if self.mode == ShowcaseMode::AppAccess && self.activate_app_access(panel, point) {
            return true;
        }
        if self.mode == ShowcaseMode::Vault && self.drawer.is_docked() {
            if Rect::new(
                panel.x1 - 110.0,
                panel.y0 + 20.0,
                panel.x1 - 10.0,
                panel.y0 + 84.0,
            )
            .contains(point)
            {
                self.begin_edit();
                return true;
            }
            if point.x >= panel.x1 - 150.0
                && (panel.y0 + 116.0..panel.y0 + 202.0).contains(&point.y)
            {
                self.activate_detail_row_at(0, point.x, panel.x1);
                return true;
            }
            if point.x >= panel.x1 - 150.0
                && (panel.y0 + 194.0..panel.y0 + 280.0).contains(&point.y)
            {
                self.activate_detail_row_at(1, point.x, panel.x1);
                return true;
            }
            if point.x >= panel.x1 - 150.0
                && (panel.y0 + 272.0..panel.y0 + 358.0).contains(&point.y)
            {
                self.activate_detail_row_at(2, point.x, panel.x1);
                return true;
            }
        }
        if self.mode == ShowcaseMode::Editor {
            let x = panel.x0 + 36.0;
            for (segment, kind) in self.kind_segments(panel).into_iter().zip([
                SecretItemKind::Login,
                SecretItemKind::ApiKey,
                SecretItemKind::SecureNote,
                SecretItemKind::Wifi,
                SecretItemKind::Other,
            ]) {
                if segment.contains(point) {
                    self.select_kind(kind);
                    return true;
                }
            }
            let ordinary: Vec<_> = self
                .editor_field_order()
                .into_iter()
                .filter(|field| {
                    !matches!(
                        field,
                        ActiveField::Notes | ActiveField::Tags | ActiveField::CustomFields
                    )
                })
                .collect();
            for (index, field) in ordinary.into_iter().enumerate() {
                let y = self.editor_fields_top(panel)
                    + 20.0
                    + f64::from(u32::try_from(index).unwrap_or(0)) * Self::editor_field_step(panel);
                if Rect::new(x, y, panel.x1 - 36.0, y + 48.0).contains(point) {
                    self.active_field = Some(field);
                    return true;
                }
            }
            let note_y = self.editor_note_y(panel);
            if self.draft_kind != SecretItemKind::SecureNote
                && Rect::new(x, note_y - 52.0, x + 206.0, note_y - 4.0).contains(point)
            {
                self.generate_password();
                return true;
            }
            if Rect::new(x, note_y + 16.0, panel.x1 - 36.0, note_y + 90.0).contains(point) {
                self.active_field = Some(ActiveField::Notes);
                return true;
            }
            let tags_y = self.editor_tags_y(panel);
            if Rect::new(x, tags_y + 16.0, panel.x1 - 36.0, tags_y + 68.0).contains(point) {
                self.active_field = Some(ActiveField::Tags);
                return true;
            }
            let custom_y = self.editor_custom_fields_y(panel);
            if Rect::new(x, custom_y + 16.0, panel.x1 - 36.0, custom_y + 104.0).contains(point) {
                self.active_field = Some(ActiveField::CustomFields);
                return true;
            }
            let (delete, cancel, save) = self.editor_footer_rects(panel);
            if delete.is_some_and(|rect| rect.contains(point)) {
                if let Some(id) = self.editing.clone() {
                    self.request_delete(id);
                }
                return true;
            }
            if cancel.contains(point) {
                self.cancel_edit();
                return true;
            }
            if save.contains(point) {
                self.save_draft();
                return true;
            }
        }
        false
    }

    fn card(
        scene: &mut Scene,
        transform: Affine,
        color: vello::peniko::Color,
        rect: Rect,
        radius: f64,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            color,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
    }
    fn pill(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, active: bool) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        Self::card(
            ctx.scene,
            ctx.transform,
            if active {
                c.accent.stops.get(5).copied().unwrap_or(c.text_primary)
            } else {
                c.surfaces.raised
            },
            rect,
            17.0,
        );
        ctx.text(
            label,
            &TextStyle::new(12.0).with_weight(TextWeight::Medium),
            if active {
                c.surfaces.base
            } else {
                c.text_primary
            },
            Point::new(rect.x0 + 14.0, rect.y0 + 9.0),
            None,
        );
    }

    fn sidebar_nav(&self, rect: Rect) -> SidebarNav {
        let count = |kind| self.items.iter().filter(|item| item.kind == kind).count();
        let favorites = self
            .items
            .iter()
            .filter(|item| Self::is_favorite(item))
            .count();
        SidebarNav::new(
            rect,
            vec![
                SidebarNavSection::new(
                    "VAULT",
                    vec![
                        SidebarNavItem::new("all", "All items")
                            .badge(self.items.len().to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter == VaultFilter::All,
                            ),
                        SidebarNavItem::new("favorites", "Favorites")
                            .badge(favorites.to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter == VaultFilter::Favorites,
                            ),
                        SidebarNavItem::new("logins", "Logins")
                            .badge(count(SecretItemKind::Login).to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter
                                        == VaultFilter::Kind(SecretItemKind::Login),
                            ),
                        SidebarNavItem::new("api-keys", "API keys")
                            .badge(count(SecretItemKind::ApiKey).to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter
                                        == VaultFilter::Kind(SecretItemKind::ApiKey),
                            ),
                        SidebarNavItem::new("notes", "Secure notes")
                            .badge(count(SecretItemKind::SecureNote).to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter
                                        == VaultFilter::Kind(SecretItemKind::SecureNote),
                            ),
                        SidebarNavItem::new("wifi", "Wi-Fi")
                            .badge(count(SecretItemKind::Wifi).to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter == VaultFilter::Kind(SecretItemKind::Wifi),
                            ),
                        SidebarNavItem::new("other", "Other secrets")
                            .badge(count(SecretItemKind::Other).to_string())
                            .selected(
                                self.mode == ShowcaseMode::Vault
                                    && self.vault_filter
                                        == VaultFilter::Kind(SecretItemKind::Other),
                            ),
                    ],
                ),
                SidebarNavSection::new(
                    "TOOLS",
                    vec![
                        SidebarNavItem::new("review", "Password review")
                            .selected(self.mode == ShowcaseMode::Health),
                        SidebarNavItem::new("app-access", "App access")
                            .badge(self.app_grants.len().to_string())
                            .selected(self.mode == ShowcaseMode::AppAccess),
                        SidebarNavItem::new("backup", "Back up vault…"),
                        SidebarNavItem::new("restore", "Restore backup…"),
                        SidebarNavItem::new("import", "Import passwords"),
                        SidebarNavItem::new("export", "Export plaintext CSV…"),
                    ],
                ),
            ],
        )
        .with_scroll_offset(self.sidebar_scroll)
    }

    fn sidebar_viewport(&self) -> Rect {
        let side = self.drawer.sidebar_rect();
        Rect::new(side.x0, side.y0, side.x1 - 18.0, side.y1)
    }

    fn scroll_sidebar_by(&mut self, delta: f64) -> bool {
        let nav = self.sidebar_nav(self.sidebar_viewport());
        let before = self.sidebar_scroll.min(nav.max_scroll_offset());
        self.sidebar_scroll = (before + delta).clamp(0.0, nav.max_scroll_offset());
        self.sidebar_scroll.to_bits() != before.to_bits()
    }

    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        self.sidebar_nav(rect).render(scene, transform, tr, theme);
    }

    fn transfer_rect(&self) -> Rect {
        let width = (self.rect.width() - 40.0).min(560.0);
        let height = 330.0_f64.min(self.rect.height() - 40.0);
        Rect::new(
            self.rect.center().x - width / 2.0,
            self.rect.center().y - height / 2.0,
            self.rect.center().x + width / 2.0,
            self.rect.center().y + height / 2.0,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the transfer dialog that reads top-to-bottom"
    )]
    fn draw_transfer_dialog(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let Some(dialog) = &self.transfer_dialog else {
            return;
        };
        let c = &theme.tokens.colors;
        ctx.scene
            .fill(Fill::NonZero, transform, c.surfaces.scrim, None, &self.rect);
        let rect = self.transfer_rect();
        Self::card(ctx.scene, transform, c.surfaces.overlay, rect, 18.0);
        let x = rect.x0 + 28.0;
        let compact = rect.width() < 420.0;
        match dialog {
            TransferDialog::ImportPreview { rows, conflicts } => {
                ctx.text(
                    "Review import",
                    &TextStyle::new(24.0).with_weight(TextWeight::Bold),
                    c.text_primary,
                    Point::new(x, rect.y0 + 28.0),
                    None,
                );
                ctx.text(
                    &format!("{} logins found", rows.len()),
                    &TextStyle::new(14.0).with_weight(TextWeight::Medium),
                    c.text_primary,
                    Point::new(x, rect.y0 + 72.0),
                    None,
                );
                ctx.text(
                    &format!("{conflicts} matching logins will be skipped"),
                    &TextStyle::new(13.0),
                    c.text_dim,
                    Point::new(x, rect.y0 + 100.0),
                    None,
                );
                for (index, row) in rows.iter().take(3).enumerate() {
                    let step = if compact { 44.0 } else { 34.0 };
                    let y = rect.y0 + 138.0 + f64::from(u32::try_from(index).unwrap_or(0)) * step;
                    ctx.text(
                        &row.title,
                        &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                        c.text_primary,
                        Point::new(x, y),
                        None,
                    );
                    ctx.text(
                        &row.username,
                        &TextStyle::new(12.0),
                        c.text_dim,
                        Point::new(
                            if compact { x } else { x + 190.0 },
                            if compact { y + 20.0 } else { y },
                        ),
                        None,
                    );
                }
                let cancel = ui::widget::ButtonFace::new(
                    Rect::new(
                        rect.x1 - 220.0,
                        rect.y1 - 64.0,
                        rect.x1 - 126.0,
                        rect.y1 - 20.0,
                    ),
                    "Cancel",
                );
                ui::widget::Button::paint(ctx.scene, transform, ctx.text, theme, &cancel);
                let import = ui::widget::ButtonFace::new(
                    Rect::new(
                        rect.x1 - 116.0,
                        rect.y1 - 64.0,
                        rect.x1 - 20.0,
                        rect.y1 - 20.0,
                    ),
                    "Import",
                )
                .style(ui::widget::ButtonStyle::Primary);
                ui::widget::Button::paint(ctx.scene, transform, ctx.text, theme, &import);
            }
            TransferDialog::ExportPlaintext { acknowledged } => {
                ctx.text(
                    "Export as plaintext CSV",
                    &TextStyle::new(24.0).with_weight(TextWeight::Bold),
                    c.text_primary,
                    Point::new(x, rect.y0 + 28.0),
                    None,
                );
                let body_lines: &[(&str, f64, vello::peniko::Color)] = if compact {
                    &[
                        ("The exported file is readable by", 76.0, c.text_primary),
                        ("anyone who can open it.", 98.0, c.text_primary),
                        ("Use it only to move passwords,", 128.0, c.text_dim),
                        ("then delete it.", 150.0, c.text_dim),
                    ]
                } else {
                    &[
                        (
                            "The exported file can be read by anyone who can open it.",
                            78.0,
                            c.text_primary,
                        ),
                        (
                            "Use it only to move passwords, then delete it when finished.",
                            108.0,
                            c.text_dim,
                        ),
                    ]
                };
                for (line, y, color) in body_lines {
                    ctx.text(
                        line,
                        &TextStyle::new(if *color == c.text_primary { 14.0 } else { 13.0 }),
                        *color,
                        Point::new(x, rect.y0 + y),
                        None,
                    );
                }
                let check_y = if compact { 182.0 } else { 152.0 };
                let check = Rect::new(
                    x,
                    rect.y0 + check_y,
                    rect.x1 - 28.0,
                    rect.y0 + check_y + 52.0,
                );
                Self::card(ctx.scene, transform, c.surfaces.base, check, 10.0);
                let check_icon = if *acknowledged {
                    Icon::Check
                } else {
                    Icon::Square
                };
                check_icon.draw(
                    ctx.scene,
                    transform,
                    Rect::new(x + 13.0, check.y0 + 17.0, x + 31.0, check.y0 + 35.0),
                    c.text_primary,
                );
                ctx.text(
                    "I understand this file is plaintext",
                    &TextStyle::new(13.0).with_weight(TextWeight::Medium),
                    c.text_primary,
                    Point::new(x + 48.0, check.y0 + 17.0),
                    None,
                );
                let cancel = ui::widget::ButtonFace::new(
                    Rect::new(
                        rect.x1 - 236.0,
                        rect.y1 - 64.0,
                        rect.x1 - 142.0,
                        rect.y1 - 20.0,
                    ),
                    "Cancel",
                );
                ui::widget::Button::paint(ctx.scene, transform, ctx.text, theme, &cancel);
                let export = ui::widget::ButtonFace::new(
                    Rect::new(
                        rect.x1 - 132.0,
                        rect.y1 - 64.0,
                        rect.x1 - 20.0,
                        rect.y1 - 20.0,
                    ),
                    "Choose file",
                )
                .style(ui::widget::ButtonStyle::Primary)
                .disabled(!acknowledged);
                ui::widget::Button::paint(ctx.scene, transform, ctx.text, theme, &export);
            }
            TransferDialog::Backup | TransferDialog::Restore { .. } => {
                let restoring = matches!(dialog, TransferDialog::Restore { .. });
                ctx.text(
                    if restoring {
                        "Unlock backup"
                    } else {
                        "Back up your vault"
                    },
                    &TextStyle::new(24.0).with_weight(TextWeight::Bold),
                    c.text_primary,
                    Point::new(x, rect.y0 + 28.0),
                    None,
                );
                let description: &[(&str, f64)] = if compact && restoring {
                    &[
                        ("Enter the password used when this", 70.0),
                        ("backup was created.", 90.0),
                    ]
                } else if compact {
                    &[
                        ("Encrypted locally. Preserves every item", 70.0),
                        ("and custom field.", 90.0),
                    ]
                } else if restoring {
                    &[(
                        "Enter the password used when this backup was created.",
                        70.0,
                    )]
                } else {
                    &[(
                        "Encrypted locally. Preserves every item and custom field.",
                        70.0,
                    )]
                };
                for (line, y) in description {
                    ctx.text(
                        line,
                        &TextStyle::new(13.0),
                        c.text_dim,
                        Point::new(x, rect.y0 + y),
                        None,
                    );
                }
                let first = Rect::new(x, rect.y0 + 106.0, rect.x1 - 28.0, rect.y0 + 154.0);
                self.backup_password.apply_layout_external(first);
                Widget::draw(
                    &mut self.backup_password,
                    ctx.scene,
                    transform,
                    ctx.text,
                    theme,
                    presence,
                );
                if !restoring {
                    let second = Rect::new(x, rect.y0 + 170.0, rect.x1 - 28.0, rect.y0 + 218.0);
                    self.backup_confirmation.apply_layout_external(second);
                    Widget::draw(
                        &mut self.backup_confirmation,
                        ctx.scene,
                        transform,
                        ctx.text,
                        theme,
                        presence,
                    );
                }
                if let Some(error) = self.backup_form_error(!restoring) {
                    ctx.text(
                        error,
                        &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                        c.semantic.danger,
                        Point::new(x, rect.y0 + if restoring { 166.0 } else { 226.0 }),
                        None,
                    );
                }
                let cancel = ButtonFace::new(
                    Rect::new(
                        rect.x1 - 236.0,
                        rect.y1 - 64.0,
                        rect.x1 - 142.0,
                        rect.y1 - 20.0,
                    ),
                    "Cancel",
                );
                Button::paint(ctx.scene, transform, ctx.text, theme, &cancel);
                let action = ButtonFace::new(
                    Rect::new(
                        rect.x1 - 132.0,
                        rect.y1 - 64.0,
                        rect.x1 - 20.0,
                        rect.y1 - 20.0,
                    ),
                    if restoring { "Restore" } else { "Choose file" },
                )
                .style(ui::widget::ButtonStyle::Primary)
                .disabled(self.backup_form_error(!restoring).is_some());
                Button::paint(ctx.scene, transform, ctx.text, theme, &action);
            }
            TransferDialog::DeleteConfirm { title, .. } => {
                ctx.text(
                    "Delete item?",
                    &TextStyle::new(24.0).with_weight(TextWeight::Bold),
                    c.text_primary,
                    Point::new(x, rect.y0 + 28.0),
                    None,
                );
                ctx.text(
                    title,
                    &TextStyle::new(15.0).with_weight(TextWeight::Medium),
                    c.text_primary,
                    Point::new(x, rect.y0 + 82.0),
                    None,
                );
                let body: &[(&str, f64)] = if compact {
                    &[
                        ("This item will be permanently removed", 116.0),
                        ("from your vault.", 138.0),
                    ]
                } else {
                    &[(
                        "This item will be permanently removed from your vault.",
                        116.0,
                    )]
                };
                for (line, y) in body {
                    ctx.text(
                        line,
                        &TextStyle::new(13.0),
                        c.text_dim,
                        Point::new(x, rect.y0 + y),
                        None,
                    );
                }
                let cancel = ButtonFace::new(
                    Rect::new(
                        rect.x1 - 236.0,
                        rect.y1 - 64.0,
                        rect.x1 - 142.0,
                        rect.y1 - 20.0,
                    ),
                    "Keep item",
                );
                Button::paint(ctx.scene, transform, ctx.text, theme, &cancel);
                let delete = ButtonFace::new(
                    Rect::new(
                        rect.x1 - 132.0,
                        rect.y1 - 64.0,
                        rect.x1 - 20.0,
                        rect.y1 - 20.0,
                    ),
                    "Delete",
                )
                .style(ui::widget::ButtonStyle::Danger);
                Button::paint(ctx.scene, transform, ctx.text, theme, &delete);
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the item list that reads top-to-bottom"
    )]
    fn draw_list(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        compact: bool,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let c = &theme.tokens.colors;
        ctx.text(
            "Passwords",
            &TextStyle::new(if compact { 25.0 } else { 28.0 }).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(rect.x0, rect.y0),
            None,
        );
        Button::paint(
            ctx.scene,
            transform,
            ctx.text,
            theme,
            &ButtonFace::new(
                Rect::new(rect.x1 - 112.0, rect.y0 - 4.0, rect.x1, rect.y0 + 40.0),
                "New item",
            )
            .icon(Icon::Plus),
        );
        let search = Rect::new(rect.x0, rect.y0 + 54.0, rect.x1, rect.y0 + 98.0);
        self.search_input.apply_layout_external(search);
        self.search_input.draw(
            ctx.scene,
            transform,
            ctx.text,
            theme,
            &PresenceCtx::default(),
        );
        let tabs_y = rect.y0 + 116.0;
        let visible = self.visible_indices();
        let count_label = self.custody_notice().map_or_else(
            || format!("{} items", visible.len()),
            |notice| format!("{} items · {notice}", visible.len()),
        );
        ctx.text(
            &count_label,
            &TextStyle::new(12.0).with_weight(TextWeight::Medium),
            c.text_dim,
            Point::new(rect.x0, tabs_y + 9.0),
            None,
        );
        let shown = Self::list_capacity(rect);
        let total = visible.len();
        let scroll = self.list_scroll.min(total.saturating_sub(shown.max(1)));
        for (display_index, item_index) in visible.into_iter().skip(scroll).take(shown).enumerate()
        {
            let Some(item) = self.items.get(item_index) else {
                continue;
            };
            let y = tabs_y + 52.0 + ui::num::f64_of_usize(display_index) * 76.0;
            let row = Rect::new(rect.x0, y, rect.x1, y + 66.0);
            Self::card(
                ctx.scene,
                transform,
                if item_index == self.selected && !compact {
                    c.surfaces.raised
                } else {
                    c.surfaces.base
                },
                row,
                12.0,
            );
            let accent = c
                .accent
                .stops
                .get(display_index.saturating_add(3).min(9))
                .copied()
                .unwrap_or(c.text_primary);
            Self::card(
                ctx.scene,
                transform,
                accent,
                Rect::new(row.x0 + 10.0, row.y0 + 10.0, row.x0 + 54.0, row.y0 + 54.0),
                12.0,
            );
            let initial = item.title.chars().next().unwrap_or('•').to_string();
            ctx.text(
                &initial,
                &TextStyle::new(18.0).with_weight(TextWeight::Bold),
                c.surfaces.base,
                Point::new(row.x0 + 25.0, row.y0 + 20.0),
                None,
            );
            ctx.text(
                &item.title,
                &TextStyle::new(15.0).with_weight(TextWeight::Bold),
                c.text_primary,
                Point::new(row.x0 + 68.0, row.y0 + 12.0),
                None,
            );
            let sub = Self::item_summary(item);
            ctx.text(
                &sub,
                &TextStyle::new(12.0),
                c.text_dim,
                Point::new(row.x0 + 68.0, row.y0 + 37.0),
                None,
            );
            let star = Rect::new(row.x1 - 42.0, row.y0 + 11.0, row.x1 - 10.0, row.y0 + 43.0);
            if Self::is_favorite(item) {
                Icon::Star.draw_filled(
                    ctx.scene,
                    transform,
                    star,
                    c.accent.stops.get(5).copied().unwrap_or(c.text_primary),
                );
            } else {
                Icon::Star.draw(ctx.scene, transform, star, c.text_dim);
            }
        }
        if total > shown && shown > 0 {
            let track = Rect::new(
                rect.x1 - 4.0,
                tabs_y + 52.0,
                rect.x1,
                tabs_y + 52.0 + ui::num::f64_of_usize(shown) * 76.0 - 10.0,
            );
            let metrics = ScrollMetrics::new(
                ui::num::f64_of_usize(total),
                ui::num::f64_of_usize(shown),
                ui::num::f64_of_usize(scroll),
            );
            if let Some(thumb) = Scrollbar::vertical().thumb_rect(track, metrics) {
                ctx.scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &RoundedRect::from_rect(thumb, 2.0),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the detail pane that reads top-to-bottom"
    )]
    fn draw_detail(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let c = &theme.tokens.colors;
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let field_rows = match item.kind {
            SecretItemKind::Login | SecretItemKind::ApiKey => 3.0,
            SecretItemKind::Wifi => 2.0,
            SecretItemKind::SecureNote | SecretItemKind::Other => 1.0,
        };
        let content_height = 126.0
            + field_rows * 78.0
            + if item.kind == SecretItemKind::SecureNote {
                58.0
            } else {
                132.0
            };
        let rect = Rect::new(
            rect.x0,
            rect.y0,
            rect.x1,
            rect.y1.min(rect.y0 + content_height),
        );
        Self::card(ctx.scene, transform, c.surfaces.raised, rect, 18.0);
        let x = rect.x0 + 28.0;
        Self::card(
            ctx.scene,
            transform,
            c.accent.stops.get(5).copied().unwrap_or(c.text_primary),
            Rect::new(x, rect.y0 + 28.0, x + 58.0, rect.y0 + 86.0),
            16.0,
        );
        ctx.text(
            &item.title.chars().next().unwrap_or('•').to_string(),
            &TextStyle::new(24.0).with_weight(TextWeight::Bold),
            c.surfaces.base,
            Point::new(x + 20.0, rect.y0 + 43.0),
            None,
        );
        ctx.text(
            &item.title,
            &TextStyle::new(23.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(x + 76.0, rect.y0 + 31.0),
            None,
        );
        ctx.text(
            Self::kind_label(item.kind),
            &TextStyle::new(12.0).with_weight(TextWeight::Medium),
            c.text_dim,
            Point::new(x + 76.0, rect.y0 + 64.0),
            None,
        );
        Self::pill(
            &mut ctx,
            Rect::new(
                rect.x1 - 94.0,
                rect.y0 + 34.0,
                rect.x1 - 24.0,
                rect.y0 + 68.0,
            ),
            "Edit",
            false,
        );
        let mut y = rect.y0 + 126.0;
        let password = self
            .revealed
            .as_ref()
            .filter(|(id, _, until)| id == &item.id && *until > ui::time::now())
            .map_or("••••••••••••••••", |(_, value, _)| value.as_str());
        let origin_summary = item.origins.iter().next().map(|first| {
            let remaining = item.origins.len().saturating_sub(1);
            if remaining == 0 {
                first.clone()
            } else if remaining == 1 {
                format!("{first} and 1 more")
            } else {
                format!("{first} and {remaining} more")
            }
        });
        let origin = origin_summary.as_deref();
        let copy_origin_label = if item.origins.len() > 1 {
            "Copy addresses"
        } else {
            "Copy address"
        };
        let fields: Vec<(&str, &str, &str)> = match item.kind {
            SecretItemKind::Login => vec![
                ("USERNAME", item.username.as_str(), "Copy username"),
                ("PASSWORD", password, "Reveal 10s"),
                (
                    "SIGN-IN ADDRESS",
                    origin.unwrap_or("No address saved"),
                    copy_origin_label,
                ),
            ],
            SecretItemKind::ApiKey => vec![
                ("ACCOUNT", item.username.as_str(), "Copy account"),
                ("API KEY", password, "Reveal 10s"),
                (
                    "SERVICE",
                    origin.unwrap_or("No service address saved"),
                    copy_origin_label,
                ),
            ],
            SecretItemKind::Wifi => vec![
                (
                    "NETWORK",
                    item.fields
                        .get("network")
                        .map_or(item.title.as_str(), String::as_str),
                    "Copy network",
                ),
                ("PASSWORD", password, "Reveal 10s"),
            ],
            SecretItemKind::SecureNote => vec![("NOTE", password, "Reveal 10s")],
            SecretItemKind::Other => vec![("SECRET", password, "Reveal 10s")],
        };
        let detail_x = x + 12.0;
        let field_count = f64::from(u32::try_from(fields.len()).unwrap_or(0));
        Self::card(
            ctx.scene,
            transform,
            c.surfaces.base,
            Rect::new(
                rect.x0 + 20.0,
                y - 18.0,
                rect.x1 - 20.0,
                y + field_count * 78.0 - 8.0,
            ),
            14.0,
        );
        for (row, (label, value, action)) in fields.into_iter().enumerate() {
            ctx.text(
                label,
                &TextStyle::new(10.0).with_weight(TextWeight::Bold),
                c.text_dim,
                Point::new(detail_x, y),
                None,
            );
            ctx.text(
                value,
                &TextStyle::new(14.0).with_weight(TextWeight::Medium),
                c.text_primary,
                Point::new(detail_x, y + 25.0),
                None,
            );
            if self.detail_row_is_secret(row) {
                Button::paint(
                    ctx.scene,
                    transform,
                    ctx.text,
                    theme,
                    &ButtonFace::new(
                        Rect::new(rect.x1 - 120.0, y + 10.0, rect.x1 - 76.0, y + 54.0),
                        "",
                    )
                    .icon(Icon::Copy)
                    .accessible_name("Copy secret"),
                );
                Button::paint(
                    ctx.scene,
                    transform,
                    ctx.text,
                    theme,
                    &ButtonFace::new(
                        Rect::new(rect.x1 - 68.0, y + 10.0, rect.x1 - 24.0, y + 54.0),
                        "",
                    )
                    .icon(Icon::Eye)
                    .accessible_name("Reveal secret for ten seconds"),
                );
            } else {
                let action_face = ButtonFace::new(
                    Rect::new(rect.x1 - 150.0, y + 10.0, rect.x1 - 24.0, y + 54.0),
                    action,
                );
                Button::paint(ctx.scene, transform, ctx.text, theme, &action_face);
            }
            y += 78.0;
        }
        if !self.status.is_empty() {
            ctx.text(
                &self.status,
                &TextStyle::new(11.0).with_weight(TextWeight::Medium),
                c.text_dim,
                Point::new(x, rect.y1 - 16.0),
                None,
            );
        }
        if item.kind != SecretItemKind::SecureNote {
            y += 20.0;
            ctx.text(
                "NOTES",
                &TextStyle::new(10.0).with_weight(TextWeight::Bold),
                c.text_dim,
                Point::new(x, y),
                None,
            );
            ctx.text(
                if self.has_private_note(item) {
                    "Private note available — edit to view"
                } else {
                    "No notes"
                },
                &TextStyle::new(13.0),
                c.text_primary,
                Point::new(x, y + 25.0),
                None,
            );
        }
        let now = ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let updated = ui::format::relative(
            i64::try_from(now.saturating_sub(item.updated_unix)).unwrap_or(i64::MAX),
        );
        ctx.text(
            &format!("Updated {updated}"),
            &TextStyle::new(11.0),
            c.text_dim,
            Point::new(x, rect.y1 - 34.0),
            None,
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the editor that reads top-to-bottom"
    )]
    fn draw_editor(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let c = &theme.tokens.colors;
        Self::card(ctx.scene, transform, c.surfaces.raised, rect, 18.0);
        let x = rect.x0 + 36.0;
        let title_x = if self.drawer.is_overlay() {
            rect.x0 + 60.0
        } else {
            x
        };
        let w = rect.width() - 72.0;
        ctx.text(
            if self.editing.is_some() {
                "Edit item"
            } else {
                "New item"
            },
            &TextStyle::new(25.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(title_x, rect.y0 + 30.0),
            None,
        );
        if !Self::editor_is_compact(rect) {
            ctx.text(
                if self.draft_title.is_empty() {
                    Self::kind_label(self.draft_kind)
                } else {
                    &self.draft_title
                },
                &TextStyle::new(13.0),
                c.text_dim,
                Point::new(x, rect.y0 + 65.0),
                None,
            );
        }
        self.editor_scroll = self.editor_scroll.clamp(0.0, self.max_editor_scroll(rect));
        let body_viewport = self.editor_body_viewport(rect);
        ctx.scene
            .push_clip_layer(Fill::NonZero, transform, &body_viewport);
        let kinds = [
            SecretItemKind::Login,
            SecretItemKind::ApiKey,
            SecretItemKind::SecureNote,
            SecretItemKind::Wifi,
            SecretItemKind::Other,
        ];
        let segments = self.kind_segments(rect);
        let labels = ["Login", "API key", "Note", "Wi-Fi", "Other"];
        let selected = kinds
            .iter()
            .position(|kind| *kind == self.draft_kind)
            .unwrap_or(0);
        ToggleGroup::paint(
            ctx.scene,
            transform,
            ctx.text,
            theme,
            &SegmentedFace::new(&segments, &labels, selected)
                .style(SegmentedStyle::Pill)
                .radius(9.0)
                .text_size(if Self::editor_is_compact(rect) {
                    11.0
                } else {
                    12.0
                }),
        );
        let mut fields = vec![("NAME", ActiveField::Name)];
        match self.draft_kind {
            SecretItemKind::Login => fields.extend([
                ("USERNAME", ActiveField::Username),
                ("SIGN-IN ADDRESSES", ActiveField::Origin),
                ("PASSWORD", ActiveField::Password),
            ]),
            SecretItemKind::ApiKey => fields.extend([
                ("ACCOUNT OR LABEL", ActiveField::Username),
                ("SERVICE ADDRESSES", ActiveField::Origin),
                ("API KEY", ActiveField::Password),
            ]),
            SecretItemKind::Wifi => fields.extend([
                ("NETWORK NAME", ActiveField::Username),
                ("PASSWORD", ActiveField::Password),
            ]),
            SecretItemKind::Other => fields.extend([
                ("LABEL", ActiveField::Username),
                ("SECRET", ActiveField::Password),
            ]),
            SecretItemKind::SecureNote => {}
        }
        let mut y = self.editor_fields_top(rect);
        let field_step = Self::editor_field_step(rect);
        let input_presence = PresenceCtx::empty();
        for (label, field) in fields {
            let placeholder = match field {
                ActiveField::Name => "Name",
                ActiveField::Username if self.draft_kind == SecretItemKind::Wifi => {
                    "Network name (SSID)"
                }
                ActiveField::Username if self.draft_kind == SecretItemKind::ApiKey => {
                    "Account, project, or environment"
                }
                ActiveField::Username => "Username or label",
                ActiveField::Origin => "Addresses separated by commas",
                ActiveField::Password => {
                    if Self::editor_is_compact(rect) {
                        "Keep current password"
                    } else {
                        "Leave blank to keep current password"
                    }
                }
                ActiveField::Search
                | ActiveField::Notes
                | ActiveField::Tags
                | ActiveField::CustomFields => "",
            };
            ctx.text(
                label,
                &TextStyle::new(10.0).with_weight(TextWeight::Bold),
                c.text_dim,
                Point::new(x, y),
                None,
            );
            if let Some(input) = self.editor_inputs.get_mut(field) {
                input.set_placeholder(placeholder);
                input.apply_layout_external(Rect::new(x, y + 20.0, x + w, y + 64.0));
                Widget::set_focus(input, self.active_field == Some(field));
                input.draw(ctx.scene, transform, ctx.text, theme, &input_presence);
            }
            y += field_step;
        }
        if self.draft_kind != SecretItemKind::SecureNote {
            let generate_label = if Self::editor_is_compact(rect) {
                "Generate"
            } else {
                match self.draft_kind {
                    SecretItemKind::ApiKey => "Generate API key",
                    SecretItemKind::Other => "Generate random secret",
                    _ => "Generate strong password",
                }
            };
            let generate_rect = Rect::new(
                x,
                y,
                if Self::editor_is_compact(rect) {
                    x + w
                } else {
                    (x + 200.0).min(rect.x1 - 36.0)
                },
                y + 44.0,
            );
            Button::paint(
                ctx.scene,
                transform,
                ctx.text,
                theme,
                &ButtonFace::new(generate_rect, generate_label),
            );
            if !Self::editor_is_compact(rect) {
                ctx.text(
                    "20 random characters",
                    &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                    c.text_dim,
                    Point::new(x + 208.0, y + 10.0),
                    None,
                );
            }
            y += Self::editor_generate_height(rect);
        }
        ctx.text(
            if self.draft_kind == SecretItemKind::SecureNote {
                "NOTE"
            } else {
                "NOTES"
            },
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(x, y),
            None,
        );
        let note_placeholder = if self.draft_notes.is_empty() {
            if self.draft_kind == SecretItemKind::SecureNote {
                "Write note"
            } else {
                "Optional notes"
            }
        } else {
            ""
        };
        self.editor_inputs.notes.set_placeholder(note_placeholder);
        self.editor_inputs.notes.apply_layout_external(Rect::new(
            x,
            y + 20.0,
            x + w,
            y + if Self::editor_is_compact(rect) {
                70.0
            } else {
                84.0
            },
        ));
        Widget::set_focus(
            &mut self.editor_inputs.notes,
            self.active_field == Some(ActiveField::Notes),
        );
        self.editor_inputs
            .notes
            .draw(ctx.scene, transform, ctx.text, theme, &input_presence);
        y += if Self::editor_is_compact(rect) {
            82.0
        } else {
            104.0
        };
        ctx.text(
            "TAGS",
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(x, y),
            None,
        );
        self.editor_inputs
            .tags
            .apply_layout_external(Rect::new(x, y + 20.0, x + w, y + 64.0));
        Widget::set_focus(
            &mut self.editor_inputs.tags,
            self.active_field == Some(ActiveField::Tags),
        );
        self.editor_inputs
            .tags
            .draw(ctx.scene, transform, ctx.text, theme, &input_presence);
        y += 72.0;
        ctx.text(
            "CUSTOM FIELDS",
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(x, y),
            None,
        );
        self.editor_inputs
            .custom_fields
            .apply_layout_external(Rect::new(x, y + 20.0, x + w, y + 100.0));
        Widget::set_focus(
            &mut self.editor_inputs.custom_fields,
            self.active_field == Some(ActiveField::CustomFields),
        );
        self.editor_inputs.custom_fields.draw(
            ctx.scene,
            transform,
            ctx.text,
            theme,
            &input_presence,
        );
        y += 116.0;
        if !self.status.is_empty() {
            ctx.text(
                &self.status,
                &TextStyle::new(12.0).with_weight(TextWeight::Medium),
                c.text_dim,
                Point::new(x, y - 14.0),
                None,
            );
        }
        ctx.scene.pop_layer();
        let content_height = body_viewport.height() + self.max_editor_scroll(rect);
        let track = Rect::new(
            body_viewport.x1 - 5.0,
            body_viewport.y0 + 4.0,
            body_viewport.x1 - 2.0,
            body_viewport.y1 - 4.0,
        );
        if let Some(thumb) = Scrollbar::vertical().thumb_rect(
            track,
            ScrollMetrics::new(content_height, body_viewport.height(), self.editor_scroll),
        ) {
            ctx.scene.fill(
                Fill::NonZero,
                transform,
                c.border_standard,
                None,
                &RoundedRect::from_rect(thumb, 2.0),
            );
        }
        let (delete, cancel, save) = self.editor_footer_rects(rect);
        Button::paint(
            ctx.scene,
            transform,
            ctx.text,
            theme,
            &ButtonFace::new(
                save,
                if Self::editor_is_compact(rect) {
                    "Save"
                } else {
                    "Save changes"
                },
            )
            .style(ui::widget::ButtonStyle::Primary),
        );
        Button::paint(
            ctx.scene,
            transform,
            ctx.text,
            theme,
            &ButtonFace::new(cancel, "Cancel"),
        );
        if let Some(delete) = delete {
            Button::paint(
                ctx.scene,
                transform,
                ctx.text,
                theme,
                &ButtonFace::new(delete, "Delete").style(ui::widget::ButtonStyle::Danger),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the health report that reads top-to-bottom"
    )]
    fn draw_health(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let c = &theme.tokens.colors;
        Self::card(ctx.scene, transform, c.surfaces.raised, rect, 18.0);
        let compact = rect.width() < 420.0;
        let inset = if compact { 18.0 } else { 34.0 };
        let x = rect.x0 + inset;
        // On phones the drawer toggle shares the title row, but the review
        // cards should still use the full screen width below it.
        let header_x = if compact && self.drawer.is_overlay() {
            x + 60.0
        } else {
            x
        };
        let total = self.health.total.to_string();
        let reused = self.health.reused.to_string();
        let weak = self.health.weak.to_string();
        let old = self.health.older_than_year.to_string();
        ctx.text(
            "Password review",
            &TextStyle::new(25.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(header_x, rect.y0 + 30.0),
            None,
        );
        ctx.text(
            if compact {
                "Based on passwords saved here"
            } else {
                "Facts derived from the passwords stored here"
            },
            &TextStyle::new(14.0),
            c.text_dim,
            Point::new(header_x, rect.y0 + 66.0),
            None,
        );
        Self::card(
            ctx.scene,
            transform,
            c.surfaces.base,
            Rect::new(x, rect.y0 + 104.0, rect.x1 - inset, rect.y0 + 194.0),
            14.0,
        );
        ctx.text(
            &total,
            &TextStyle::new(38.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(x + 24.0, rect.y0 + 128.0),
            None,
        );
        ctx.text(
            "logins checked",
            &TextStyle::new(13.0).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(x + 76.0, rect.y0 + 145.0),
            None,
        );
        let cards = if compact {
            [
                (
                    "Reused passwords",
                    reused.as_str(),
                    "Used by more than one login",
                ),
                (
                    "Weak passwords",
                    weak.as_str(),
                    "Too short or easy to guess",
                ),
                (
                    "Older passwords",
                    old.as_str(),
                    "Unchanged for at least a year",
                ),
            ]
        } else {
            [
                (
                    "Repeated passwords",
                    reused.as_str(),
                    "Logins sharing the same password",
                ),
                (
                    "Short or simple passwords",
                    weak.as_str(),
                    "Under 12 characters or missing basic variety",
                ),
                (
                    "Not changed in a year",
                    old.as_str(),
                    "Based only on the saved update date",
                ),
            ]
        };
        for (i, (title, count, detail)) in cards.iter().enumerate() {
            let y = rect.y0 + 218.0 + ui::num::f64_of_usize(i) * 96.0;
            Self::card(
                ctx.scene,
                transform,
                c.surfaces.base,
                Rect::new(x, y, rect.x1 - inset, y + 78.0),
                12.0,
            );
            ctx.text(
                count,
                &TextStyle::new(22.0).with_weight(TextWeight::Bold),
                c.text_primary,
                Point::new(x + 18.0, y + 18.0),
                None,
            );
            ctx.text(
                title,
                &TextStyle::new(14.0).with_weight(TextWeight::Bold),
                c.text_primary,
                Point::new(x + 64.0, y + 13.0),
                None,
            );
            ctx.text(
                detail,
                &TextStyle::new(12.0),
                c.text_dim,
                Point::new(x + 64.0, y + 39.0),
                None,
            );
        }
    }

    fn app_access_viewport(rect: Rect) -> Rect {
        Rect::new(
            rect.x0 + 18.0,
            rect.y0 + 96.0,
            rect.x1 - 18.0,
            rect.y1 - 18.0,
        )
    }

    fn max_app_access_scroll(&self, rect: Rect) -> f64 {
        let content = ui::num::f64_of_usize(self.app_grants.len()) * 88.0;
        (content - Self::app_access_viewport(rect).height()).max(0.0)
    }

    fn scroll_app_access_by(&mut self, rect: Rect, delta: f64) -> bool {
        let before = self
            .app_access_scroll
            .clamp(0.0, self.max_app_access_scroll(rect));
        self.app_access_scroll = (before + delta).clamp(0.0, self.max_app_access_scroll(rect));
        self.app_access_scroll.to_bits() != before.to_bits()
    }

    fn activate_app_access(&mut self, rect: Rect, point: Point) -> bool {
        let viewport = Self::app_access_viewport(rect);
        if !viewport.contains(point) || point.x < viewport.x1 - 106.0 {
            return false;
        }
        let scroll = self
            .app_access_scroll
            .clamp(0.0, self.max_app_access_scroll(rect));
        let content_y = point.y - viewport.y0 + scroll;
        let index = ui::num::usize_of_f64(content_y / 88.0);
        let row_y = viewport.y0 + ui::num::f64_of_usize(index) * 88.0 - scroll;
        if !Rect::new(viewport.x1 - 106.0, row_y, viewport.x1, row_y + 76.0).contains(point) {
            return false;
        }
        let Some(grant) = self.app_grants.get(index).cloned() else {
            return false;
        };
        self.revoke_app_grant_index(index, &grant, rect)
    }

    fn revoke_app_grant_index(
        &mut self,
        index: usize,
        grant: &CredentialGrant,
        rect: Rect,
    ) -> bool {
        if let Some(model) = &mut self.model {
            match model.revoke_app_grant(grant) {
                Ok(true) => self.status = "App access revoked".into(),
                Ok(false) => self.status = "That access was already removed".into(),
                Err(error) => self.status = error,
            }
            self.app_grants = model.app_grants();
        } else {
            self.app_grants.remove(index);
            self.status = "App access revoked".into();
        }
        self.app_access_scroll = self.app_access_scroll.min(self.max_app_access_scroll(rect));
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative drawing of the app access list that reads top-to-bottom"
    )]
    fn draw_app_access(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let c = &theme.tokens.colors;
        Self::card(ctx.scene, transform, c.surfaces.raised, rect, 18.0);
        let compact = rect.width() < 420.0;
        let x = rect.x0 + if compact { 18.0 } else { 34.0 };
        let header_x = if compact && self.drawer.is_overlay() {
            x + 60.0
        } else {
            x
        };
        ctx.text(
            "App access",
            &TextStyle::new(25.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(header_x, rect.y0 + 30.0),
            None,
        );
        ctx.text(
            if !self.status.is_empty() {
                &self.status
            } else if self.app_grants.is_empty() {
                "Apps you always allow will appear here"
            } else {
                "Apps can use only the accounts and sites shown below"
            },
            &TextStyle::new(13.0),
            c.text_dim,
            Point::new(header_x, rect.y0 + 66.0),
            None,
        );
        let viewport = Self::app_access_viewport(rect);
        if self.app_grants.is_empty() {
            let empty = Rect::new(
                viewport.x0,
                viewport.y0 + 18.0,
                viewport.x1,
                viewport.y0 + 150.0,
            );
            Self::card(ctx.scene, transform, c.surfaces.base, empty, 14.0);
            ctx.text(
                "No ongoing access",
                &TextStyle::new(17.0).with_weight(TextWeight::Bold),
                c.text_primary,
                Point::new(empty.x0 + 22.0, empty.y0 + 26.0),
                None,
            );
            ctx.text("One-time and while-open access ends automatically. You stay in control when an app asks again.", &TextStyle::new(13.0), c.text_dim, Point::new(empty.x0 + 22.0, empty.y0 + 62.0), None);
            return;
        }
        ctx.scene
            .push_clip_layer(Fill::NonZero, transform, &viewport);
        let scroll = self
            .app_access_scroll
            .clamp(0.0, self.max_app_access_scroll(rect));
        for (index, grant) in self.app_grants.iter().enumerate() {
            let y = viewport.y0 + ui::num::f64_of_usize(index) * 88.0 - scroll;
            let row = Rect::new(viewport.x0, y, viewport.x1, y + 76.0);
            if row.y1 < viewport.y0 || row.y0 > viewport.y1 {
                continue;
            }
            Self::card(ctx.scene, transform, c.surfaces.base, row, 12.0);
            let app_name = grant.app_id.rsplit('.').next().unwrap_or(&grant.app_id);
            let title = self
                .items
                .iter()
                .find(|item| item.id == grant.item_id)
                .map_or("Saved account", |item| item.title.as_str());
            ctx.text.draw_text_weighted(
                ctx.scene,
                app_name,
                15.0,
                c.text_primary,
                transform.pre_translate((row.x0 + 16.0, row.y0 + 14.0).into()),
                Some(ui::num::f32_of_f64((row.width() - 132.0).max(40.0))),
                TextWeight::Bold,
            );
            let scope = grant.origin.as_deref().unwrap_or("This saved account");
            ctx.text.draw_text_weighted(
                ctx.scene,
                &format!("{title}  ·  {scope}"),
                12.0,
                c.text_dim,
                transform.pre_translate((row.x0 + 16.0, row.y0 + 43.0).into()),
                Some(ui::num::f32_of_f64((row.width() - 132.0).max(40.0))),
                TextWeight::Regular,
            );
            Button::paint(
                ctx.scene,
                transform,
                ctx.text,
                theme,
                &ButtonFace::new(
                    Rect::new(row.x1 - 94.0, row.y0 + 16.0, row.x1 - 12.0, row.y0 + 60.0),
                    "Revoke",
                )
                .style(ui::widget::ButtonStyle::Danger),
            );
        }
        ctx.scene.pop_layer();
        let metrics = ScrollMetrics::new(
            ui::num::f64_of_usize(self.app_grants.len()) * 88.0,
            viewport.height(),
            scroll,
        );
        let track = Rect::new(
            viewport.x1 - 4.0,
            viewport.y0 + 4.0,
            viewport.x1,
            viewport.y1 - 4.0,
        );
        if let Some(thumb) = Scrollbar::vertical().thumb_rect(track, metrics) {
            ctx.scene.fill(
                Fill::NonZero,
                transform,
                c.border_standard,
                None,
                &RoundedRect::from_rect(thumb, 2.0),
            );
        }
    }
}

impl PasswordsSurface {
    fn answer_resource_operation(
        &mut self,
        _kind: ui::tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != "tilo.passwords.operation" {
            return Ok(false);
        }
        let operation: crate::actions::PasswordOperation = serde_json::from_slice(payload)
            .map_err(|error| format!("invalid Passwords operation: {error}"))?;
        match operation {
            crate::actions::PasswordOperation::Add => self.begin_add(),
            crate::actions::PasswordOperation::SaveDraft => self.save_draft(),
            crate::actions::PasswordOperation::GeneratePassword => self.generate_password(),
            crate::actions::PasswordOperation::CancelDraft => self.cancel_edit(),
            crate::actions::PasswordOperation::Edit { id } => {
                if let Some(index) = self.items.iter().position(|item| item.id.0 == id) {
                    self.select_index(doc, index);
                }
                self.begin_edit();
            }
            crate::actions::PasswordOperation::Copy { id } => {
                if let Some(index) = self.items.iter().position(|item| item.id.0 == id) {
                    self.select_index(doc, index);
                }
                self.copy_selected();
            }
            crate::actions::PasswordOperation::Reveal { id } => {
                if let Some(index) = self.items.iter().position(|item| item.id.0 == id) {
                    self.select_index(doc, index);
                }
                self.reveal_selected();
            }
            crate::actions::PasswordOperation::RequestDelete { id } => {
                self.request_delete(SecretId(id));
            }
            crate::actions::PasswordOperation::ImportCsv => self.request_import(),
            crate::actions::PasswordOperation::ExportCsv => self.request_export_confirmation(),
        }
        Ok(true)
    }
}

impl Widget for PasswordsSurface {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        self.transfer_poll_deadline.set(None);
        self.poll_transfers();
        if self
            .revealed
            .as_ref()
            .is_some_and(|(_, _, until)| *until <= ui::time::now())
        {
            self.revealed = None;
        }
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["tilo.passwords.operation"]
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }
    #[allow(clippy::expect_used, reason = "a fresh taffy leaf cannot fail")]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Style::default()
            })
            .expect("fresh leaf");
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(l) = taffy.layout(node) {
            self.rect = Rect::new(
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
                origin.x + f64::from(l.location.x + l.size.width),
                origin.y + f64::from(l.location.y + l.size.height),
            );
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        let pad = 24.0;
        let content = Rect::new(
            self.rect.x0 + pad,
            self.rect.y0 + pad,
            self.rect.x1 - pad,
            self.rect.y1 - pad,
        );
        self.adapt_layout_for_text_scale(theme);
        let _ = self.drawer.tick(false);
        let main = self.resolve_drawer(content);
        if self.drawer.is_overlay() {
            let compact_content = compact_content_rect(self.mode, main);
            match self.mode {
                ShowcaseMode::Vault if self.mobile_detail => {
                    self.draw_detail(scene, tr, transform, theme, compact_content);
                    Self::pill(
                        &mut PaintCtx::new(scene, transform, tr, theme),
                        Rect::new(
                            compact_content.x0 + 8.0,
                            compact_content.y0 + 30.0,
                            compact_content.x0 + 50.0,
                            compact_content.y0 + 64.0,
                        ),
                        "",
                        false,
                    );
                    Icon::ChevronLeft.draw(
                        scene,
                        transform,
                        Rect::new(
                            compact_content.x0 + 21.0,
                            compact_content.y0 + 39.0,
                            compact_content.x0 + 37.0,
                            compact_content.y0 + 55.0,
                        ),
                        c.text_primary,
                    );
                }
                ShowcaseMode::Vault | ShowcaseMode::Import | ShowcaseMode::Export => {
                    self.draw_list(scene, tr, transform, theme, compact_content, true);
                }
                ShowcaseMode::Editor | ShowcaseMode::DeleteConfirm => {
                    self.draw_editor(scene, tr, transform, theme, compact_content);
                }
                ShowcaseMode::Health => {
                    self.draw_health(scene, tr, transform, theme, compact_content);
                }
                ShowcaseMode::AppAccess => {
                    self.draw_app_access(scene, tr, transform, theme, compact_content);
                }
            }
            if self.drawer.is_open() {
                self.drawer.draw_scrim(scene, transform);
                let side = self.drawer.sidebar_rect();
                self.draw_sidebar(
                    scene,
                    tr,
                    transform,
                    theme,
                    Rect::new(side.x0, side.y0, side.x1 - 18.0, side.y1),
                );
            }
            self.drawer.draw_toggle(
                scene,
                transform,
                theme,
                Rect::new(content.x0, content.y0, content.x0 + 48.0, content.y0 + 48.0),
                false,
            );
            self.draw_transfer_dialog(scene, tr, transform, theme, presence);
            return;
        }
        let side = self.drawer.sidebar_rect();
        let sidebar = Rect::new(side.x0, side.y0, side.x1 - 24.0, side.y1);
        self.draw_sidebar(scene, tr, transform, theme, sidebar);
        let list = Rect::new(main.x0, content.y0 + 4.0, main.x0 + 330.0, content.y1);
        self.draw_list(scene, tr, transform, theme, list, false);
        let detail = Rect::new(list.x1 + 24.0, content.y0, content.x1, content.y1);
        match self.mode {
            ShowcaseMode::Vault => self.draw_detail(scene, tr, transform, theme, detail),
            ShowcaseMode::Editor | ShowcaseMode::DeleteConfirm => {
                self.draw_editor(scene, tr, transform, theme, detail);
            }
            ShowcaseMode::Health => self.draw_health(scene, tr, transform, theme, detail),
            ShowcaseMode::AppAccess => {
                self.draw_app_access(scene, tr, transform, theme, detail);
            }
            ShowcaseMode::Import | ShowcaseMode::Export => {
                self.draw_detail(scene, tr, transform, theme, detail);
            }
        }
        self.draw_transfer_dialog(scene, tr, transform, theme, presence);
    }
    #[expect(
        clippy::too_many_lines,
        reason = "event routing is ordered by precedence (dialogs, editor, list); splitting it would hide that order"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::Focused(false)) {
            // A reveal is only for the actively attended window. Hiding it on
            // deactivation also avoids it surviving task switching or a
            // screenshot taken by another surface.
            self.clear_revealed();
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        // The vault search is a real shared text widget, so pointer selection,
        // Unicode word motion, IME, and clipboard shortcuts follow the same path
        // as every other application input.
        if self.mode == ShowcaseMode::Vault || self.drawer.is_docked() {
            Widget::set_focus(
                &mut self.search_input,
                self.active_field == Some(ActiveField::Search),
            );
            let mut search_handled = false;
            self.search_input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut search_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if self.search_input.is_focused() {
                self.active_field = Some(ActiveField::Search);
            }
            if self.search_input.take_dirty().is_some() {
                let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                let query = self.search_input.value().to_owned();
                self.commit_view(
                    doc,
                    &crate::resource::PasswordsAction::SetSearch { value: query },
                );
                self.list_scroll = 0;
            }
            if search_handled {
                *ctx.handled = true;
                return;
            }
        }
        if matches!(
            self.transfer_dialog,
            Some(TransferDialog::Backup | TransferDialog::Restore { .. })
        ) {
            let restoring = matches!(self.transfer_dialog, Some(TransferDialog::Restore { .. }));
            if let WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == ElementState::Pressed
                && key.logical_key == Key::Named(NamedKey::Tab)
                && !restoring
            {
                self.backup_focus = usize::from(self.backup_focus == 0);
                self.backup_password.set_focus(self.backup_focus == 0);
                self.backup_confirmation.set_focus(self.backup_focus == 1);
                *ctx.handled = true;
                return;
            }
            let mut input_handled = false;
            if matches!(event, WindowEvent::CursorMoved { .. }) {
                self.backup_password.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        handled: &mut input_handled,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
                if !restoring {
                    self.backup_confirmation.handle_event(
                        event,
                        &mut ui::widget::EventCtx {
                            handled: &mut input_handled,
                            ..ctx.reborrow(ctx.path, ctx.node_id)
                        },
                    );
                }
            } else {
                let input = if self.backup_focus == 0 {
                    &mut self.backup_password
                } else {
                    &mut self.backup_confirmation
                };
                input.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        handled: &mut input_handled,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            }
            if input_handled {
                *ctx.handled = true;
                return;
            }
        }
        if self.mode == ShowcaseMode::Editor {
            match event {
                WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
                    let point = Point::new(
                        touch.location.x / ctx.scale_factor,
                        touch.location.y / ctx.scale_factor,
                    );
                    self.resolve_drawer(Rect::new(
                        self.rect.x0 + 24.0,
                        self.rect.y0 + 24.0,
                        self.rect.x1 - 24.0,
                        self.rect.y1 - 24.0,
                    ));
                    let panel = self.current_panel_rect();
                    if self.editor_body_viewport(panel).contains(point) {
                        self.editor_touch = Some((touch.id, point, self.editor_scroll));
                    }
                }
                WindowEvent::Touch(touch) if touch.phase == TouchPhase::Moved => {
                    if let Some((id, start, initial_scroll)) = self.editor_touch
                        && id == touch.id
                    {
                        let point = Point::new(
                            touch.location.x / ctx.scale_factor,
                            touch.location.y / ctx.scale_factor,
                        );
                        if (start.y - point.y).abs() > 8.0 {
                            self.editor_scroll = initial_scroll;
                            let panel = self.current_panel_rect();
                            let _ = self.scroll_editor_by(panel, start.y - point.y);
                            *ctx.handled = true;
                            return;
                        }
                    }
                }
                WindowEvent::Touch(touch)
                    if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) =>
                {
                    if let Some((id, start, _)) = self.editor_touch
                        && id == touch.id
                    {
                        let point = Point::new(
                            touch.location.x / ctx.scale_factor,
                            touch.location.y / ctx.scale_factor,
                        );
                        self.editor_touch = None;
                        if (start.y - point.y).abs() > 8.0 {
                            *ctx.handled = true;
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        if self.route_editor_inputs(event, ctx) {
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.activate_transfer(self.last_pointer) {
                    *ctx.handled = true;
                    return;
                }
                let content = Rect::new(
                    self.rect.x0 + 24.0,
                    self.rect.y0 + 24.0,
                    self.rect.x1 - 24.0,
                    self.rect.y1 - 24.0,
                );
                let toggle =
                    Rect::new(content.x0, content.y0, content.x0 + 48.0, content.y0 + 48.0);
                self.resolve_drawer(content);
                if self.drawer.is_overlay() && toggle.contains(self.last_pointer) {
                    self.drawer.toggle();
                    *ctx.handled = true;
                    return;
                }
                let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                if self.activate_sidebar(doc, self.last_pointer) {
                    *ctx.handled = true;
                    return;
                }
                if self.drawer.press_dismisses(self.last_pointer) {
                    *ctx.handled = true;
                    return;
                }
                let main = self.drawer.content_rect();
                if self.drawer.is_docked() {
                    let sidebar = self.drawer.sidebar_rect();
                    if Rect::new(
                        sidebar.x0,
                        content.y0 + 126.0,
                        sidebar.x1,
                        content.y0 + 176.0,
                    )
                    .contains(self.last_pointer)
                    {
                        self.mode = ShowcaseMode::Health;
                        *ctx.handled = true;
                        return;
                    }
                }
                let list = if self.drawer.is_docked() {
                    Rect::new(main.x0, content.y0 + 4.0, main.x0 + 330.0, content.y1)
                } else if self.mode == ShowcaseMode::Editor {
                    main
                } else {
                    Rect::new(main.x0 + 60.0, main.y0, main.x1, main.y1)
                };
                if self.mode == ShowcaseMode::Vault || self.drawer.is_docked() {
                    if Rect::new(
                        list.x1 - 118.0,
                        list.y0 - 8.0,
                        list.x1 + 4.0,
                        list.y0 + 42.0,
                    )
                    .contains(self.last_pointer)
                    {
                        self.begin_add();
                        *ctx.handled = true;
                        return;
                    }
                    if Rect::new(list.x0, list.y0 + 50.0, list.x1, list.y0 + 104.0)
                        .contains(self.last_pointer)
                    {
                        self.active_field = Some(ActiveField::Search);
                        Widget::set_focus(&mut self.search_input, true);
                        *ctx.handled = true;
                        return;
                    }
                    let first_y = list.y0 + 168.0;
                    if self.last_pointer.x >= list.x0
                        && self.last_pointer.x <= list.x1
                        && self.last_pointer.y >= first_y
                    {
                        let display_index =
                            ui::num::usize_of_f64((self.last_pointer.y - first_y) / 76.0);
                        let scroll = self.list_scroll.min(self.max_list_scroll(list));
                        if display_index < Self::list_capacity(list)
                            && let Some(item_index) = self
                                .visible_indices()
                                .get(scroll.saturating_add(display_index))
                                .copied()
                        {
                            let row_y = first_y + ui::num::f64_of_usize(display_index) * 76.0;
                            if Rect::new(list.x1 - 52.0, row_y, list.x1, row_y + 68.0)
                                .contains(self.last_pointer)
                            {
                                self.toggle_favorite(item_index);
                                *ctx.handled = true;
                                return;
                            }
                            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                            self.select_index(doc, item_index);
                            if self.drawer.is_overlay() {
                                self.mobile_detail = true;
                            }
                            *ctx.handled = true;
                            return;
                        }
                    }
                }
                let panel = if self.drawer.is_docked() {
                    Rect::new(list.x1 + 24.0, content.y0, content.x1, content.y1)
                } else {
                    list
                };
                match self.mode {
                    ShowcaseMode::AppAccess => {
                        if self.activate_app_access(panel, self.last_pointer) {
                            *ctx.handled = true;
                        }
                    }
                    ShowcaseMode::Vault if self.drawer.is_overlay() && self.mobile_detail => {
                        if Rect::new(panel.x0, panel.y0 + 18.0, panel.x0 + 62.0, panel.y0 + 76.0)
                            .contains(self.last_pointer)
                        {
                            self.mobile_detail = false;
                            *ctx.handled = true;
                            return;
                        }
                        if Rect::new(
                            panel.x1 - 104.0,
                            panel.y0 + 24.0,
                            panel.x1 - 14.0,
                            panel.y0 + 80.0,
                        )
                        .contains(self.last_pointer)
                        {
                            self.begin_edit();
                            *ctx.handled = true;
                            return;
                        }
                        if self.last_pointer.x >= panel.x1 - 150.0 {
                            for row in 0_u32..3 {
                                let y0 = panel.y0 + 116.0 + f64::from(row) * 78.0;
                                if (y0..y0 + 78.0).contains(&self.last_pointer.y) {
                                    self.activate_detail_row_at(
                                        row as usize,
                                        self.last_pointer.x,
                                        panel.x1,
                                    );
                                    *ctx.handled = true;
                                    return;
                                }
                            }
                        }
                    }
                    ShowcaseMode::Vault if self.drawer.is_docked() => {
                        if Rect::new(
                            panel.x1 - 104.0,
                            panel.y0 + 24.0,
                            panel.x1 - 14.0,
                            panel.y0 + 80.0,
                        )
                        .contains(self.last_pointer)
                        {
                            self.begin_edit();
                            *ctx.handled = true;
                            return;
                        }
                        let action_x = panel.x1 - 150.0;
                        if self.last_pointer.x >= action_x
                            && self.last_pointer.y >= panel.y0 + 120.0
                            && self.last_pointer.y < panel.y0 + 198.0
                        {
                            self.activate_detail_row_at(0, self.last_pointer.x, panel.x1);
                            *ctx.handled = true;
                            return;
                        }
                        if self.last_pointer.x >= action_x
                            && self.last_pointer.y >= panel.y0 + 276.0
                            && self.last_pointer.y < panel.y0 + 354.0
                        {
                            self.activate_detail_row_at(2, self.last_pointer.x, panel.x1);
                            *ctx.handled = true;
                            return;
                        }
                        if self.last_pointer.x >= action_x
                            && self.last_pointer.y >= panel.y0 + 198.0
                            && self.last_pointer.y < panel.y0 + 276.0
                        {
                            self.activate_detail_row_at(1, self.last_pointer.x, panel.x1);
                            *ctx.handled = true;
                        }
                    }
                    ShowcaseMode::Editor => {
                        let x = panel.x0 + 36.0;
                        for (segment, kind) in self.kind_segments(panel).into_iter().zip([
                            SecretItemKind::Login,
                            SecretItemKind::ApiKey,
                            SecretItemKind::SecureNote,
                            SecretItemKind::Wifi,
                            SecretItemKind::Other,
                        ]) {
                            if segment.contains(self.last_pointer) {
                                self.select_kind(kind);
                                *ctx.handled = true;
                                return;
                            }
                        }
                        let ordinary: Vec<_> = self
                            .editor_field_order()
                            .into_iter()
                            .filter(|field| {
                                !matches!(
                                    field,
                                    ActiveField::Notes
                                        | ActiveField::Tags
                                        | ActiveField::CustomFields
                                )
                            })
                            .collect();
                        for (index, field) in ordinary.into_iter().enumerate() {
                            let y = self.editor_fields_top(panel)
                                + 20.0
                                + f64::from(u32::try_from(index).unwrap_or(0))
                                    * Self::editor_field_step(panel);
                            if Rect::new(x, y, panel.x1 - 36.0, y + 44.0)
                                .contains(self.last_pointer)
                            {
                                self.active_field = Some(field);
                                *ctx.handled = true;
                                return;
                            }
                        }
                        let note_y = self.editor_note_y(panel);
                        if self.draft_kind != SecretItemKind::SecureNote
                            && Rect::new(x, note_y - 52.0, x + 206.0, note_y - 4.0)
                                .contains(self.last_pointer)
                        {
                            self.generate_password();
                            *ctx.handled = true;
                            return;
                        }
                        if Rect::new(x, note_y + 16.0, panel.x1 - 36.0, note_y + 92.0)
                            .contains(self.last_pointer)
                        {
                            self.active_field = Some(ActiveField::Notes);
                            *ctx.handled = true;
                            return;
                        }
                        let tags_y = self.editor_tags_y(panel);
                        if Rect::new(x, tags_y + 16.0, panel.x1 - 36.0, tags_y + 68.0)
                            .contains(self.last_pointer)
                        {
                            self.active_field = Some(ActiveField::Tags);
                            *ctx.handled = true;
                            return;
                        }
                        let custom_y = self.editor_custom_fields_y(panel);
                        if Rect::new(x, custom_y + 16.0, panel.x1 - 36.0, custom_y + 104.0)
                            .contains(self.last_pointer)
                        {
                            self.active_field = Some(ActiveField::CustomFields);
                            *ctx.handled = true;
                            return;
                        }
                        let (delete, cancel, save) = self.editor_footer_rects(panel);
                        if delete.is_some_and(|rect| rect.contains(self.last_pointer)) {
                            if let Some(id) = self.editing.clone() {
                                self.request_delete(id);
                            }
                            *ctx.handled = true;
                            return;
                        }
                        if cancel.contains(self.last_pointer) {
                            self.cancel_edit();
                            *ctx.handled = true;
                            return;
                        }
                        if save.contains(self.last_pointer) {
                            self.save_draft();
                            *ctx.handled = true;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.resolve_drawer(Rect::new(
                    self.rect.x0 + 24.0,
                    self.rect.y0 + 24.0,
                    self.rect.x1 - 24.0,
                    self.rect.y1 - 24.0,
                ));
                let panel = self.current_panel_rect();
                if self.mode == ShowcaseMode::AppAccess
                    && Self::app_access_viewport(panel).contains(self.last_pointer)
                {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 46.0,
                        MouseScrollDelta::PixelDelta(position) => -position.y,
                    };
                    if self.scroll_app_access_by(panel, dy) {
                        *ctx.handled = true;
                    }
                } else if self.mode == ShowcaseMode::Editor
                    && self.editor_body_viewport(panel).contains(self.last_pointer)
                {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 46.0,
                        MouseScrollDelta::PixelDelta(position) => -position.y,
                    };
                    if self.scroll_editor_by(panel, dy) {
                        *ctx.handled = true;
                    }
                } else if self.drawer.sidebar_contains(self.last_pointer) {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 46.0,
                        MouseScrollDelta::PixelDelta(position) => position.y,
                    };
                    if self.scroll_sidebar_by(-dy) {
                        *ctx.handled = true;
                    }
                } else {
                    let list = self.current_list_rect();
                    if (self.mode == ShowcaseMode::Vault || self.drawer.is_docked())
                        && Rect::new(list.x0, list.y0 + 150.0, list.x1, list.y1)
                            .contains(self.last_pointer)
                    {
                        let rows = match delta {
                            MouseScrollDelta::LineDelta(_, y) => {
                                ui::num::usize_of_f64(f64::from(y.abs().ceil()))
                            }
                            MouseScrollDelta::PixelDelta(position) => {
                                ui::num::usize_of_f64((position.y.abs() / 38.0).ceil())
                            }
                        }
                        .max(1);
                        let upwards = match delta {
                            MouseScrollDelta::LineDelta(_, y) => *y > 0.0,
                            MouseScrollDelta::PixelDelta(position) => position.y > 0.0,
                        };
                        let next = if upwards {
                            self.list_scroll.saturating_sub(rows)
                        } else {
                            self.list_scroll.saturating_add(rows)
                        };
                        if self.scroll_list_to(list, next) {
                            *ctx.handled = true;
                        }
                    }
                }
            }
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                self.resolve_drawer(Rect::new(
                    self.rect.x0 + 24.0,
                    self.rect.y0 + 24.0,
                    self.rect.x1 - 24.0,
                    self.rect.y1 - 24.0,
                ));
                if self.drawer.sidebar_contains(point) {
                    self.sidebar_touch = Some((touch.id, point, self.sidebar_scroll));
                    *ctx.handled = true;
                } else {
                    let panel = self.current_panel_rect();
                    if self.mode == ShowcaseMode::AppAccess
                        && Self::app_access_viewport(panel).contains(point)
                    {
                        self.app_access_touch = Some((touch.id, point, self.app_access_scroll));
                        *ctx.handled = true;
                        return;
                    }
                    if self.mode == ShowcaseMode::Editor
                        && self.editor_body_viewport(panel).contains(point)
                    {
                        self.editor_touch = Some((touch.id, point, self.editor_scroll));
                        *ctx.handled = true;
                        return;
                    }
                    let list = self.current_list_rect();
                    if (self.mode == ShowcaseMode::Vault || self.drawer.is_docked())
                        && Rect::new(list.x0, list.y0 + 150.0, list.x1, list.y1).contains(point)
                    {
                        self.list_touch = Some((touch.id, point, self.list_scroll));
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Moved => {
                if let Some((id, start, initial_scroll)) = self.sidebar_touch
                    && id == touch.id
                {
                    let point = Point::new(
                        touch.location.x / ctx.scale_factor,
                        touch.location.y / ctx.scale_factor,
                    );
                    self.sidebar_scroll = initial_scroll;
                    let _ = self.scroll_sidebar_by(start.y - point.y);
                    *ctx.handled = true;
                } else if let Some((id, start, initial_scroll)) = self.editor_touch
                    && id == touch.id
                {
                    let point = Point::new(
                        touch.location.x / ctx.scale_factor,
                        touch.location.y / ctx.scale_factor,
                    );
                    self.editor_scroll = initial_scroll;
                    let panel = self.current_panel_rect();
                    let _ = self.scroll_editor_by(panel, start.y - point.y);
                    *ctx.handled = true;
                } else if let Some((id, start, initial_scroll)) = self.app_access_touch
                    && id == touch.id
                {
                    let point = Point::new(
                        touch.location.x / ctx.scale_factor,
                        touch.location.y / ctx.scale_factor,
                    );
                    self.app_access_scroll = initial_scroll;
                    let panel = self.current_panel_rect();
                    let _ = self.scroll_app_access_by(panel, start.y - point.y);
                    *ctx.handled = true;
                } else if let Some((id, start, initial_scroll)) = self.list_touch
                    && id == touch.id
                {
                    let point = Point::new(
                        touch.location.x / ctx.scale_factor,
                        touch.location.y / ctx.scale_factor,
                    );
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "a rounded drag distance in rows; the float-to-int cast saturates"
                    )]
                    let rows = ((start.y - point.y) / 76.0).round() as isize;
                    let next = initial_scroll.saturating_add_signed(rows);
                    let list = self.current_list_rect();
                    let _ = self.scroll_list_to(list, next);
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch)
                if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) =>
            {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                let sidebar_scroll = self.sidebar_touch.take().is_some_and(|(id, start, _)| {
                    id == touch.id && (start.y - point.y).abs() > 8.0
                });
                let list_scroll = self.list_touch.take().is_some_and(|(id, start, _)| {
                    id == touch.id && (start.y - point.y).abs() > 8.0
                });
                let editor_scroll = self.editor_touch.take().is_some_and(|(id, start, _)| {
                    id == touch.id && (start.y - point.y).abs() > 8.0
                });
                let app_access_scroll =
                    self.app_access_touch.take().is_some_and(|(id, start, _)| {
                        id == touch.id && (start.y - point.y).abs() > 8.0
                    });
                let was_scroll =
                    sidebar_scroll || list_scroll || editor_scroll || app_access_scroll;
                if touch.phase == TouchPhase::Ended && !was_scroll {
                    self.last_pointer = point;
                    let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                    if self.activate_touch(doc, point) {
                        *ctx.handled = true;
                    }
                } else if was_scroll {
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.transfer_dialog.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.transfer_dialog = None;
                            self.clear_backup_form();
                        }
                        Key::Named(NamedKey::Space) => {
                            if let Some(TransferDialog::ExportPlaintext { acknowledged }) =
                                &mut self.transfer_dialog
                            {
                                *acknowledged = !*acknowledged;
                            }
                        }
                        Key::Named(NamedKey::Enter) => match self.transfer_dialog {
                            Some(TransferDialog::ImportPreview { .. }) => self.apply_import(),
                            Some(TransferDialog::ExportPlaintext { acknowledged: true }) => {
                                self.begin_plaintext_export();
                            }
                            Some(TransferDialog::DeleteConfirm { .. }) => self.confirm_delete(),
                            Some(TransferDialog::Backup) => self.begin_encrypted_backup(),
                            Some(TransferDialog::Restore { .. }) => self.apply_encrypted_restore(),
                            _ => {}
                        },
                        _ => return,
                    }
                    *ctx.handled = true;
                    return;
                }
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    if self.drawer.is_overlay() && self.drawer.is_open() {
                        self.drawer.close();
                    } else if self.mode == ShowcaseMode::Editor {
                        self.cancel_edit();
                    } else if self.mobile_detail {
                        self.mobile_detail = false;
                    } else {
                        self.active_field = None;
                    }
                    *ctx.handled = true;
                    return;
                }
                match &event.logical_key {
                    Key::Named(NamedKey::Tab) if self.mode == ShowcaseMode::Editor => {
                        let fields = self.editor_field_order();
                        let backwards = ctx.modifiers.state().shift_key();
                        let last = fields.len().saturating_sub(1);
                        let next = self
                            .active_field
                            .and_then(|active| fields.iter().position(|field| *field == active))
                            .map_or_else(
                                || if backwards { last } else { 0 },
                                |index| {
                                    if backwards {
                                        index.checked_sub(1).unwrap_or(last)
                                    } else {
                                        index
                                            .saturating_add(1)
                                            .checked_rem(fields.len())
                                            .unwrap_or(0)
                                    }
                                },
                            );
                        self.active_field = fields.get(next).copied();
                        if let Some(field) = self.active_field {
                            self.ensure_editor_field_visible(self.current_panel_rect(), field);
                        }
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) if self.mode == ShowcaseMode::Editor => {
                        self.save_draft();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown)
                        if self.active_field.is_none() && !self.items.is_empty() =>
                    {
                        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                        self.move_list_selection(doc, true);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) if self.active_field.is_none() => {
                        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
                        self.move_list_selection(doc, false);
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text == "/" => {
                        self.active_field = Some(ActiveField::Search);
                        Widget::set_focus(&mut self.search_input, true);
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("n") => {
                        self.begin_add();
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("e") => {
                        self.begin_edit();
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("c") => {
                        self.copy_selected();
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("r") => {
                        self.reveal_selected();
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("p") => {
                        self.mode = ShowcaseMode::Health;
                        self.mobile_detail = false;
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("i") => {
                        self.request_import();
                        *ctx.handled = true;
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("x") => {
                        self.request_export_confirmation();
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &'static str {
        "PasswordsSurface"
    }
    #[expect(
        clippy::too_many_lines,
        reason = "builds the whole accessibility tree mirroring the drawn layout"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        fn normalize_sibling_keys(node: &mut AccessNode, path: &str) {
            if node.children.iter().any(|child| child.key.is_some()) {
                for (index, child) in node.children.iter_mut().enumerate() {
                    if child.key.is_none() {
                        child.key = Some(ui::a11y::hash_key(&(
                            path,
                            child.role,
                            child.name.as_deref(),
                            index,
                        )));
                    }
                }
            }
            for (index, child) in node.children.iter_mut().enumerate() {
                normalize_sibling_keys(child, &format!("{path}/{index}"));
            }
        }

        let rows = self
            .visible_indices()
            .into_iter()
            .filter_map(|index| self.items.get(index).map(|item| (index, item)))
            .map(|(index, item)| {
                AccessNode::new(Role::ListItem)
                    .with_name(&item.title)
                    .with_description(if item.username.is_empty() {
                        "Saved login"
                    } else {
                        &item.username
                    })
                    .with_action_target(format!("passwords.select:{index}"))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(index == self.selected),
                    )
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name(if Self::is_favorite(item) {
                                "Remove from favorites"
                            } else {
                                "Add to favorites"
                            })
                            .with_action_target(format!("passwords.favorite:{index}")),
                    ])
                    .with_hashed_key(&item.id.0)
            })
            .collect();
        let search = AccessNode::new(Role::TextInput)
            .with_name("Search passwords")
            .with_value(&self.query)
            .with_action_target("passwords.search")
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.active_field == Some(ActiveField::Search)),
            );
        let list = AccessNode::new(Role::List)
            .with_name("Vault items")
            .with_children(rows);
        let navigation =
            self.sidebar_nav(self.sidebar_viewport())
                .accessibility("Vault navigation", |id| {
                    match id {
                        "all" => "passwords.filter.all",
                        "favorites" => "passwords.filter.favorites",
                        "logins" => "passwords.filter.logins",
                        "api-keys" => "passwords.filter.api-keys",
                        "notes" => "passwords.filter.notes",
                        "wifi" => "passwords.filter.wifi",
                        "other" => "passwords.filter.other",
                        "review" => "passwords.review",
                        "app-access" => "passwords.app-access",
                        "backup" => "passwords.backup",
                        "restore" => "passwords.restore",
                        "import" => "passwords.import",
                        "export" => "passwords.export",
                        _ => "passwords.navigation.toggle",
                    }
                    .to_owned()
                });
        let detail = self.items.get(self.selected).map(|item| {
            let value_label = Self::secret_field_label(item.kind);
            let copy_label = format!("Copy {}", value_label.to_lowercase());
            let reveal_label = format!("Reveal {} for ten seconds", value_label.to_lowercase());
            let mut detail_children = Vec::new();
            if !item.username.is_empty() {
                detail_children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(match item.kind {
                            SecretItemKind::Wifi => "Network name",
                            SecretItemKind::ApiKey => "Account or label",
                            _ => "Username or label",
                        })
                        .with_value(&item.username),
                );
            }
            detail_children.extend([
                AccessNode::new(Role::StaticText)
                    .with_name(value_label)
                    .with_description("Hidden; use Reveal for ten seconds"),
                AccessNode::new(Role::Button)
                    .with_name(copy_label)
                    .with_action_target(format!("passwords.copy:{}", item.id.0)),
                AccessNode::new(Role::Button)
                    .with_name(reveal_label)
                    .with_action_target(format!("passwords.reveal:{}", item.id.0)),
                AccessNode::new(Role::Button)
                    .with_name("Edit item")
                    .with_action_target(format!("passwords.edit:{}", item.id.0)),
            ]);
            if matches!(item.kind, SecretItemKind::Login | SecretItemKind::ApiKey)
                && !item.origins.is_empty()
            {
                detail_children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Copy sign-in address")
                        .with_action_target(format!("passwords.copy-origin:{}", item.id.0)),
                );
            }
            AccessNode::new(Role::Group)
                .with_name(format!("{} details", item.title))
                .with_children(detail_children)
        });
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name(if self.drawer.is_open() {
                    "Hide navigation"
                } else {
                    "Show navigation"
                })
                .with_action_target("passwords.navigation.toggle")
                .with_state(AccessState::new().focusable(true)),
            navigation,
            AccessNode::new(Role::Button)
                .with_name("New item")
                .with_action_target("passwords.add"),
            search,
            list,
        ];
        if self.mobile_detail {
            children.insert(
                0,
                AccessNode::new(Role::Button)
                    .with_name("Back to passwords")
                    .with_action_target("passwords.back"),
            );
        }
        if self.mode == ShowcaseMode::Editor {
            for kind in [
                SecretItemKind::Login,
                SecretItemKind::ApiKey,
                SecretItemKind::SecureNote,
                SecretItemKind::Wifi,
                SecretItemKind::Other,
            ] {
                children.push(
                    AccessNode::new(Role::RadioButton)
                        .with_name(Self::kind_label(kind))
                        .with_action_target(format!(
                            "passwords.kind:{}",
                            match kind {
                                SecretItemKind::Login => "login",
                                SecretItemKind::ApiKey => "api-key",
                                SecretItemKind::SecureNote => "note",
                                SecretItemKind::Wifi => "wifi",
                                SecretItemKind::Other => "other",
                            }
                        ))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(kind == self.draft_kind),
                        ),
                );
            }
            let mut fields: Vec<(&str, &str, ActiveField)> =
                vec![("Name", "passwords.field.name", ActiveField::Name)];
            match self.draft_kind {
                SecretItemKind::Login => fields.extend([
                    (
                        "Username",
                        "passwords.field.username",
                        ActiveField::Username,
                    ),
                    (
                        "Sign-in address",
                        "passwords.field.origin",
                        ActiveField::Origin,
                    ),
                    (
                        "Password",
                        "passwords.field.password",
                        ActiveField::Password,
                    ),
                ]),
                SecretItemKind::ApiKey => fields.extend([
                    (
                        "Account or label",
                        "passwords.field.username",
                        ActiveField::Username,
                    ),
                    (
                        "Service address",
                        "passwords.field.origin",
                        ActiveField::Origin,
                    ),
                    ("API key", "passwords.field.password", ActiveField::Password),
                ]),
                SecretItemKind::Wifi => fields.extend([
                    (
                        "Network name",
                        "passwords.field.username",
                        ActiveField::Username,
                    ),
                    (
                        "Password",
                        "passwords.field.password",
                        ActiveField::Password,
                    ),
                ]),
                SecretItemKind::Other => fields.extend([
                    ("Label", "passwords.field.username", ActiveField::Username),
                    ("Secret", "passwords.field.password", ActiveField::Password),
                ]),
                SecretItemKind::SecureNote => {}
            }
            children.extend(fields.into_iter().filter_map(|(name, target, field)| {
                self.editor_inputs
                    .get(field)
                    .and_then(Widget::accessibility)
                    .map(|node| node.with_name(name).with_action_target(target))
            }));
            if let Some(notes) = Widget::accessibility(&self.editor_inputs.notes) {
                children.push(
                    notes
                        .with_name(if self.draft_kind == SecretItemKind::SecureNote {
                            "Note"
                        } else {
                            "Notes"
                        })
                        .with_action_target("passwords.field.notes"),
                );
            }
            if let Some(tags) = Widget::accessibility(&self.editor_inputs.tags) {
                children.push(
                    tags.with_name("Tags")
                        .with_action_target("passwords.field.tags"),
                );
            }
            if let Some(custom_fields) = Widget::accessibility(&self.editor_inputs.custom_fields) {
                children.push(
                    custom_fields
                        .with_name("Custom fields, one label and value per line")
                        .with_action_target("passwords.field.custom-fields"),
                );
            }
            if self.draft_kind != SecretItemKind::SecureNote {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(format!(
                            "Generate {}",
                            Self::secret_field_label(self.draft_kind).to_lowercase()
                        ))
                        .with_action_target("passwords.generate"),
                );
            }
            children.extend([
                AccessNode::new(Role::Button)
                    .with_name("Cancel")
                    .with_action_target("passwords.cancel"),
                AccessNode::new(Role::Button)
                    .with_name("Save")
                    .with_action_target("passwords.save"),
            ]);
            if self.editing.is_some() {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Delete item")
                        .with_action_target("passwords.delete"),
                );
            }
        } else if self.mode == ShowcaseMode::AppAccess {
            let grants = self
                .app_grants
                .iter()
                .enumerate()
                .map(|(index, grant)| {
                    let account = self
                        .items
                        .iter()
                        .find(|item| item.id == grant.item_id)
                        .map_or("Saved account", |item| item.title.as_str());
                    AccessNode::new(Role::Group)
                        .with_name(&grant.app_id)
                        .with_description(format!(
                            "Can use {account} for {}",
                            grant.origin.as_deref().unwrap_or("this saved account")
                        ))
                        .with_children(vec![
                            AccessNode::new(Role::Button)
                                .with_name(format!("Revoke {} access", grant.app_id))
                                .with_action_target(format!("passwords.app-access.revoke:{index}")),
                        ])
                })
                .collect();
            children.push(
                AccessNode::new(Role::List)
                    .with_name("Apps with ongoing access")
                    .with_children(grants),
            );
        } else if let Some(detail) = detail {
            children.push(detail);
        }
        if let Some(dialog) = &self.transfer_dialog {
            let controls = match dialog {
                TransferDialog::ImportPreview { rows, conflicts } => vec![
                    AccessNode::new(Role::StaticText).with_name(format!(
                        "{} logins found; {conflicts} matching logins will be skipped",
                        rows.len()
                    )),
                    AccessNode::new(Role::Button)
                        .with_name("Cancel import")
                        .with_action_target("passwords.transfer.cancel"),
                    AccessNode::new(Role::Button)
                        .with_name("Import logins")
                        .with_action_target("passwords.transfer.import"),
                ],
                TransferDialog::ExportPlaintext { acknowledged } => vec![
                    AccessNode::new(Role::Checkbox)
                        .with_name("I understand this file is plaintext")
                        .with_action_target("passwords.transfer.ack")
                        .with_state(AccessState::new().focusable(true).checked(*acknowledged)),
                    AccessNode::new(Role::Button)
                        .with_name("Cancel export")
                        .with_action_target("passwords.transfer.cancel"),
                    AccessNode::new(Role::Button)
                        .with_name("Choose export file")
                        .with_action_target("passwords.transfer.choose")
                        .with_state(AccessState::new().focusable(true).disabled(!acknowledged)),
                ],
                TransferDialog::DeleteConfirm { title, .. } => vec![
                    AccessNode::new(Role::StaticText).with_name(format!(
                        "{title} will be permanently removed from your vault"
                    )),
                    AccessNode::new(Role::Button)
                        .with_name("Keep item")
                        .with_action_target("passwords.transfer.cancel"),
                    AccessNode::new(Role::Button)
                        .with_name("Delete permanently")
                        .with_action_target("passwords.delete.confirm"),
                ],
                TransferDialog::Backup => vec![
                    AccessNode::new(Role::StaticText)
                        .with_name("Encrypted locally; preserves every item and custom field"),
                    self.backup_password
                        .accessibility()?
                        .with_action_target("passwords.backup.password"),
                    self.backup_confirmation
                        .accessibility()?
                        .with_action_target("passwords.backup.confirmation"),
                    AccessNode::new(Role::Button)
                        .with_name("Cancel backup")
                        .with_action_target("passwords.transfer.cancel"),
                    AccessNode::new(Role::Button)
                        .with_name("Choose backup file")
                        .with_action_target("passwords.transfer.backup"),
                ],
                TransferDialog::Restore { .. } => vec![
                    self.backup_password
                        .accessibility()?
                        .with_action_target("passwords.backup.password"),
                    AccessNode::new(Role::Button)
                        .with_name("Cancel restore")
                        .with_action_target("passwords.transfer.cancel"),
                    AccessNode::new(Role::Button)
                        .with_name("Restore vault items")
                        .with_action_target("passwords.transfer.restore"),
                ],
            };
            children = vec![
                AccessNode::new(Role::Dialog)
                    .with_name(match dialog {
                        TransferDialog::ImportPreview { .. } => "Review import",
                        TransferDialog::ExportPlaintext { .. } => "Export as plaintext CSV",
                        TransferDialog::DeleteConfirm { .. } => "Delete item?",
                        TransferDialog::Backup => "Back up your vault",
                        TransferDialog::Restore { .. } => "Unlock backup",
                    })
                    .with_state(AccessState::new().modal(true))
                    .with_children(controls),
            ];
        }
        if !self.status.is_empty() {
            children.push(AccessNode::new(Role::StaticText).with_name(&self.status));
        }
        let mut root = AccessNode::new(Role::Window)
            .with_name("Passwords")
            .with_children(children);
        normalize_sibling_keys(&mut root, "passwords");
        Some(root)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per semantic action target"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (doc, _) = ui::doc::document(state, path);
        match (target, action) {
            ("passwords.navigation.toggle", Action::Activate) => self.drawer.toggle(),
            ("passwords.add", Action::Activate) => self.begin_add(),
            ("passwords.filter.all", Action::Activate) => self.select_filter(doc, VaultFilter::All),
            ("passwords.filter.favorites", Action::Activate) => {
                self.select_filter(doc, VaultFilter::Favorites);
            }
            ("passwords.filter.logins", Action::Activate) => {
                self.select_filter(doc, VaultFilter::Kind(SecretItemKind::Login));
            }
            ("passwords.filter.api-keys", Action::Activate) => {
                self.select_filter(doc, VaultFilter::Kind(SecretItemKind::ApiKey));
            }
            ("passwords.filter.notes", Action::Activate) => {
                self.select_filter(doc, VaultFilter::Kind(SecretItemKind::SecureNote));
            }
            ("passwords.filter.wifi", Action::Activate) => {
                self.select_filter(doc, VaultFilter::Kind(SecretItemKind::Wifi));
            }
            ("passwords.filter.other", Action::Activate) => {
                self.select_filter(doc, VaultFilter::Kind(SecretItemKind::Other));
            }
            (target, Action::Activate) if target.starts_with("passwords.favorite:") => {
                let Some(index) = target
                    .strip_prefix("passwords.favorite:")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    return false;
                };
                self.toggle_favorite(index);
            }
            ("passwords.back", Action::Activate) => self.mobile_detail = false,
            ("passwords.review", Action::Activate) => {
                self.mode = ShowcaseMode::Health;
                self.mobile_detail = false;
            }
            ("passwords.app-access", Action::Activate) => {
                self.mode = ShowcaseMode::AppAccess;
                self.mobile_detail = false;
            }
            (target, Action::Activate) if target.starts_with("passwords.app-access.revoke:") => {
                let Some(index) = target
                    .strip_prefix("passwords.app-access.revoke:")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    return false;
                };
                let Some(grant) = self.app_grants.get(index).cloned() else {
                    return false;
                };
                self.revoke_app_grant_index(index, &grant, self.current_panel_rect());
            }
            ("passwords.import", Action::Activate) => self.request_import(),
            ("passwords.export", Action::Activate) => self.request_export_confirmation(),
            ("passwords.backup", Action::Activate) => self.request_backup(),
            ("passwords.restore", Action::Activate) => self.request_restore(),
            ("passwords.transfer.cancel", Action::Activate) => {
                self.transfer_dialog = None;
                self.clear_backup_form();
            }
            (
                "passwords.backup.password" | "passwords.backup.confirmation",
                Action::SetValue(_) | Action::Focus,
            ) => {
                let confirming = target == "passwords.backup.confirmation";
                if !matches!(self.transfer_dialog, Some(TransferDialog::Backup))
                    && (confirming
                        || !matches!(self.transfer_dialog, Some(TransferDialog::Restore { .. })))
                {
                    return false;
                }
                self.backup_focus = usize::from(confirming);
                self.backup_password.set_focus(!confirming);
                self.backup_confirmation.set_focus(confirming);
                if let Action::SetValue(value) = action {
                    let field = if confirming {
                        &mut self.backup_confirmation
                    } else {
                        &mut self.backup_password
                    };
                    field.set_text(value);
                }
            }
            ("passwords.transfer.backup", Action::Activate) => self.begin_encrypted_backup(),
            ("passwords.transfer.restore", Action::Activate) => self.apply_encrypted_restore(),
            ("passwords.transfer.import", Action::Activate) => self.apply_import(),
            ("passwords.transfer.ack", Action::Activate | Action::ToggleCheck) => {
                if let Some(TransferDialog::ExportPlaintext { acknowledged }) =
                    &mut self.transfer_dialog
                {
                    *acknowledged = !*acknowledged;
                } else {
                    return false;
                }
            }
            ("passwords.transfer.choose", Action::Activate) => {
                if !matches!(
                    self.transfer_dialog,
                    Some(TransferDialog::ExportPlaintext { acknowledged: true })
                ) {
                    return false;
                }
                self.begin_plaintext_export();
            }
            ("passwords.search", Action::Focus) => {
                self.active_field = Some(ActiveField::Search);
                Widget::set_focus(&mut self.search_input, true);
            }
            ("passwords.search", Action::SetValue(value)) => {
                self.commit_view(
                    doc,
                    &crate::resource::PasswordsAction::SetSearch {
                        value: value.clone(),
                    },
                );
            }
            ("passwords.generate", Action::Activate) => self.generate_password(),
            ("passwords.cancel", Action::Activate) => self.cancel_edit(),
            ("passwords.save", Action::Activate) => self.save_draft(),
            ("passwords.delete", Action::Activate) => {
                let Some(id) = self.editing.clone() else {
                    return false;
                };
                self.request_delete(id);
            }
            ("passwords.delete.confirm", Action::Activate) => self.confirm_delete(),
            (_, Action::Activate) if target.starts_with("passwords.kind:") => {
                let kind = match target.strip_prefix("passwords.kind:") {
                    Some("login") => SecretItemKind::Login,
                    Some("api-key") => SecretItemKind::ApiKey,
                    Some("note") => SecretItemKind::SecureNote,
                    Some("wifi") => SecretItemKind::Wifi,
                    Some("other") => SecretItemKind::Other,
                    _ => return false,
                };
                self.select_kind(kind);
            }
            (_, Action::Focus) if target.starts_with("passwords.field.") => {
                self.active_field = match target.strip_prefix("passwords.field.") {
                    Some("name") => Some(ActiveField::Name),
                    Some("username") => Some(ActiveField::Username),
                    Some("origin") => Some(ActiveField::Origin),
                    Some("password") => Some(ActiveField::Password),
                    Some("notes") => Some(ActiveField::Notes),
                    Some("tags") => Some(ActiveField::Tags),
                    Some("custom-fields") => Some(ActiveField::CustomFields),
                    _ => return false,
                };
                if let Some(field) = self.active_field {
                    self.ensure_editor_field_visible(self.current_panel_rect(), field);
                }
            }
            (_, Action::SetValue(value)) if target.starts_with("passwords.field.") => {
                let field: &mut String = match target.strip_prefix("passwords.field.") {
                    Some("name") => &mut self.draft_title,
                    Some("username") => &mut self.draft_username,
                    Some("origin") => &mut self.draft_origin,
                    Some("password") => &mut self.draft_password,
                    Some("notes") => &mut self.draft_notes,
                    Some("tags") => &mut self.draft_tags,
                    Some("custom-fields") => &mut self.draft_custom_fields,
                    _ => return false,
                };
                field.clone_from(value);
                self.sync_editor_inputs();
            }
            (_, Action::Activate) if target.starts_with("passwords.select:") => {
                if let Some(index) = target
                    .strip_prefix("passwords.select:")
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    let index = index.min(self.items.len().saturating_sub(1));
                    self.select_index(doc, index);
                    self.ensure_selected_visible(self.current_list_rect());
                } else {
                    return false;
                }
            }
            (_, Action::Activate) if target.starts_with("passwords.edit:") => {
                let Some(id) = target.strip_prefix("passwords.edit:") else {
                    return false;
                };
                let Some(index) = self.items.iter().position(|item| item.id.0 == id) else {
                    return false;
                };
                self.select_index(doc, index);
                self.begin_edit();
            }
            (_, Action::Activate) if target.starts_with("passwords.copy:") => {
                let Some(id) = target.strip_prefix("passwords.copy:") else {
                    return false;
                };
                let Some(index) = self.items.iter().position(|item| item.id.0 == id) else {
                    return false;
                };
                self.select_index(doc, index);
                self.copy_selected();
            }
            (_, Action::Activate) if target.starts_with("passwords.copy-origin:") => {
                let Some(id) = target.strip_prefix("passwords.copy-origin:") else {
                    return false;
                };
                let Some(index) = self.items.iter().position(|item| item.id.0 == id) else {
                    return false;
                };
                self.select_index(doc, index);
                self.copy_origin_selected();
            }
            (_, Action::Activate) if target.starts_with("passwords.reveal:") => {
                let Some(id) = target.strip_prefix("passwords.reveal:") else {
                    return false;
                };
                let Some(index) = self.items.iter().position(|item| item.id.0 == id) else {
                    return false;
                };
                self.select_index(doc, index);
                self.reveal_selected();
            }
            _ => return false,
        }
        true
    }
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let pending = self.import_portal.is_some()
            || self.import_parse.is_some()
            || self.export_portal.is_some()
            || self.backup_portal.is_some()
            || self.restore_portal.is_some();
        let transfer = if pending {
            // Keep the deadline fixed until sync consumes the tick. Returning
            // now + 16 ms on every query would postpone it indefinitely.
            let deadline = self.transfer_poll_deadline.get().unwrap_or_else(|| {
                ui::time::now()
                    .checked_add(std::time::Duration::from_millis(16))
                    .unwrap_or_else(ui::time::now)
            });
            self.transfer_poll_deadline.set(Some(deadline));
            Some(deadline)
        } else {
            self.transfer_poll_deadline.set(None);
            None
        };
        [
            transfer,
            self.revealed.as_ref().map(|(_, _, deadline)| *deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.transfer_dialog.as_ref().map(|_| self.transfer_rect())
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::resources::PublishesResources for PasswordsSurface {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the vault door — keep only live item / session extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<ui::tilo_actions::Resource> {
        if self.is_hosted() && id == crate::actions::VAULT_RESOURCE_ID {
            return None;
        }
        fn find(
            resources: &[ui::tilo_actions::Resource],
            id: &str,
        ) -> Option<ui::tilo_actions::Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.publish_resources(doc), id)
    }
}

fn origins_match(item: &SecretItem, candidate: &str) -> bool {
    if candidate.trim().is_empty() {
        return item.origins.is_empty();
    }
    let mut probe = SecretItem::login("", "", "");
    probe.add_origin(candidate).is_ok()
        && probe
            .origins
            .iter()
            .next()
            .is_some_and(|origin| item.origins.contains(origin))
}

fn demo_items() -> Vec<SecretItem> {
    let data = [
        ("Northstar Bank", "ada@example.com", "https://bank.example"),
        ("Personal mail", "ada@example.com", "https://mail.example"),
        ("Cloud Console", "ada.l", "https://console.example"),
        ("Design Library", "ada", "https://design.example"),
        ("Home Wi-Fi", "Ada Home", "https://router.local"),
        ("Recovery codes", "Note", "https://accounts.example"),
    ];
    data.into_iter()
        .enumerate()
        .map(|(i, (title, user, origin))| {
            let password = if i < 2 {
                "never-rendered-Shared9!".to_string()
            } else {
                format!("never-rendered-Unique{i}A!")
            };
            let mut item = SecretItem::login(title, user, password);
            item.id = accounts::SecretId(format!("demo-{i}"));
            item.kind = match i {
                2 => SecretItemKind::ApiKey,
                3 => SecretItemKind::Other,
                4 => SecretItemKind::Wifi,
                5 => SecretItemKind::SecureNote,
                _ => SecretItemKind::Login,
            };
            if i == 0 || i == 2 {
                item.tags.insert("favorite".into());
            }
            if item.kind == SecretItemKind::Wifi {
                item.fields.insert("network".into(), "Ada Home".into());
            }
            if item.kind == SecretItemKind::SecureNote {
                item.password.clear();
            }
            let _ = item.add_origin(origin);
            if i == 0 {
                let _ = item.add_origin("https://login.bank.example");
            }
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

    #[expect(
        clippy::float_cmp,
        reason = "layout offsets are sums of exactly representable constants"
    )]
    #[test]
    fn vault_layout_fits_desktop_phone_and_large_text_viewports() {
        eprintln!("phase list_capacity_counts_whole_rows_and_never_goes_negative");
        {
            let capacity =
                |height: f64| PasswordsSurface::list_capacity(Rect::new(0.0, 0.0, 400.0, height));
            assert_eq!(capacity(0.0), 0);
            assert_eq!(capacity(168.0), 0);
            assert_eq!(capacity(168.0 + 75.9), 0);
            assert_eq!(capacity(168.0 + 76.0), 1);
            assert_eq!(capacity(168.0 + 3.0 * 76.0 + 10.0), 3);
            assert_eq!(capacity(f64::MAX), usize::MAX);
        }

        eprintln!("phase phone_list_capacity_uses_every_fully_visible_row");
        {
            let list = Rect::new(76.0, 24.0, 366.0, 820.0);
            assert_eq!(PasswordsSurface::list_capacity(list), 8);
        }

        eprintln!("phase desktop_columns_are_disjoint_and_inside_viewport");
        {
            let content = Rect::new(24.0, 24.0, 1416.0, 876.0);
            let sidebar = Rect::new(content.x0, content.y0, content.x0 + 190.0, content.y1);
            let list = Rect::new(
                sidebar.x1 + 24.0,
                content.y0 + 4.0,
                sidebar.x1 + 354.0,
                content.y1,
            );
            let detail = Rect::new(list.x1 + 24.0, content.y0, content.x1, content.y1);
            assert!(sidebar.x1 < list.x0 && list.x1 < detail.x0);
            assert!(detail.x1 <= content.x1 && sidebar.y0 >= content.y0 && detail.y1 <= content.y1);
            assert!(detail.width() >= 480.0);
        }

        eprintln!("phase shared_drawer_uses_overlay_on_compact_viewports");
        {
            let content = Rect::new(24.0, 24.0, 496.0, 796.0);
            let mut drawer = SidebarDrawer::new()
                .docked_width(214.0)
                .overlay_width(292.0)
                .breakpoint(960.0);
            let main = drawer.resolve(content);
            assert!(drawer.is_overlay());
            assert!(!drawer.is_open());
            assert_eq!(main, content);
            assert_eq!(drawer.scrim_rect(), Rect::ZERO);
        }

        eprintln!("phase large_text_uses_the_compact_single_pane_layout");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let content = Rect::new(24.0, 24.0, 1256.0, 776.0);
            app.adapt_layout_for_text_scale(&Theme::dark().with_text_scale(2.0));
            let main = app.resolve_drawer(content);

            assert!(app.compact_for_large_text);
            assert!(app.drawer.is_overlay());
            assert_eq!(main, content);
            assert_eq!(compact_content_rect(app.mode, main).x0, content.x0 + 60.0);
        }

        eprintln!("phase phone_review_and_editor_keep_full_content_width");
        {
            let main = Rect::new(24.0, 24.0, 366.0, 820.0);
            assert_eq!(compact_content_rect(ShowcaseMode::Health, main), main);
            assert_eq!(compact_content_rect(ShowcaseMode::Editor, main), main);
            assert_eq!(
                compact_content_rect(ShowcaseMode::Vault, main),
                Rect::new(84.0, 24.0, 366.0, 820.0)
            );
        }
    }

    #[expect(
        clippy::float_cmp,
        reason = "scroll offsets are clamped to exactly these values, not computed approximately"
    )]
    #[test]
    fn sidebar_list_and_app_access_scroll_state_clamps_and_follows_selection() {
        eprintln!("phase short_phone_sidebar_retains_and_clamps_real_scroll_state");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let content = Rect::new(24.0, 24.0, 366.0, 244.0);
            app.drawer.resolve(content);
            app.drawer.open();
            app.drawer.snap(true);
            app.drawer.resolve(content);

            assert!(app.scroll_sidebar_by(f64::MAX));
            let nav = app.sidebar_nav(app.sidebar_viewport());
            assert!(nav.max_scroll_offset() > 0.0);
            assert_eq!(app.sidebar_scroll, nav.max_scroll_offset());
            assert!(
                !app.scroll_sidebar_by(1.0),
                "scrolling beyond the end is inert"
            );

            assert!(app.scroll_sidebar_by(-f64::MAX));
            assert_eq!(app.sidebar_scroll, 0.0);
        }

        eprintln!("phase long_vault_list_scrolls_clamps_and_keeps_keyboard_selection_visible");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let seed = app.items[0].clone();
            for index in 0..18 {
                let mut item = seed.clone();
                item.id = SecretId(format!("overflow-{index}"));
                item.title = format!("Overflow item {index}");
                app.items.push(item);
            }
            let list = Rect::new(84.0, 24.0, 366.0, 430.0);
            assert_eq!(PasswordsSurface::list_capacity(list), 3);
            assert!(app.scroll_list_to(list, usize::MAX));
            assert_eq!(app.list_scroll, app.items.len() - 3);
            assert!(!app.scroll_list_to(list, usize::MAX));

            app.selected = app.items.len() - 1;
            app.ensure_selected_visible(list);
            assert_eq!(app.list_scroll, app.items.len() - 3);
            app.selected = 0;
            app.ensure_selected_visible(list);
            assert_eq!(app.list_scroll, 0);

            let mut doc = AutoCommit::new();
            app.select_filter(&mut doc, VaultFilter::Favorites);
            assert_eq!(
                app.list_scroll, 0,
                "changing views starts at the first result"
            );
        }

        eprintln!("phase app_access_is_scrollable_revocable_and_assistive_on_a_phone");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-access-ui-{}-{}",
                std::process::id(),
                ui::time::wall()
                    .duration_since(ui::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut store = accounts::SecretStore::open_in(&root);
            let mut item = SecretItem::login("Northstar Bank", "ada", "not-rendered");
            item.add_origin("https://bank.example").unwrap();
            store.put(&mut item).unwrap();
            let mut broker = accounts::CredentialBroker::new(store);
            for index in 0..10 {
                broker
                    .request(
                        &accounts::AccessRequest {
                            app_id: format!("org.example.App{index}"),
                            item_id: item.id.clone(),
                            purpose: "Sign in".into(),
                            origin: Some("https://bank.example".into()),
                        },
                        |_, _| accounts::AccessDecision::AllowAlways,
                    )
                    .unwrap();
            }
            drop(broker);

            let mut app = PasswordsSurface::demo(ShowcaseMode::AppAccess);
            app.model = Some(crate::PasswordsModel::open_in(&root));
            app.refresh();
            app.rect = Rect::new(0.0, 0.0, 390.0, 640.0);
            app.drawer.resolve(Rect::new(24.0, 24.0, 366.0, 616.0));
            let panel = app.current_panel_rect();
            assert_eq!(app.app_grants.len(), 10);
            assert!(app.max_app_access_scroll(panel) > 0.0);
            assert!(app.scroll_app_access_by(panel, f64::MAX));
            assert_eq!(app.app_access_scroll, app.max_app_access_scroll(panel));

            let json = serde_json::to_string(&Widget::accessibility(&app)).unwrap();
            assert!(json.contains("Apps with ongoing access"));
            assert!(json.contains("passwords.app-access.revoke:9"));
            assert!(!json.contains("not-rendered"));

            let grant = app.app_grants[9].clone();
            assert!(app.revoke_app_grant_index(9, &grant, panel));
            assert_eq!(app.app_grants.len(), 9);
            assert_eq!(crate::PasswordsModel::open_in(&root).app_grants().len(), 9);
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[expect(
        clippy::float_cmp,
        reason = "scroll offsets are clamped to exactly these values, not computed approximately"
    )]
    #[test]
    fn editor_controls_stay_disjoint_and_fields_scroll_behind_fixed_actions() {
        eprintln!("phase phone_editor_uses_full_width_and_keeps_controls_disjoint");
        {
            let app = PasswordsSurface::demo(ShowcaseMode::Editor);
            let panel = Rect::new(24.0, 24.0, 366.0, 820.0);
            let segments = app.kind_segments(panel);
            assert_eq!(segments.len(), 5);
            assert!(segments.iter().all(|rect| rect.width() >= 44.0));
            assert!(
                segments
                    .iter()
                    .all(|rect| (rect.y0 - segments[0].y0).abs() < f64::EPSILON),
                "390px-class phones keep secret types on one row"
            );
            assert!(segments.windows(2).all(|pair| pair[0].x1 < pair[1].x0));

            let step = PasswordsSurface::editor_field_step(panel);
            assert!(step >= 64.0, "field labels must clear the previous input");
            let first_input_bottom = app.editor_fields_top(panel) + 64.0;
            let next_label_top = app.editor_fields_top(panel) + step;
            assert!(first_input_bottom <= next_label_top);

            let (delete, cancel, save) = app.editor_footer_rects(panel);
            let delete = delete.expect("editing exposes delete");
            assert!(delete.width() >= 44.0 && cancel.width() >= 44.0 && save.width() >= 44.0);
            assert!(
                delete.width() <= 96.0,
                "delete must remain a secondary action"
            );
            assert!(delete.x1 < cancel.x0 && cancel.x1 < save.x0);
        }

        eprintln!("phase minimum_width_editor_actions_do_not_overlap");
        {
            let app = PasswordsSurface::demo(ShowcaseMode::Editor);
            let panel = Rect::new(24.0, 24.0, 296.0, 736.0);
            let (delete, cancel, save) = app.editor_footer_rects(panel);
            let delete = delete.expect("editing exposes delete");
            assert!(delete.width() >= 44.0 && cancel.width() >= 44.0 && save.width() >= 44.0);
            assert!(delete.x1 < cancel.x0 && cancel.x1 < save.x0);
            assert!(delete.x0 >= panel.x0 && save.x1 <= panel.x1);
        }

        eprintln!("phase short_phone_editor_scrolls_every_field_behind_a_fixed_action_bar");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Editor);
            let panel = Rect::new(24.0, 24.0, 366.0, 496.0);
            let (_, cancel_before, save_before) = app.editor_footer_rects(panel);
            let maximum = app.max_editor_scroll(panel);
            assert!(maximum > 0.0);

            assert!(app.scroll_editor_by(panel, f64::MAX));
            assert_eq!(app.editor_scroll, maximum);
            let (_, cancel_after, save_after) = app.editor_footer_rects(panel);
            assert_eq!(cancel_before, cancel_after);
            assert_eq!(save_before, save_after);
            assert!(app.editor_note_y(panel) < app.editor_body_viewport(panel).y1);
            assert!(
                app.editor_custom_fields_y(panel) + 108.0
                    <= app.editor_body_viewport(panel).y1 + f64::EPSILON,
                "the last custom-field row must scroll fully above the fixed actions"
            );

            assert!(app.scroll_editor_by(panel, -f64::MAX));
            assert_eq!(app.editor_scroll, 0.0);
            assert!(!app.scroll_editor_by(panel, -1.0));
        }

        eprintln!("phase short_editor_focus_scrolls_fields_into_view_and_restores_the_top");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Editor);
            let panel = Rect::new(24.0, 24.0, 366.0, 496.0);

            app.ensure_editor_field_visible(panel, ActiveField::CustomFields);
            assert!(app.editor_scroll > 0.0);
            let custom_bottom = app.editor_custom_fields_y(panel) + 104.0;
            assert!(custom_bottom <= app.editor_body_viewport(panel).y1 + f64::EPSILON);

            app.ensure_editor_field_visible(panel, ActiveField::Name);
            assert_eq!(app.editor_scroll, 0.0);
            assert!(app.kind_segments(panel)[0].y0 >= app.editor_body_viewport(panel).y0);
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn assistive_and_sidebar_actions_drive_the_real_surface() {
        eprintln!("phase shared_sidebar_routes_to_real_review_and_back");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let content = Rect::new(24.0, 24.0, 1416.0, 876.0);
            app.drawer.resolve(content);
            let side = app.drawer.sidebar_rect();
            let mut doc = AutoCommit::new();
            assert!(app.activate_sidebar(&mut doc, Point::new(side.x0 + 20.0, side.y0 + 453.0)));
            assert_eq!(app.mode, ShowcaseMode::Health);
            assert!(app.activate_sidebar(&mut doc, Point::new(side.x0 + 20.0, side.y0 + 76.0)));
            assert_eq!(app.mode, ShowcaseMode::Vault);
        }

        eprintln!("phase storage_status_is_available_to_assistive_clients");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.status = "Vault unavailable; recovery required".into();
            let tree = app.accessibility().unwrap();
            assert!(
                tree.children
                    .iter()
                    .any(|node| { node.name.as_deref() == Some(app.status.as_str()) })
            );
        }

        eprintln!("phase assistive_actions_drive_the_same_editor");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let mut doc = AutoCommit::new();
            assert!(app.perform_semantic_action(
                "passwords.add",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(app.mode, ShowcaseMode::Editor);
            assert!(app.perform_semantic_action(
                "passwords.field.name",
                &ui::a11y::Action::SetValue("New name".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(app.draft_title, "New name");

            for (target, expected) in [
                ("passwords.kind:api-key", SecretItemKind::ApiKey),
                ("passwords.kind:note", SecretItemKind::SecureNote),
                ("passwords.kind:wifi", SecretItemKind::Wifi),
                ("passwords.kind:other", SecretItemKind::Other),
                ("passwords.kind:login", SecretItemKind::Login),
            ] {
                assert!(app.perform_semantic_action(
                    target,
                    &ui::a11y::Action::Activate,
                    &mut doc,
                    &automerge::ROOT,
                ));
                assert_eq!(app.draft_kind, expected);
            }
        }

        eprintln!("phase backup_and_restore_fields_share_protected_semantic_entry");
        {
            use ui::a11y::Action;
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let mut doc = AutoCommit::new();
            for restoring in [false, true] {
                app.transfer_dialog = Some(if restoring {
                    TransferDialog::Restore {
                        bytes: Zeroizing::new(Vec::new()),
                    }
                } else {
                    TransferDialog::Backup
                });
                let tree = app.accessibility().unwrap();
                let controls = &tree.children[0].children;
                let password = controls
                    .iter()
                    .find(|node| node.name.as_deref() == Some("Backup password"))
                    .unwrap();
                assert!(password.state.protected);
                assert_eq!(password.value, None);
                assert!(app.perform_semantic_action(
                    "passwords.backup.password",
                    &Action::SetValue("Owned fixture password 52!".into()),
                    &mut doc,
                    &automerge::ROOT,
                ));
                assert_eq!(app.backup_password.value(), "Owned fixture password 52!");
                if !restoring {
                    assert!(app.perform_semantic_action(
                        "passwords.backup.confirmation",
                        &Action::SetValue("Owned fixture password 52!".into()),
                        &mut doc,
                        &automerge::ROOT,
                    ));
                    assert!(app.backup_form_error(true).is_none());
                }
                let tree = app.accessibility().unwrap();
                assert!(
                    tree.children[0]
                        .children
                        .iter()
                        .filter(|node| node.state.protected)
                        .all(|node| node.value.is_none())
                );
            }
            app.transfer_dialog = None;
            assert!(!app.perform_semantic_action(
                "passwords.backup.password",
                &Action::SetValue("outside dialog".into()),
                &mut doc,
                &automerge::ROOT,
            ));
        }
    }

    #[test]
    fn surface_resources_and_accessibility_never_expose_secret_values() {
        eprintln!("phase demo_inventory_contains_no_renderable_secret");
        {
            use std::fmt::Write as _;
            let items = demo_items();
            assert!(items.iter().all(
                |item| item.password.is_empty() || item.password.starts_with("never-rendered-")
            ));
            let visible = items.iter().fold(String::new(), |mut text, item| {
                write!(text, "{} {} {:?}", item.title, item.username, item.origins).unwrap();
                text
            });
            assert!(!visible.contains("never-rendered"));
        }

        eprintln!("phase accessibility_never_exposes_password_values");
        {
            let app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let json = serde_json::to_string(&Widget::accessibility(&app)).unwrap();
            assert!(!json.contains("never-rendered"));
            assert!(json.contains("Reveal for ten seconds"));
        }

        eprintln!("phase cached_surface_and_resources_exclude_notes_and_custom_values");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-surface-redaction-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = crate::PasswordsModel::open_in(&root);
            let mut item = SecretItem::login("Private account", "ada", "password-value");
            item.notes = "note-value".into();
            item.fields
                .insert("recovery code".into(), "field-value".into());
            model.save_item(&mut item).unwrap();

            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(model);
            app.refresh();
            assert!(app.items[0].password.is_empty());
            assert!(app.items[0].notes.is_empty());
            assert!(app.items[0].fields.is_empty());
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            let public = resources
                .iter()
                .map(|resource| {
                    let mut description =
                        format!("{} {} {}", resource.id.0, resource.kind, resource.label);
                    for action in &resource.actions {
                        description.push_str(&action.label);
                        let encoded = action
                            .build_args(Vec::<(String, ui::tilo_actions::ArgValue)>::new())
                            .unwrap();
                        description.push_str(&String::from_utf8_lossy(&encoded.payload));
                    }
                    description
                })
                .collect::<String>();
            for secret in [
                "password-value",
                "note-value",
                "field-value",
                "Private account",
            ] {
                assert!(
                    !public.contains(secret),
                    "public resource leaked {secret:?}"
                );
            }
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn filters_favorites_and_confirmed_deletes_are_real_vault_state() {
        eprintln!("phase type_filters_and_favorites_are_real_vault_state");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-favorites-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = crate::PasswordsModel::open_in(&root);
            let mut login = SecretItem::login("Account", "ada", "secret-login");
            model.save_item(&mut login).unwrap();
            let mut note = SecretItem::login("Codes", "", "secret-note");
            note.kind = SecretItemKind::SecureNote;
            model.save_item(&mut note).unwrap();

            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(model);
            app.refresh();
            let mut doc = AutoCommit::new();
            app.select_filter(&mut doc, VaultFilter::Kind(SecretItemKind::SecureNote));
            assert_eq!(app.visible_indices().len(), 1);
            assert_eq!(app.items[app.visible_indices()[0]].title, "Codes");

            let login_index = app
                .items
                .iter()
                .position(|item| item.id == login.id)
                .unwrap();
            app.toggle_favorite(login_index);
            app.select_filter(&mut doc, VaultFilter::Favorites);
            assert_eq!(app.visible_indices(), vec![login_index]);
            let reopened = crate::PasswordsModel::open_in(&root);
            assert!(
                reopened
                    .items()
                    .iter()
                    .find(|item| item.id == login.id)
                    .unwrap()
                    .tags
                    .contains("favorite")
            );
            let _ = std::fs::remove_dir_all(root);
        }

        eprintln!("phase every_delete_path_requires_confirmation_before_mutating_the_vault");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-delete-confirm-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = crate::PasswordsModel::open_in(&root);
            let mut item = SecretItem::login("Keep me", "ada", "private-value");
            let id = model.save_item(&mut item).unwrap();
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(model);
            app.refresh();
            app.begin_edit();
            let mut doc = AutoCommit::new();

            assert!(app.perform_semantic_action(
                "passwords.delete",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(matches!(
                app.transfer_dialog,
                Some(TransferDialog::DeleteConfirm { .. })
            ));
            assert_eq!(app.model.as_ref().unwrap().items().len(), 1);
            app.transfer_dialog = None;

            let payload =
                serde_json::to_vec(&crate::actions::PasswordOperation::RequestDelete { id: id.0 })
                    .unwrap();
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "tilo.passwords.operation",
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .map(|started| started.is_some())
                .unwrap()
            );
            assert_eq!(app.model.as_ref().unwrap().items().len(), 1);

            assert!(app.perform_semantic_action(
                "passwords.delete.confirm",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(app.model.as_ref().unwrap().items().is_empty());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    // Clipboard transport is injected at the host boundary, scoped to the
    // test's thread. Keep these unit tests independent of the desktop; native
    // integration checks still exercise the Windows clipboard.
    struct TestClipboard {
        accepts: bool,
        text: std::sync::Mutex<Option<String>>,
    }

    impl ui::clipboard::ClipboardBackend for TestClipboard {
        fn set_text(&self, _target: ui::clipboard::ClipboardTarget, text: &str) -> bool {
            if !self.accepts {
                return false;
            }
            *self.text.lock().unwrap() = Some(text.into());
            true
        }
        fn text(&self, _target: ui::clipboard::ClipboardTarget) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
        fn name(&self) -> &'static str {
            "passwords-test-transport"
        }
    }

    fn clipboard_fixture(accepts: bool) -> ui::host::HostScope {
        let services =
            ui::host::HostServices::null().with_clipboard(std::sync::Arc::new(TestClipboard {
                accepts,
                text: std::sync::Mutex::new(Some("existing clipboard".into())),
            }));
        ui::host::enter(&services)
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn reveal_and_copy_are_temporary_and_honest_about_the_clipboard() {
        eprintln!("phase revealed_value_uses_a_deadline_wakeup_and_locks_on_focus_loss");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.revealed = Some((
                SecretId("demo-0".into()),
                Zeroizing::new("temporary-value".into()),
                ui::time::now() + std::time::Duration::from_secs(10),
            ));
            assert!(Widget::next_wake_deadline(&app).is_some());

            let mut doc = AutoCommit::new();
            let mut handled = false;
            let mut focus = None;
            let mut taffy = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            app.handle_event(
                &WindowEvent::Focused(false),
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut doc,
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(app.revealed.is_none());
        }

        eprintln!("phase pending_transfer_keeps_host_polling_and_reveal_has_an_expiry_deadline");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            let (tx, rx) = std::sync::mpsc::channel();
            app.export_portal = Some(rx);
            let first = app.next_wake_deadline().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert_eq!(app.next_wake_deadline(), Some(first));
            tx.send(ui::portal::PortalOutcome::Cancelled).unwrap();
            app.sync(&mut AutoCommit::new(), &automerge::ROOT);
            assert!(app.next_wake_deadline().is_none());
            let deadline = ui::time::now();
            app.revealed = Some((
                SecretId("test".into()),
                Zeroizing::new("fixture secret".into()),
                deadline,
            ));
            assert_eq!(app.next_wake_deadline(), Some(deadline));
            app.sync(&mut AutoCommit::new(), &automerge::ROOT);
            assert!(app.revealed.is_none());
            assert!(app.next_wake_deadline().is_none());
        }

        eprintln!("phase secret_row_has_distinct_copy_and_reveal_targets");
        {
            let _clipboard = clipboard_fixture(true);
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-secret-actions-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = crate::PasswordsModel::open_in(&root);
            let mut item = SecretItem::login("Account", "ada", "private-value");
            model.save_item(&mut item).unwrap();
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(model);
            app.refresh();

            app.activate_detail_row_at(1, 880.0, 1_000.0);
            assert!(app.status.starts_with("Copied password"));
            assert_eq!(ui::clipboard_paste().as_deref(), Some("private-value"));
            assert!(
                app.revealed.is_none(),
                "copying must not disclose the value"
            );

            app.activate_detail_row_at(1, 960.0, 1_000.0);
            assert!(
                app.revealed.is_some(),
                "the separate eye target reveals temporarily"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        eprintln!(
            "phase failed_copy_reports_error_without_claiming_or_replacing_clipboard_contents"
        );
        {
            let _clipboard = clipboard_fixture(false);
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-copy-failure-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = crate::PasswordsModel::open_in(&root);
            let mut item = SecretItem::login("Account", "ada", "private-value");
            item.add_origin("https://example.invalid").unwrap();
            model.save_item(&mut item).unwrap();
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(model);
            app.refresh();
            for copy in [
                PasswordsSurface::copy_selected,
                PasswordsSurface::copy_identity_selected,
                PasswordsSurface::copy_origin_selected,
            ] {
                copy(&mut app);
                assert_eq!(
                    app.status,
                    "Could not copy to the system clipboard. Try again."
                );
                assert_eq!(ui::clipboard_paste().as_deref(), Some("existing clipboard"));
                assert!(app.revealed.is_none());
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn editor_saves_edits_and_discards_drafts_against_the_real_store() {
        eprintln!("phase polished_surface_saves_searches_and_updates_real_store");
        {
            let _clipboard = clipboard_fixture(true);
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-surface-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(crate::PasswordsModel::open_in(&root));
            app.items.clear();
            app.begin_add();
            app.draft_title = "Example account".into();
            app.draft_username = "ada@example.com".into();
            app.draft_origin = "https://example.com, https://login.example.com".into();
            app.draft_password = Zeroizing::new("correct horse battery staple".into());
            app.draft_notes = Zeroizing::new("recovery details".into());
            app.sync_editor_inputs();
            app.save_draft();
            assert_eq!(app.mode, ShowcaseMode::Vault);
            assert!(app.draft_password.is_empty());
            assert!(app.draft_notes.is_empty());
            assert!(app.editor_inputs.password.value().is_empty());
            assert!(app.editor_inputs.notes.value().is_empty());
            assert_eq!(app.items.len(), 1);
            assert_eq!(app.items[0].title, "Example account");
            assert_eq!(app.items[0].origins.len(), 2);
            assert!(app.items[0].password.is_empty());

            app.query = "missing".into();
            app.refresh();
            assert!(app.items.is_empty());
            app.query = "example".into();
            app.refresh();
            assert_eq!(app.items.len(), 1);

            app.begin_edit();
            assert!(app.draft_origin.contains("https://example.com"));
            assert!(app.draft_origin.contains("https://login.example.com"));
            app.draft_title = "Renamed account".into();
            app.save_draft();
            assert_eq!(app.items[0].title, "Renamed account");
            assert_eq!(app.items[0].origins.len(), 2);
            app.copy_origin_selected();
            let copied = ui::clipboard_paste().unwrap();
            assert!(copied.contains("https://example.com"));
            assert!(copied.contains("https://login.example.com"));
            let _ = std::fs::remove_dir_all(root);
        }

        eprintln!("phase cancelling_discards_sensitive_shared_input_buffers");
        {
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.begin_add();
            app.draft_password = Zeroizing::new("unsaved password".into());
            app.draft_notes = Zeroizing::new("unsaved private note".into());
            app.sync_editor_inputs();

            app.cancel_edit();

            assert_eq!(app.mode, ShowcaseMode::Vault);
            assert!(app.draft_password.is_empty());
            assert!(app.draft_notes.is_empty());
            assert!(app.editor_inputs.password.value().is_empty());
            assert!(app.editor_inputs.notes.value().is_empty());
        }

        eprintln!("phase editing_preserves_app_specific_secret_fields");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-custom-fields-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let mut model = crate::PasswordsModel::open_in(&root);
            let mut item = SecretItem::login("Deployment", "ada", "token");
            item.kind = SecretItemKind::Other;
            item.fields.insert("client-id".into(), "app-42".into());
            item.fields.insert("tenant".into(), "northstar".into());
            item.tags.extend(["favorite".into(), "work".into()]);
            let id = model.save_item(&mut item).unwrap();

            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(model);
            app.refresh();
            app.begin_edit();
            assert!(app.draft_custom_fields.contains("client-id = app-42"));
            assert_eq!(app.draft_tags, "work");
            app.draft_title = "Production deployment".into();
            app.draft_custom_fields =
                Zeroizing::new("client-id = app-43\nenvironment = production".into());
            app.draft_tags = "work, infrastructure".into();
            app.save_draft();

            let stored = crate::PasswordsModel::open_in(&root)
                .items()
                .into_iter()
                .find(|candidate| candidate.id == id)
                .unwrap();
            assert_eq!(
                stored.fields.get("client-id").map(String::as_str),
                Some("app-43")
            );
            assert_eq!(
                stored.fields.get("environment").map(String::as_str),
                Some("production")
            );
            assert!(!stored.fields.contains_key("tenant"));
            assert!(stored.tags.contains("favorite"));
            assert!(stored.tags.contains("work"));
            assert!(stored.tags.contains("infrastructure"));
            let _ = std::fs::remove_dir_all(root);
        }

        eprintln!("phase custom_field_parser_is_helpful_and_rejects_ambiguous_rows");
        {
            assert_eq!(
                PasswordsSurface::parse_custom_fields("Account = Ada\nRegion: eu-west")
                    .unwrap()
                    .len(),
                2
            );
            assert!(PasswordsSurface::parse_custom_fields("missing separator").is_err());
            assert!(PasswordsSurface::parse_custom_fields("Token = one\ntoken = two").is_err());
            assert!(PasswordsSurface::parse_custom_fields("network = reserved").is_err());
            let escaped = PasswordsSurface::escape_custom_value("first\nsecond\\third");
            assert_eq!(escaped, "first\\nsecond\\\\third");
            assert_eq!(
                PasswordsSurface::unescape_custom_value(&escaped).unwrap(),
                "first\nsecond\\third"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn import_export_and_backup_flows_round_trip_through_portals() {
        eprintln!("phase portal_descriptor_import_previews_conflicts_before_writing");
        {
            use std::io::Write;
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-import-store-{}",
                std::process::id()
            ));
            let csv_path = std::env::temp_dir()
                .join(format!("tilo-passwords-import-{}.csv", std::process::id()));
            let mut selected = std::fs::File::create(&csv_path).unwrap();
            selected
                .write_all(
                    b"name,url,username,password\nExample,https://example.com,ada,first-secret\n",
                )
                .unwrap();
            drop(selected);
            let selected = std::fs::File::open(&csv_path).unwrap();
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(crate::PasswordsModel::open_in(&root));
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_open_file(ui::portal::OpenFileOutcome::Opened(
                ui::portal::OpenedFile::new(selected, "passwords.csv"),
            ));
            ui::host::scoped(&services, || app.request_import());
            assert_eq!(recorder.pick_requests()[0].title(), "Import passwords");
            let deadline = ui::time::now() + HANG_BOUND;
            while ui::time::now() < deadline {
                app.poll_transfers();
                if app.transfer_dialog.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(
                matches!(app.transfer_dialog, Some(TransferDialog::ImportPreview { ref rows, conflicts: 0 }) if rows.len() == 1)
            );
            app.apply_import();
            assert_eq!(app.model.as_ref().unwrap().items().len(), 1);

            let selected = std::fs::File::open(&csv_path).unwrap();
            recorder.respond_to_open_file(ui::portal::OpenFileOutcome::Opened(
                ui::portal::OpenedFile::new(selected, "passwords.csv"),
            ));
            ui::host::scoped(&services, || app.request_import());
            let deadline = ui::time::now() + HANG_BOUND;
            while ui::time::now() < deadline {
                app.poll_transfers();
                if app.transfer_dialog.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(matches!(
                app.transfer_dialog,
                Some(TransferDialog::ImportPreview { conflicts: 1, .. })
            ));
            app.apply_import();
            assert_eq!(app.model.as_ref().unwrap().items().len(), 1);
            let _ = std::fs::remove_file(csv_path);
            let _ = std::fs::remove_dir_all(root);
        }

        eprintln!("phase plaintext_export_requires_acknowledgement_and_round_trips");
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-passwords-export-store-{}",
                std::process::id()
            ));
            let out =
                std::env::temp_dir().join(format!("tilo-passwords-export-{}", std::process::id()));
            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(crate::PasswordsModel::open_in(&root));
            app.model
                .as_mut()
                .unwrap()
                .save_login(
                    "Example",
                    "ada",
                    "export-secret",
                    Some("https://example.com"),
                )
                .unwrap();
            app.request_export_confirmation();
            app.begin_plaintext_export();
            assert!(app.export_portal.is_none());
            if let Some(TransferDialog::ExportPlaintext { acknowledged }) = &mut app.transfer_dialog
            {
                *acknowledged = true;
            }
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(out.clone()));
            ui::host::scoped(&services, || app.begin_plaintext_export());
            assert_eq!(
                recorder.pick_requests()[0].title(),
                "Export passwords as CSV"
            );
            app.poll_transfers();
            let written = out.with_extension("csv");
            let rows =
                crate::transfer::parse_browser_csv(std::fs::File::open(&written).unwrap()).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].password.as_str(), "export-secret");
            let _ = std::fs::remove_file(written);
            let _ = std::fs::remove_dir_all(root);
        }

        eprintln!("phase encrypted_backup_and_restore_flow_preserves_complete_items");
        {
            let base = std::env::temp_dir().join(format!(
                "tilo-passwords-backup-flow-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let source_root = base.join("source");
            let restored_root = base.join("restored");
            let destination = base.join("portable-vault");
            std::fs::create_dir_all(&base).unwrap();
            let mut source = crate::PasswordsModel::open_in(&source_root);
            let mut item = SecretItem::login("Studio", "ada", "private-token");
            item.fields.insert("workspace".into(), "design".into());
            item.tags.extend(["work".into(), "favorite".into()]);
            source.save_item(&mut item).unwrap();

            let mut app = PasswordsSurface::demo(ShowcaseMode::Vault);
            app.model = Some(source);
            app.request_backup();
            app.backup_password.set_text("a memorable backup password");
            app.backup_confirmation
                .set_text("a memorable backup password");
            assert!(app.backup_form_error(true).is_none());
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
            ui::host::scoped(&services, || app.begin_encrypted_backup());
            assert_eq!(
                recorder.pick_requests()[0].title(),
                "Back up password vault"
            );
            app.poll_transfers();
            let backup_path = destination.with_extension("tilopw");
            assert!(std::fs::metadata(&backup_path).unwrap().len() > 32);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&backup_path)
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            assert!(app.backup_password.value().is_empty());

            let mut restored = PasswordsSurface::demo(ShowcaseMode::Vault);
            restored.model = Some(crate::PasswordsModel::open_in(&restored_root));
            recorder.respond_to_open_file(ui::portal::OpenFileOutcome::Opened(
                ui::portal::OpenedFile::new(
                    std::fs::File::open(&backup_path).unwrap(),
                    "portable-vault.tilopw",
                ),
            ));
            ui::host::scoped(&services, || restored.request_restore());
            assert_eq!(
                recorder.pick_requests()[1].title(),
                "Restore password vault"
            );
            restored.poll_transfers();
            assert!(matches!(
                restored.transfer_dialog,
                Some(TransferDialog::Restore { .. })
            ));
            restored
                .backup_password
                .set_text("a memorable backup password");
            restored.apply_encrypted_restore();
            let items = restored.model.as_ref().unwrap().items();
            assert_eq!(items.len(), 1);
            assert_eq!(
                items[0].fields.get("workspace").map(String::as_str),
                Some("design")
            );
            assert!(items[0].tags.contains("favorite"));
            assert!(restored.backup_password.value().is_empty());
            let _ = std::fs::remove_dir_all(base);
        }
    }
}
