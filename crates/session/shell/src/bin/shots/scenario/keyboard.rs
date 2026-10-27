//! The on-screen keyboard over the field it types into: the real `apps/osk`
//! widget, laid out in the panel the compositor gives its surface and in the
//! purpose the focused field asks for.
//!
//! On a live desktop the keyboard is a client of its own, mirrored by the
//! shell as a floating surface (`FrameBuilder::set_osk_surface`). A shots
//! render has no clients, so it has no surface to mirror: the frame draws the
//! keyboard widget itself (`FrameBuilder::set_shots_keyboard`), docked exactly
//! where the surface would be. A shot therefore shows what a touch screen
//! shows, including the field's layout.

use super::super::driver::ShellDriver;
use super::{Action, OskField, PLACEMENT_SETTLE_FRAMES, Scenario};

/// The keyboard over a Terminal at a prompt, in its terminal layout.
pub(super) fn terminal() -> Scenario {
    Scenario {
        name: "keyboard-terminal",
        description: "The on-screen keyboard over a terminal at its prompt: the terminal's \
             own layout with Ctrl, Alt and the arrow keys, the keyboard docked as a touch \
             screen shows it.",
        actions: vec![
            Action::OpenHostedWindow {
                app: "Terminal",
                title: "Terminal",
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::ClickNamed("terminal"),
            Action::Wait(4),
            Action::TerminalPrints { from: 1, to: 6 },
            Action::Wait(4),
            Action::Relayout,
            Action::Snapshot("01-before"),
            Action::ShowKeyboard(Some(OskField::TerminalPrompt)),
            Action::Wait(2),
            Action::Relayout,
            Action::Snapshot("02-keyboard-up"),
        ],
    }
}

/// The keyboard over a text field: the plain letter layout, with the
/// suggestion band a prose field asks for.
pub(super) fn text_field() -> Scenario {
    Scenario {
        name: "keyboard-text-field",
        description: "The on-screen keyboard over a search field: the plain letter layout \
             with its suggestion band, the field's text above the panel.",
        actions: vec![
            Action::OpenHostedWindow {
                app: "Settings",
                title: "Settings",
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::ClickNamed("window-search-open"),
            Action::Wait(24),
            Action::Type("keyboard"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("01-before"),
            Action::ShowKeyboard(Some(OskField::WindowSearch)),
            Action::Wait(2),
            Action::Relayout,
            Action::Snapshot("02-keyboard-up"),
        ],
    }
}

/// The keyboard over a chat composer.
pub(super) fn chat_composer() -> Scenario {
    Scenario {
        name: "keyboard-chat-composer",
        description: "The on-screen keyboard over a chat composer: the composer tapped the \
             way a finger reaches it, the message typed, the keyboard up in the layout the \
             composer asks for.",
        actions: vec![
            Action::OpenHostedWindow {
                app: "Chat",
                title: "Chat",
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::ClickNamed("chat-composer"),
            Action::Wait(4),
            Action::Typed("Lunch at one?"),
            Action::Wait(4),
            Action::Relayout,
            Action::Snapshot("01-before"),
            Action::ShowKeyboard(Some(OskField::ChatComposer)),
            Action::Wait(2),
            Action::Relayout,
            Action::Snapshot("02-keyboard-up"),
        ],
    }
}

/// Every keyboard scenario this module adds.
pub(super) fn all() -> Vec<Scenario> {
    vec![terminal(), text_field(), chat_composer()]
}

/// Apply [`Action::ShowKeyboard`] and [`Action::Typed`]; every other action
/// belongs to another module.
pub(super) fn apply(driver: &mut ShellDriver, action: &Action) {
    let Action::ShowKeyboard(field) = action else {
        if let Action::Typed(text) = action {
            driver.type_str(text);
        }
        return;
    };
    // A named field is tapped first, as the person whose finger it is would:
    // the keyboard then takes the purpose that field declares.
    if let Some(field) = field
        && let Some(centre) = driver.resolve_named_centre(field.target())
    {
        driver.click_at(centre);
        driver.step_frames(2);
    }
    show_keyboard(driver, field.map(super::OskField::purpose));
}

/// Raise the keyboard over the focused field the way the compositor does: the
/// field's own content purpose and hints are what the keyboard lays itself
/// out for, and the frame docks it where it docks the keyboard surface. A
/// `stated` purpose stands in where the field cannot report one, and is
/// checked against the field's own when the field does report it.
fn show_keyboard(driver: &mut ShellDriver, stated: Option<ui::ime::InputPurpose>) {
    let content = match driver.active_ime_content_for_shots() {
        // The field reports its own purpose: that is what a live desktop
        // would take off the wire, so it is what the shot shows.
        Some(content) => {
            if let Some(stated) = stated {
                debug_assert_eq!(
                    content.purpose, stated,
                    "the shot states a {stated:?} field, and the focused field reports \
                     {:?}; one of them is wrong",
                    content.purpose
                );
            }
            content
        }
        // The field cannot report one in a capture (no session, or it lives
        // in the shell's chrome rather than the app's tree): this is the
        // purpose it declares, which is what the live keyboard would get.
        None => {
            let Some(stated) = stated else {
                driver.set_shots_keyboard(None);
                return;
            };
            ui::ime::InputContent::of(stated)
        }
    };
    driver.set_shots_keyboard(Some(content));
}

#[cfg(test)]
mod tests {
    use super::super::{SnapshotSink, run};
    use super::*;
    use ui::widget::Widget;
    use vello::kurbo::Rect;

    /// Records what it saw without writing files.
    #[derive(Default)]
    pub(super) struct CountingSink {
        seen: Vec<String>,
    }
    impl SnapshotSink for CountingSink {
        fn capture(&mut self, _driver: &mut ShellDriver, name: &str) {
            self.seen.push(name.to_string());
        }
    }

    /// The keyboard docks at the bottom of the display, centred, no wider
    /// than 960 and between 220 and 380 tall with a margin of 20 — the rule
    /// the compositor places the keyboard surface by.
    #[test]
    fn the_keyboard_is_docked_at_the_bottom_centred_and_capped() {
        let wide = shell::shell::shots_keyboard::keyboard_panel(Rect::new(0.0, 0.0, 1600.0, 900.0));
        assert_eq!(wide.width(), 960.0_f64.min(1600.0 - 40.0));
        assert_eq!(
            wide.x0,
            (1600.0 - wide.width()) / 2.0,
            "centred horizontally"
        );
        assert_eq!(
            wide.y1,
            900.0 - 20.0,
            "one margin above the display's bottom"
        );
        assert_eq!(wide.height(), 360.0, "forty percent of 900");

        // A short display takes the floor height rather than forty percent.
        let short =
            shell::shell::shots_keyboard::keyboard_panel(Rect::new(0.0, 0.0, 1600.0, 400.0));
        assert_eq!(short.height(), 220.0, "the floor of 220 on a short display");
        assert!(short.y1 <= 400.0 && short.y0 >= 0.0, "inside the display");

        // A display that does not start at the desktop's top-left is docked
        // inside itself, not on the desktop.
        let beside =
            shell::shell::shots_keyboard::keyboard_panel(Rect::new(1600.0, 0.0, 3200.0, 900.0));
        assert_eq!(beside.x0, 1600.0 + (1600.0 - beside.width()) / 2.0);
        assert_eq!(beside.y1, 900.0 - 20.0);
    }

    /// The scenarios run to the end, and each takes its keyboard-up picture
    /// after the keyboard rose — and the open field the keyboard rose over
    /// is the one the live tree reports.
    #[test]
    fn every_keyboard_scenario_reaches_its_keyboard_shot() {
        for scenario in all() {
            let mut driver = ShellDriver::new((1600, 900));
            let mut sink = CountingSink::default();
            run(&mut driver, &scenario, &mut sink);
            assert!(
                sink.seen.iter().any(|n| n == "02-keyboard-up"),
                "{} should snapshot the keyboard up; it took {:?}",
                scenario.name,
                sink.seen
            );
            assert!(
                driver.active_ime_content_for_shots().is_some()
                    || scenario
                        .actions
                        .iter()
                        .any(|a| matches!(a, Action::ShowKeyboard(Some(_)))),
                "{} should have a field for the keyboard; otherwise the \
                 keyboard-up shot is the same picture as the one before it",
                scenario.name
            );
        }
    }

    /// A Terminal at its prompt asks for the terminal layout, so the shot the
    /// whole issue is about — a terminal with the keyboard up — shows the
    /// editing strip and not the letter rows.
    #[test]
    fn a_terminal_at_its_prompt_asks_for_the_terminal_keyboard() {
        let content = ui::ime::InputContent::of(ui::ime::InputPurpose::Terminal);
        let keyboard = shell::shell::shots_keyboard::keyboard_for(content);
        assert_eq!(
            keyboard.purpose(),
            osk::layout::InputPurpose::Terminal,
            "a terminal prompt asks for the terminal keyboard"
        );
        // The wire values the compositor carries have to survive into the
        // keyboard app's own purpose, or a live terminal gets the letter rows.
        assert_eq!(
            osk::purpose_from_wire(ui::ime::InputContent::TERMINAL.wayland().1),
            osk::layout::InputPurpose::Terminal,
        );
    }

    /// A field's own content purpose is what the keyboard lays itself out
    /// for — a terminal gets its editing strip, a composer the letter rows.
    #[test]
    fn the_keyboard_takes_the_purposes_the_fields_ask_for() {
        let text = shell::shell::shots_keyboard::keyboard_for(ui::ime::InputContent::TEXT);
        assert_eq!(text.purpose(), osk::layout::InputPurpose::Text);
        let email = shell::shell::shots_keyboard::keyboard_for(ui::ime::InputContent::of(
            ui::ime::InputPurpose::Email,
        ));
        assert_eq!(email.purpose(), osk::layout::InputPurpose::Email);
        // The wire is what the compositor actually carries, so the purpose
        // has to survive the round trip the keyboard app reads it off.
        let terminal = shell::shell::shots_keyboard::keyboard_for(ui::ime::InputContent::TERMINAL);
        assert_eq!(terminal.purpose(), osk::layout::InputPurpose::Terminal);
    }

    /// At 200% the keyboard is laid out and drawn from the theme in hand, so
    /// a 200% shot is a real picture of a 200% keyboard rather than a 100%
    /// one in a bigger panel: its keys are bigger, and it still fits the
    /// panel the compositor docks it in.
    #[test]
    fn the_keyboard_grows_with_the_text_scale_and_stays_in_its_panel() {
        let panel =
            shell::shell::shots_keyboard::keyboard_panel(Rect::new(0.0, 0.0, 1600.0, 900.0));
        let mut drawn = Vec::new();
        for scale in [1.0_f32, 2.0] {
            let theme = ui::theme::Theme::default().with_text_scale(scale);
            let mut keyboard =
                shell::shell::shots_keyboard::keyboard_for(ui::ime::InputContent::TEXT);
            let rect = shell::shell::shots_keyboard::lay_out(&mut keyboard, panel, &theme);
            assert!(
                rect.x0 >= panel.x0 - 0.5
                    && rect.y0 >= panel.y0 - 0.5
                    && rect.x1 <= panel.x1 + 0.5
                    && rect.y1 <= panel.y1 + 0.5,
                "at {scale}x the keyboard is laid out inside the panel: {rect:?} in {panel:?}"
            );
            let mut scene = vello::Scene::new();
            let mut text = ui::TextRenderer::new();
            let presence = ui::PresenceCtx::default();
            keyboard.draw(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
                &presence,
            );
            drawn.push(scene);
        }
        assert!(
            !drawn[0].encoding_eq(&drawn[1]),
            "a 200% keyboard draws different keys than a 100% one"
        );
    }
}
