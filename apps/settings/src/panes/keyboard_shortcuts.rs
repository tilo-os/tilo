//! Editable compositor shortcuts. The rows are the desktop's keyboard
//! shortcut catalogue ([`settings_store::keyboard_shortcuts`]), the same one
//! the shell publishes on the action door and the shortcut guide shows. A
//! chord is bound with a [`ShortcutEffect`], which validates it against the
//! catalogue and stores it in the person's `shortcuts` section the
//! compositor reads; a chord it refuses leaves the binding as it was and
//! says why.
//!
//! "Paste in apps" is the other shortcut the person sets here: the chord
//! the shell pastes with in an app (an image or files chosen from clipboard
//! history). An app is chosen with the shell's app chooser, never typed.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use settings_store::keyboard_shortcuts::{ShortcutCategory, ShortcutEntry, catalogue};
use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource;
use ui::widget::{AppModel, ModelPath, ShortcutRecorder, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::prefs::InputPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{Item, SettingsSection, note, rows, section, section_action, setting_row};
use settings_store::paste_shortcut::{PasteShortcut, default_for};
use ui::shell_choosers::ChosenApp;

crate::effects::effect_family!(ShortcutEffect => answer);

/// The shell's desktop navigation preferences, as the person's settings
/// hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub struct DesktopControls {
    #[serde(default)]
    pub vim_mode: bool,
    /// Hide the top bar until the pointer reaches for it; shown in the
    /// Appearance pane.
    #[serde(default)]
    pub hide_top_bar: bool,
}

impl ui::storage::SettingsSchema for DesktopControls {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("desktop_controls");
}

#[ui::tilo_resource::resource(kind = "settings.desktop-controls", label = "Desktop controls")]
impl DesktopControls {}

/// The section holding the person's shortcut bindings.
const SHORTCUTS_SECTION: &str = "shortcuts";

/// The ids of the rebindable shortcuts, as an effect takes them.
fn rebindable_ids() -> impl Iterator<Item = &'static str> {
    rebindable().map(ShortcutAction::id)
}

/// The keys that move around the desktop.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum ShortcutEffect {
    /// Bind a desktop shortcut to a chord.
    #[effect(idempotent)]
    BindShortcut {
        #[param("The shortcut", choices = rebindable_ids())]
        shortcut: String,
        #[param("The chord, such as `Super+K` or `Ctrl+Alt+T`")]
        chord: String,
    },
    /// Choose an app with the shell's app chooser, to set how the shell
    /// pastes in it.
    #[effect(job, label = "Choose an app to set how it pastes")]
    ChoosePasteApp,
}

/// Answer `effect`: a chord the catalogue refuses changes nothing and the
/// caller is told why.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: ShortcutEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let host = app.host();
    match effect {
        ShortcutEffect::ChoosePasteApp => {
            let purpose = t("settings-paste-choose-purpose");
            Ok(crate::device_read::job(
                cx,
                move || choose_app(&purpose),
                |app: &mut SettingsApp, chosen: Result<Option<ChosenApp>, String>, _| {
                    // Chose none: nothing changes.
                    let Some(chosen) = chosen? else {
                        return Ok(Effected::done());
                    };
                    remember_app_name(&chosen);
                    let settings = app.host().settings::<InputPrefs>();
                    let mut input = settings.get();
                    add_paste_app(&mut input, &chosen)?;
                    settings
                        .set(&input)
                        .map(|()| Effected::done())
                        .map_err(|error| format!("the input settings could not be saved: {error}"))
                },
            ))
        }
        ShortcutEffect::BindShortcut { shortcut, chord } => {
            let action = ShortcutAction::from_id(&shortcut)
                .ok_or_else(|| format!("`{shortcut}` is not a shortcut that can be changed"))?;
            let chord = KeyChord::parse(&chord).map_err(|error| format!("`{chord}`: {error}"))?;
            let document = host.storage().settings_document();
            let mut config = ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION));
            config
                .set(action, chord)
                .map_err(|error| error.to_string())?;
            let section = config.section().map_err(|error| error.to_string())?;
            document
                .update_section(SHORTCUTS_SECTION, section)
                .map_err(|error| format!("the shortcut could not be saved: {error}"))?;
            Ok(Effected::done())
        }
    }
}

/// The app the person chooses in the shell's app chooser for `purpose`;
/// `None` when they choose none. Blocks until they choose: run it off the
/// UI thread.
fn choose_app(purpose: &str) -> Result<Option<ChosenApp>, String> {
    let link =
        ui::shell_choosers::ShellLink::inherited().ok_or_else(|| t("settings-paste-no-chooser"))?;
    let (chosen_tx, chosen) = std::sync::mpsc::channel();
    link.choose_app(purpose, move |outcome| {
        let _ = chosen_tx.send(outcome);
    });
    chosen.recv().map_err(|_| t("settings-paste-no-chooser"))?
}

/// Set how the shell pastes in `app`, chosen to paste differently from
/// what its id suggests: the other of Ctrl+V and Ctrl+Shift+V. One already
/// set keeps its choice.
fn add_paste_app(input: &mut InputPrefs, app: &ChosenApp) -> Result<(), String> {
    if input
        .paste_shortcuts
        .keys()
        .any(|id| id.eq_ignore_ascii_case(&app.id))
    {
        return Ok(());
    }
    let other = match default_for(&app.id) {
        PasteShortcut::CtrlV => PasteShortcut::CtrlShiftV,
        PasteShortcut::CtrlShiftV => PasteShortcut::CtrlV,
    };
    input.set_paste_shortcut(&app.id, other.id())
}

thread_local! {
    /// The names of the apps chosen in the app chooser this session, by id:
    /// what their rows are titled.
    static APP_NAMES: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

fn remember_app_name(app: &ChosenApp) {
    APP_NAMES.with(|names| {
        names.borrow_mut().insert(app.id.clone(), app.name.clone());
    });
}

/// The name the person knows the app with id `id` by: the one the app
/// chooser answered with, else what its id spells.
fn app_name(id: &str) -> String {
    APP_NAMES
        .with(|names| names.borrow().get(id).cloned())
        .unwrap_or_else(|| package_store::display_name_for_package_id(id))
}

/// Every rebindable action, in catalogue order.
fn rebindable() -> impl Iterator<Item = ShortcutAction> {
    ShortcutAction::ALL.into_iter()
}

/// Every shortcut the page lists, with its name, in page order — what a pen
/// or pad button can be set to run.
pub(crate) fn shortcut_actions() -> Vec<(ShortcutAction, String)> {
    rebindable()
        .map(|action| (action, t(&action.label_key())))
        .collect()
}

/// The legends printed on the keys of the layout the keyboard is typing
/// with, which the page names keys and modifiers in.
fn active_keycaps(document: &ui::storage::SettingsDocument) -> ui::Keycaps {
    let config =
        settings_store::hardware::RichInputConfig::from_section(&document.section("input"));
    config
        .sources()
        .into_iter()
        .nth(config.active_layout_index())
        .map_or(ui::Keycaps::US, |source| {
            ui::Keycaps::from_xkb(&source.layout, source.variant.as_deref().unwrap_or(""))
        })
}

/// `chord` as the active layout's keycaps spell it (`Strg+Umschalt+T` on a
/// German layout). The stored chord stays canonical.
fn chord_label(chord: &KeyChord) -> String {
    ui::keycaps::active().chord(chord.modifiers.iter().map(String::as_str), &chord.key)
}

/// Ctrl+Tab, Ctrl+Shift+Tab and Ctrl+Shift+T are the focused app's first:
/// they change desktop tabs only where the app leaves them unhandled, while
/// the Super spellings listed below always do.
fn app_tab_chords_note() -> String {
    let chord =
        |text: &str| KeyChord::parse(text).map_or_else(|_| text.to_owned(), |c| chord_label(&c));
    t_args(
        "settings-shortcuts-app-tab-chords",
        &[
            ("next", chord("Ctrl+Tab").into()),
            ("previous", chord("Ctrl+Shift+Tab").into()),
            ("reopen", chord("Ctrl+Shift+T").into()),
        ],
    )
}

/// `chords` as drawn keycaps, side by side: the keys a person presses.
fn chord_caps(chords: &[KeyChord]) -> Box<dyn Widget> {
    let caps = ui::keycaps::active();
    Box::new(ui::widget::KeyHints::new(
        chords
            .iter()
            .map(|chord| ui::widget::KeyHint {
                keys: caps.chord_keys(chord.modifiers.iter().map(String::as_str), &chord.key),
                alternatives: Vec::new(),
                label: String::new(),
            })
            .collect(),
    ))
}

pub struct ShortcutPane {
    cx: PaneCx,
    config: ShortcutConfig,
    /// When the shortcuts section last changed, to read it again.
    seen: ui::storage::SettingsRevision,
    controls: Followed<DesktopControls>,
    /// The chord each rebindable shortcut's recorder shows.
    chords: HashMap<ShortcutAction, LocalState<String>>,
    /// The last chord refused, and why.
    refused: LocalState<Option<ui::framework::Refused>>,
    refusal_shown: Option<String>,
    /// The person's input preferences: how the shell pastes in each app.
    input: Followed<InputPrefs>,
    structure_dirty: bool,
    composed: ui::widget::Composed,
}

impl ShortcutPane {
    /// The pane over `cx`'s shortcut bindings.
    pub fn new(cx: PaneCx) -> Self {
        let document = cx.storage().settings_document().clone();
        ui::keycaps::set_active(active_keycaps(&document));
        let config = ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION));
        let controls = cx.follow::<DesktopControls>();
        let input = cx.follow::<InputPrefs>();
        let chords = rebindable()
            .map(|action| (action, LocalState::new(String::new())))
            .collect();
        let pane = Self {
            refused: cx.refused(),
            cx,
            config,
            seen: document.revision(),
            controls,
            chords,
            refusal_shown: None,
            input,
            structure_dirty: false,
            composed: ui::widget::Composed::default(),
        };
        pane.show();
        pane
    }

    fn show(&self) {
        for (action, state) in &self.chords {
            state.set(
                self.config
                    .bindings
                    .get(action)
                    .map(KeyChord::display)
                    .unwrap_or_default(),
            );
        }
    }

    fn entry_row(&self, entry: &ShortcutEntry) -> Item {
        let label = t(&entry.label_key);
        let recorder = entry
            .action
            .filter(|_| entry.rebindable)
            .and_then(|action| Some((action, self.chords.get(&action)?)));
        match recorder {
            Some((action, chord)) => {
                let id = action.id();
                let recorder = ShortcutRecorder::new(
                    &format!("shortcuts.{}", id.replace('-', "_")),
                    chord,
                    &entry
                        .defaults
                        .first()
                        .map(KeyChord::display)
                        .unwrap_or_default(),
                );
                let recorder = match self.cx.binding(
                    effect(ShortcutEffect::BIND_SHORTCUT)
                        .map(move |chord: String| (id.to_owned(), chord)),
                ) {
                    Some(binding) => recorder.on_action(binding),
                    None => recorder,
                };
                setting_row(&label, Box::new(recorder))
            }
            None if entry.chords.is_empty() => {
                crate::widgets::value_row(&label, &t("settings-shortcut-unassigned"))
            }
            None => setting_row(&label, chord_caps(&entry.chords)),
        }
    }

    fn composition(&self) -> Box<dyn Widget> {
        let entries = catalogue(&self.config);
        let mut cards: Vec<SettingsSection> = ShortcutCategory::ALL
            .into_iter()
            .map(|category| {
                let mut body: Vec<Item> = Vec::new();
                if category == ShortcutCategory::Navigation {
                    // The refusal opens the section so it is seen next to the
                    // switch that caused it, not after a long list of rows.
                    if let Some(reason) = &self.refusal_shown {
                        body.push(note(reason));
                    }
                    body.push(vim_mode(&self.cx, &self.controls));
                    body.push(note(&app_tab_chords_note()));
                }
                body.extend(
                    entries
                        .iter()
                        .filter(|entry| entry.category == category)
                        .map(|entry| self.entry_row(entry)),
                );
                section(&t(&category.label_key()), rows(body))
            })
            .collect();
        if !self.config.actions.is_empty() {
            let mut actions: Vec<Item> = self
                .config
                .actions
                .iter()
                .map(|bound| {
                    setting_row(&bound.label, chord_caps(std::slice::from_ref(&bound.chord)))
                })
                .collect();
            actions.push(note(&t("settings-shortcuts-actions-caption")));
            cards.push(section(&t("settings-shortcuts-actions"), rows(actions)));
        }
        cards.push(self.paste_card());
        crate::widgets::page(cards)
    }

    /// "Paste in apps": a row per app the person set, choosing Ctrl+V or
    /// Ctrl+Shift+V, and the app chooser to add one.
    fn paste_card(&self) -> SettingsSection {
        let shortcuts = [PasteShortcut::CtrlV, PasteShortcut::CtrlShiftV];
        let label = |shortcut: PasteShortcut| {
            t(match shortcut {
                PasteShortcut::CtrlV => "input-paste-ctrl-v",
                PasteShortcut::CtrlShiftV => "input-paste-ctrl-shift-v",
            })
        };
        let mut body = vec![note(&t("input-paste-caption"))];
        let mut apps: Vec<(String, String)> = self
            .input
            .get()
            .paste_shortcuts
            .iter()
            .map(|(id, shortcut)| (id.clone(), shortcut.clone()))
            .collect();
        apps.sort_by_cached_key(|(id, _)| app_name(id).to_lowercase());
        for (index, (id, stored)) in apps.into_iter().enumerate() {
            let name = app_name(&id);
            let current = PasteShortcut::parse(&stored).unwrap_or(PasteShortcut::CtrlV);
            let app = id.clone();
            let segments = ui::widget::ToggleGroup::from_state(
                shortcuts
                    .iter()
                    .map(|shortcut| (*shortcut, ui::widget::Segment::new(label(*shortcut))))
                    .collect(),
                &LocalState::new(current),
            )
            .with_id(format!("shortcuts.paste.{index}"))
            .with_accessible_name(t_args(
                "settings-paste-shortcut-for",
                &[("app", name.as_str().into())],
            ));
            let segments = self.cx.bind(
                segments,
                effect(crate::panes::mouse_keyboard::InputEffect::SET_PASTE_SHORTCUT).map(
                    move |chosen: usize| {
                        let shortcut = shortcuts.get(chosen).copied().unwrap_or(current);
                        (app.clone(), shortcut.id().to_owned())
                    },
                ),
            );
            let remove = self.cx.bind_invocation(
                ui::widget::primitives::IconButton::regular(
                    &format!("shortcuts.paste.{index}.remove"),
                    ui::icons::Icon::X,
                    &t_args("settings-paste-remove", &[("app", name.as_str().into())]),
                ),
                crate::panes::mouse_keyboard::InputEffect::SetPasteShortcut {
                    app: id,
                    shortcut: "default".into(),
                },
            );
            body.push(setting_row(
                &name,
                Box::new(
                    ui::widget::Row::new()
                        .no_wrap()
                        .with_align_items(taffy::AlignItems::Center)
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .push(Box::new(segments))
                        .push(Box::new(remove)),
                ),
            ));
        }
        let choose = self.cx.bind_invocation(
            ui::widget::Button::new("shortcuts.paste.choose", &t("settings-paste-choose"))
                .with_icon(ui::icons::Icon::Plus),
            ShortcutEffect::ChoosePasteApp,
        );
        section_action(&t("settings-paste-title"), choose, rows(body))
    }
}

pub(crate) fn vim_mode(cx: &PaneCx, controls: &Followed<DesktopControls>) -> Item {
    cx.row(t("settings-shortcuts-vim"))
        .id("shortcuts.vim_mode")
        .of(controls, DesktopControls::paths().vim_mode)
        .toggle()
}

impl Widget for ShortcutPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let document = self.cx.storage().settings_document();
        let revision = document.revision();
        if revision != self.seen {
            self.seen = revision;
            let config = ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION));
            if config != self.config {
                self.config = config;
                self.structure_dirty = true;
            }
        }
        // A refused chord is not kept: the recorder shows the stored one
        // again, and the pane says why it was refused.
        let refusal = self
            .refused
            .get()
            .filter(|refused| refused.action == ShortcutEffect::BIND_SHORTCUT.id)
            .map(|refused| refused.reason);
        if refusal != self.refusal_shown {
            self.refusal_shown = refusal;
            self.structure_dirty = true;
        }
        if self.structure_dirty {
            self.show();
        }
        self.controls.refresh();
        if self.input.refresh() {
            self.structure_dirty = true;
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut c = self.composition();
        let child = c.build_layout(taffy, theme, node_map);
        self.composed.replace(c);
        crate::panes::pane_node(taffy, child, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.composed.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = self.composed.element_mut() {
            c.draw(scene, transform, text, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.composed.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }
    fn name(&self) -> &'static str {
        "ShortcutPane"
    }
    fn get_rect(&self) -> Rect {
        self.composed.rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn bind(app: &mut TestApp<SettingsShell>, shortcut: &str, chord: &str) -> bool {
        app.effect(&SettingsEffect::from(ShortcutEffect::BindShortcut {
            shortcut: shortcut.into(),
            chord: chord.into(),
        }))
        .is_ok()
    }

    /// The catalogue names every shortcut, and only the editable ones are
    /// offered for rebinding.
    #[test]
    fn rows_are_the_shortcut_catalogue() {
        let listed: Vec<&str> = rebindable_ids().collect();
        assert!(listed.contains(&"quick-settings"));
        assert!(
            listed.contains(&"lock"),
            "the system catalogue offers the lock chord for editing"
        );
        for entry in catalogue(&ShortcutConfig::default()) {
            assert_ne!(
                t(&entry.label_key),
                entry.label_key,
                "{} has no name in the Settings catalogue",
                entry.id
            );
        }
        for category in ShortcutCategory::ALL {
            assert_ne!(t(&category.label_key()), category.label_key());
        }
    }

    /// A chord lands in the `shortcuts` section the compositor reads; a
    /// chord another shortcut holds is refused with the reason and changes
    /// nothing; Vim mode is the shell's desktop-controls preference.
    #[test]
    fn bindings_are_validated_and_stored() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let document = app.settings::<DesktopControls>().document().clone();
        let stored = |document: &ui::storage::SettingsDocument| {
            ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION))
        };
        assert!(bind(&mut app, "quick-settings", "Super+Shift+Q"));
        assert_eq!(
            stored(&document).bindings[&ShortcutAction::QuickSettings].display(),
            "Super+Shift+q"
        );
        assert!(
            !bind(&mut app, "quick-settings", "Super+Space"),
            "Super+Space opens search"
        );
        assert_eq!(
            stored(&document).bindings[&ShortcutAction::QuickSettings].display(),
            "Super+Shift+q"
        );
        assert!(bind(&mut app, "lock", "Super+Shift+L"));
        assert_eq!(
            stored(&document).bindings[&ShortcutAction::Lock].display(),
            "Super+Shift+l"
        );

        crate::host::testing::edit_field(&mut app, "settings.desktop-controls", "vim_mode", true)
            .unwrap();
        assert!(app.settings::<DesktopControls>().get().vim_mode);
    }

    /// The pane shows the stored chords and follows a change made elsewhere.
    #[test]
    fn the_pane_follows_the_bindings() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut pane = ShortcutPane::new(cx.clone());
        assert_eq!(
            pane.chords[&ShortcutAction::Search].get(),
            "Super+Space",
            "the default binding"
        );
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::Search, KeyChord::parse("Super+J").unwrap())
            .unwrap();
        cx.storage()
            .settings_document()
            .update_section(SHORTCUTS_SECTION, config.section().unwrap())
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.chords[&ShortcutAction::Search].get(), "Super+j");
    }

    #[test]
    fn an_app_is_chosen_for_its_paste_shortcut_never_typed() {
        fn find<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if widget.test_id() == Some(id) {
                return Some(widget);
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| find(child, id))
        }
        fn named(widget: &dyn Widget, name: &str) -> usize {
            usize::from(widget.name() == name)
                + widget
                    .children()
                    .into_iter()
                    .map(|child| named(child, name))
                    .sum::<usize>()
        }
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let settings = cx.storage().settings::<InputPrefs>();
        let mut input = settings.get();
        input
            .set_paste_shortcut("org.example.Writer", "ctrl_shift_v")
            .unwrap();
        settings.set(&input).unwrap();
        let pane = ShortcutPane::new(cx);
        let composed = pane.composition();
        let said = crate::widgets::announced_text(composed.as_ref());
        assert!(said.iter().any(|line| line == "Paste in apps"), "{said:#?}");
        assert!(
            find(composed.as_ref(), "shortcuts.paste.choose").is_some(),
            "an app is added with the app chooser"
        );
        assert_eq!(
            named(composed.as_ref(), "TextInput"),
            0,
            "no app id is typed"
        );
        assert!(said.iter().any(|line| line == "Writer"), "{said:#?}");

        let mut input = InputPrefs::default();
        let terminal = ChosenApp {
            id: "org.example.Terminal".into(),
            name: "Terminal".into(),
            window_ids: vec!["org.example.Terminal".into()],
        };
        add_paste_app(&mut input, &terminal).unwrap();
        assert_eq!(
            input
                .paste_shortcuts
                .get("org.example.Terminal")
                .map(String::as_str),
            Some("ctrl_v")
        );
        input
            .set_paste_shortcut("org.example.Terminal", "ctrl_shift_v")
            .unwrap();
        add_paste_app(&mut input, &terminal).unwrap();
        assert_eq!(
            input
                .paste_shortcuts
                .get("org.example.Terminal")
                .map(String::as_str),
            Some("ctrl_shift_v")
        );
    }
}
