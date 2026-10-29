//! Editable compositor shortcuts. The rows are the desktop's keyboard
//! shortcut catalogue ([`settings_store::keyboard_shortcuts`]), the same one
//! the shortcut guide shows. A chord is bound with the person's `shortcuts`
//! section's `rebind` ([`ShortcutPrefs`]), which checks it against every
//! other chord; a chord it refuses leaves the binding as it was and says
//! why.
//!
//! "Paste in apps" is the other shortcut the person sets here: the chord
//! the shell pastes with in an app (an image or files chosen from clipboard
//! history). An app is chosen with the shell's app chooser, never typed.

use std::collections::HashMap;
use std::rc::Rc;

use provider_settings_ui::bound::{SettingRows as _, record_item};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use settings_store::keyboard_shortcuts::{ShortcutCategory, ShortcutEntry, catalogue};
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource;
use ui::widget::{Element, ShortcutRecorder, Widget};

use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
pub use crate::prefs::DesktopControls;
use crate::prefs::InputPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::shortcut_settings::ShortcutPrefs;
use provider_settings_ui::widgets::{
    Item, SettingsSection, note, page_of, rows, section, section_action, setting_row,
};
use settings_store::paste_shortcut::{PasteShortcut, default_for};
use ui::shell_choosers::ChosenApp;

crate::effects::effect_family!(ShortcutEffect => answer);

/// The `shortcuts` section's verb that moves a shortcut to a chord.
const REBIND: &str = "rebind";

/// The keys that move around the desktop.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum ShortcutEffect {
    /// Choose an app with the shell's app chooser, to set how the shell
    /// pastes in it.
    #[effect(job, label = "Choose an app to set how it pastes")]
    ChoosePasteApp,
}

/// Answer `effect`.
#[allow(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "the signature every effect family answers with"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: ShortcutEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    match effect {
        ShortcutEffect::ChoosePasteApp => {
            let purpose = t("settings-paste-choose-purpose");
            Ok(app.job(
                cx,
                move || choose_app(&purpose),
                |app: &SettingsApp, chosen: Result<Option<ChosenApp>, String>, _| {
                    // Chose none: nothing changes.
                    let Some(chosen) = chosen? else {
                        return Ok(Effected::done());
                    };
                    remember_app_name(&chosen);
                    let input = app.storage().settings::<InputPrefs>().get();
                    match paste_app(&input, &chosen) {
                        Some(shortcut) => app
                            .sections()
                            .invoke::<InputPrefs>(
                                InputPrefs::PASTE_WITH.with((chosen.id, shortcut.id().to_owned())),
                            )
                            .map(|()| Effected::done()),
                        None => Ok(Effected::done()),
                    }
                },
            ))
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

/// How the shell is to paste in `app`, chosen to paste differently from
/// what its id suggests: the other of Ctrl+V and Ctrl+Shift+V. `None` for
/// one `input` already sets, which keeps its choice.
fn paste_app(input: &InputPrefs, app: &ChosenApp) -> Option<PasteShortcut> {
    if input
        .paste_shortcuts
        .keys()
        .any(|id| id.eq_ignore_ascii_case(&app.id))
    {
        return None;
    }
    Some(match default_for(&app.id) {
        PasteShortcut::CtrlV => PasteShortcut::CtrlShiftV,
        PasteShortcut::CtrlShiftV => PasteShortcut::CtrlV,
    })
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
        settings_store::hardware::RichInputConfig::from_section(&settings_store::merged_sections(
            &document.load(),
            settings_store::hardware::INPUT_SECTIONS,
        ));
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
                label: String::new(),
            })
            .collect(),
    ))
}

/// The Keyboard shortcuts page as view code over the person's `shortcuts`
/// section, their desktop controls and how the shell pastes in their apps:
/// every recorder rebinds through the section's `rebind`, which checks a
/// chord against every other; a refused chord shows the stored one again,
/// and the page says why.
pub(crate) fn view(cx: &ViewCx<Settings>, _app: &Rc<SettingsApp>) -> Option<Element> {
    ui::keycaps::set_active(active_keycaps(cx.storage().settings_document()));
    let actions = cx.actions();
    let shortcuts = cx.settings::<ShortcutPrefs>();
    let catalogue = cx.record(&shortcuts, |cx| {
        let config = cx.value(ShortcutPrefs::config);
        let refused = cx.dispatcher().refused();
        cx.switch(&config, move |cx, config| {
            let config = config.clone();
            cx.switch(&refused, move |cx, refused| {
                let refusal = refused
                    .as_ref()
                    .filter(|refused| refused.action == REBIND)
                    .map(|refused| refused.reason.clone());
                page_of(shortcut_cards(cx, &config, refusal.as_deref()))
            })
        })
    })?;
    let input = cx.settings::<InputPrefs>();
    let paste = cx.record(&input, move |cx| {
        let apps = cx.value(|input: &InputPrefs| input.paste_shortcuts.clone());
        cx.switch(&apps, move |cx, _| paste_card(cx, &actions).into_element())
    })?;
    Some(page_of(vec![catalogue, paste]))
}

/// One group per category of the catalogue `config` makes, the refusal of
/// the last chord (`refusal`) opening the navigation group beside the
/// switch that may have caused it; then the desktop's bound actions.
fn shortcut_cards(
    cx: &ViewCx<ShortcutPrefs>,
    config: &ShortcutConfig,
    refusal: Option<&str>,
) -> Vec<Element> {
    let entries = catalogue(config);
    let controls = cx.settings::<DesktopControls>();
    let mut cards: Vec<Element> = ShortcutCategory::ALL
        .into_iter()
        .map(|category| {
            let mut body: Vec<Item> = Vec::new();
            if category == ShortcutCategory::Navigation {
                if let Some(reason) = refusal {
                    body.push(note(reason));
                }
                body.extend(record_item(cx, &controls, vim_mode));
                body.push(note(&app_tab_chords_note()));
            }
            body.extend(
                entries
                    .iter()
                    .filter(|entry| entry.category == category)
                    .map(|entry| entry_row(cx, config, entry)),
            );
            section(&t(&category.label_key()), rows(body)).into_element()
        })
        .collect();
    if !config.actions.is_empty() {
        let mut actions: Vec<Item> = config
            .actions
            .iter()
            .map(|bound| setting_row(&bound.label, chord_caps(std::slice::from_ref(&bound.chord))))
            .collect();
        actions.push(note(&t("settings-shortcuts-actions-caption")));
        cards.push(section(&t("settings-shortcuts-actions"), rows(actions)).into_element());
    }
    cards
}

/// The row of `entry`: a recorder of its chord where it can be rebound,
/// else the chords it has.
fn entry_row(cx: &ViewCx<ShortcutPrefs>, config: &ShortcutConfig, entry: &ShortcutEntry) -> Item {
    let label = t(&entry.label_key);
    match entry.action.filter(|_| entry.rebindable) {
        Some(action) => {
            let id = action.id();
            let chord = LocalState::new(
                config
                    .bindings
                    .get(&action)
                    .map(KeyChord::display)
                    .unwrap_or_default(),
            );
            let recorder = ShortcutRecorder::new(
                &format!("shortcuts.{}", id.replace('-', "_")),
                &chord,
                &entry
                    .defaults
                    .first()
                    .map(KeyChord::display)
                    .unwrap_or_default(),
            );
            let recorder = cx.actions().bind(
                recorder,
                ShortcutPrefs::REBIND.map(move |chord: String| (id.to_owned(), chord)),
            );
            setting_row(&label, Box::new(recorder))
        }
        None if entry.chords.is_empty() => {
            provider_settings_ui::widgets::value_row(&label, &t("settings-shortcut-unassigned"))
        }
        None => setting_row(&label, chord_caps(&entry.chords)),
    }
}

/// "Paste in apps": a row per app the person set, choosing Ctrl+V or
/// Ctrl+Shift+V through the input section's `set-paste-shortcut`, and the
/// app chooser to add one.
fn paste_card(cx: &ViewCx<InputPrefs>, actions: &Actions<Settings>) -> SettingsSection {
    let shortcuts = [PasteShortcut::CtrlV, PasteShortcut::CtrlShiftV];
    let label = |shortcut: PasteShortcut| {
        t(match shortcut {
            PasteShortcut::CtrlV => "input-paste-ctrl-v",
            PasteShortcut::CtrlShiftV => "input-paste-ctrl-shift-v",
        })
    };
    let mut body = vec![note(&t("input-paste-caption"))];
    let mut apps: Vec<(String, String)> = cx.read(|input| {
        input
            .map(|input| {
                input
                    .paste_shortcuts
                    .iter()
                    .map(|(id, shortcut)| (id.clone(), shortcut.clone()))
                    .collect()
            })
            .unwrap_or_default()
    });
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
        let segments = cx.actions().bind(
            segments,
            InputPrefs::PASTE_WITH.map(move |chosen: usize| {
                let shortcut = shortcuts.get(chosen).copied().unwrap_or(current);
                (app.clone(), shortcut.id().to_owned())
            }),
        );
        let remove = cx.actions().bind_invocation(
            ui::widget::primitives::IconButton::regular(
                &format!("shortcuts.paste.{index}.remove"),
                ui::icons::Icon::X,
                &t_args("settings-paste-remove", &[("app", name.as_str().into())]),
            ),
            InputPrefs::PASTE_WITH.with((id, "default".to_owned())),
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
    let choose = actions.bind_effect_invocation(
        ui::widget::Button::new("shortcuts.paste.choose", &t("settings-paste-choose"))
            .with_icon(ui::icons::Icon::Plus),
        &SettingsEffect::from(ShortcutEffect::ChoosePasteApp),
    );
    section_action(&t("settings-paste-title"), choose, rows(body))
}

pub(crate) fn vim_mode(cx: &ViewCx<DesktopControls>) -> Item {
    cx.setting(t("settings-shortcuts-vim"))
        .id("shortcuts.vim_mode")
        .toggle(DesktopControls::paths().vim_mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn bind(app: &mut TestApp<SettingsShell>, shortcut: &str, chord: &str) -> bool {
        crate::testing::invoke_section(
            app,
            "settings.shortcuts",
            REBIND,
            serde_json::json!({ "shortcut": shortcut, "chord": chord }),
        )
        .is_ok()
    }

    /// Each chord recorder changes its chord through the `shortcuts`
    /// section's `rebind`, as an agent does; none is left without it.
    #[test]
    fn the_recorders_rebind_through_the_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::KeyboardShortcuts);
        app.harness().resize((1200, 8000)).unwrap();
        app.settle();
        let recorders: Vec<_> =
            ui::widget::controls::control_action_bindings(app.harness().root_widget())
                .into_iter()
                .filter(|binding| {
                    binding.control_id.starts_with("shortcuts.")
                        && binding.control_id != "shortcuts.vim_mode"
                        && !binding.control_id.starts_with("shortcuts.paste.")
                })
                .collect();
        assert!(!recorders.is_empty(), "the pane shows chord recorders");
        assert!(
            recorders.iter().all(|binding| matches!(
                &binding.kind,
                ui::widget::ControlBindingKind::Action(address)
                    if address.resource == "settings:settings.shortcuts" && address.action == REBIND
            )),
            "{recorders:?}"
        );
    }

    /// The catalogue names every shortcut, and only the editable ones are
    /// offered for rebinding.
    #[test]
    fn rows_are_the_shortcut_catalogue() {
        let listed: Vec<&str> = rebindable().map(ShortcutAction::id).collect();
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
            ShortcutConfig::from_section(document.section("shortcuts"))
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

        crate::testing::edit_field(&mut app, "settings.desktop-controls", "vim_mode", true)
            .unwrap();
        assert!(app.settings::<DesktopControls>().get().vim_mode);
    }

    /// The page shows the stored chords and follows a change made
    /// elsewhere.
    #[test]
    fn the_page_follows_the_bindings() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1400, 6000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::KeyboardShortcuts);
        app.settle();
        let chord = |app: &mut TestApp<SettingsShell>| {
            ui::widget::find_test_id(app.harness().root_widget(), "shortcuts.search")
                .and_then(Widget::accessibility)
                .and_then(|node| node.value)
                // Spoken as "Super + Space": compared without the spacing
                // or case speech adds.
                .map(|chord| chord.replace(' ', "").to_lowercase())
        };
        assert_eq!(
            chord(&mut app).as_deref(),
            Some("super+space"),
            "the default binding"
        );
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::Search, KeyChord::parse("Super+J").unwrap())
            .unwrap();
        app.settings::<ShortcutPrefs>()
            .set(&ShortcutPrefs::of(&config))
            .unwrap();
        app.settle();
        assert_eq!(chord(&mut app).as_deref(), Some("super+j"));
    }

    #[test]
    fn an_app_is_chosen_for_its_paste_shortcut_never_typed() {
        fn named(widget: &dyn Widget, name: &str) -> usize {
            usize::from(widget.name() == name)
                + widget
                    .children()
                    .into_iter()
                    .map(|child| named(child, name))
                    .sum::<usize>()
        }
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let mut input = app.settings::<InputPrefs>().get();
        input
            .set_paste_shortcut("org.example.Writer", "ctrl_shift_v")
            .unwrap();
        app.settings::<InputPrefs>().set(&input).unwrap();
        app.harness().resize((1400, 6000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::KeyboardShortcuts);
        app.settle();
        assert!(app.harness().a11y_node_named("Paste in apps").is_some());
        assert!(
            ui::widget::find_test_id(app.harness().root_widget(), "shortcuts.paste.choose")
                .is_some(),
            "an app is added with the app chooser"
        );
        assert!(app.harness().a11y_node_named("Writer").is_some());
        assert_eq!(
            named(app.harness().root_widget(), "TextInput"),
            0,
            "no app id is typed"
        );

        let mut input = InputPrefs::default();
        let terminal = ChosenApp {
            id: "org.example.Terminal".into(),
            name: "Terminal".into(),
            window_ids: vec!["org.example.Terminal".into()],
        };
        assert_eq!(paste_app(&input, &terminal), Some(PasteShortcut::CtrlV));
        input
            .set_paste_shortcut("org.example.Terminal", "ctrl_shift_v")
            .unwrap();
        assert_eq!(
            paste_app(&input, &terminal),
            None,
            "one already set keeps its choice"
        );
    }
}
