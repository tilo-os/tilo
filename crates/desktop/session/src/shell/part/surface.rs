//! The part as a shell surface: the layers it lays out, the pixels it shows of
//! the windows over it, and the material it records for the compositor to fill in below.

use tilo_session_core::surface::Scroll;

use super::{
    CursorIcon, KeyClaim, Origin, ShellPart, ShellSurface, SurfaceIme, SurfaceKey, SurfaceLayer,
    SurfaceTextInput, SwitcherKey,
};

impl ShellSurface for ShellPart {
    fn layers(&self) -> Vec<SurfaceLayer> {
        // Aside for a fullscreen window, the window has the whole display: not a row of the
        // bar's pixels stands over it.
        if self.hidden() {
            return Vec::new();
        }
        self.widgets.layers()
    }

    /// The whole screen lies under the windows, which stand in its wells; the bar rides over
    /// them. A surface of the shell's (the overview, the switcher, the launcher) or a menu of
    /// the part's stands over the windows instead: while one covers the outputs, the part and
    /// the pictures it names are drawn above every window, as what it shows is the person's
    /// to look at rather than a backdrop behind their windows.
    fn below_windows(&self) -> bool {
        self.widgets.below_windows() && !self.cover.over()
    }

    /// The live pictures the part's views name: each window's own pixels, where the view would
    /// draw it, for the compositor to draw over the part. A part that draws nothing pictures
    /// nothing, and a picture never reaches past the part's own display.
    fn window_pictures(&self) -> Vec<tilo_session_core::surface::WindowPicture> {
        let Some((previews, windows)) = self.previews.as_ref() else {
            return Vec::new();
        };
        if self.hidden() {
            return Vec::new();
        }
        let placement = self.widgets.placement();
        let (ox, oy) = placement.origin;
        let display = (
            f64::from(ox),
            f64::from(oy),
            f64::from(placement.size.0),
            f64::from(placement.size.1),
        );
        previews
            .pictures()
            .into_iter()
            .filter_map(|(key, rect)| {
                let window = windows(&key)?;
                let at = (
                    f64::from(ox) + rect.x0,
                    f64::from(oy) + rect.y0,
                    rect.width(),
                    rect.height(),
                );
                (rect.width() > 0.0 && rect.height() > 0.0).then_some(
                    tilo_session_core::surface::WindowPicture {
                        window,
                        rect: at,
                        opacity: 1.0,
                        clip: Some(display),
                    },
                )
            })
            .collect()
    }

    fn claim_key(&mut self, key: &SurfaceKey, locked: bool, _inhibited: bool) -> KeyClaim {
        // A shell's overlays answer the keyboard: Alt+Tab walks the switcher, Enter goes there,
        // and Esc closes whatever surface is up. A locked session is not the shell's to answer.
        if locked || self.switcher_key(key).is_none() {
            return KeyClaim::Pass;
        }
        self.input = true;
        KeyClaim::Take
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        if self.hidden() {
            return false;
        }
        // While a menu of the part's is up, a press anywhere over it is the part's: outside
        // the menu it closes it, as a press outside any menu does. The held Super's keys are a
        // picture over the windows, not one: a press there is the window's, so the shell takes
        // none of its display.
        if self.cover.over()
            && self.origin.borrow().link.surface() != tilo_session_core::shell::PEEK_SURFACE
            && let Some((_, (ox, oy, width, height))) = self.output
        {
            let (x, y) = (x - f64::from(ox), y - f64::from(oy));
            return x >= 0.0 && y >= 0.0 && x < f64::from(width) && y < f64::from(height);
        }
        self.widgets.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.input = true;
        self.pointer = (x, y);
        self.widgets.pointer_motion(x, y);
        self.follow_lifted_float();
    }

    fn pointer_leave(&mut self) {
        self.input = true;
        self.widgets.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input = true;
        self.took_press();
        if pressed {
            self.pressed_at = self.pointer;
        }
        self.widgets.pointer_button(button, pressed);
    }

    /// The floating windows' cards, bar and body, which the part draws over the windows beneath
    /// them: those windows are drawn around a card and under its shadows, and a press on it is the
    /// part's.
    fn drawn_over_windows(&self) -> Vec<tilo_session_core::surface::WindowCover> {
        let (Some(desktop), Some((_, (ox, oy, width, height)))) = (&self.floats, &self.output)
        else {
            return Vec::new();
        };
        if self.hidden() {
            return Vec::new();
        }
        let display = ui::kurbo::Rect::new(
            f64::from(*ox),
            f64::from(*oy),
            f64::from(*ox) + f64::from(*width),
            f64::from(*oy) + f64::from(*height),
        );
        let theme = self.widgets.theme();
        desktop
            .float_covers()
            .into_iter()
            .filter(|(_, card)| display.contains(card.center()))
            .filter_map(|(tab, card)| {
                let radius = ui::widget::SurfaceLevel::Window
                    .radius(theme)
                    .min(card.height() / 2.0);
                Some(tilo_session_core::surface::WindowCover {
                    window: desktop.window_of_tab(tab)?,
                    rect: (
                        tilo_ui_base::num::i32_of_f64(card.x0.round()),
                        tilo_ui_base::num::i32_of_f64(card.y0.round()),
                        tilo_ui_base::num::i32_of_f64(card.width().round()).max(1),
                        tilo_ui_base::num::i32_of_f64(card.height().round()).max(1),
                    ),
                    shadows: tilo_ui_base::surface::floating_lift(theme)
                        .iter()
                        .map(|shadow| frame_of(shadow, radius))
                        .collect(),
                })
            })
            .collect()
    }

    /// A press landed somewhere else — a window over the shell, or another surface of the
    /// session's: the field the person left gives the keyboard up, so what they type next
    /// reaches whatever they pressed on. The shell keeps its record (what was typed stands);
    /// only the caret goes.
    fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {
        self.input = true;
        self.widgets.clear_focus();
    }

    fn pointer_axis(&mut self, scroll: Scroll) {
        self.input = true;
        self.widgets.pointer_axis(scroll);
    }

    fn pointer_modifiers(&mut self, held: tilo_session_core::surface::KeyModifiers) {
        self.widgets.pointer_modifiers(held);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::SurfaceOutput]) {
        // A part not yet told which output it stands on covers all of them, the tightest
        // bounding box there is.
        if self.output.is_some() || self.retired() {
            return;
        }
        let Some((x0, y0, x1, y1)) = outputs
            .iter()
            .map(|output| {
                let (x, y, w, h) = output.rect;
                (x, y, x.saturating_add(w), y.saturating_add(h))
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (
            u32::try_from(x1.saturating_sub(x0)),
            u32::try_from(y1.saturating_sub(y0)),
        ) else {
            return;
        };
        self.output_scale = tilo_session_core::surface::SurfaceOutput::scale_at(
            outputs,
            (
                f64::from(x0) + f64::from(width) / 2.0,
                f64::from(y0) + f64::from(height) / 2.0,
            ),
        );
        self.output = Some((String::new(), (x0, y0, width, height)));
        self.place();
    }

    fn wants_keyboard(&self) -> bool {
        // A sheet of the session's over the desktop — a question, the guide, a sheet's page —
        // takes the keys that are its own: the shell's controls stay behind it.
        if self.origin.borrow().link.sheet_up() {
            return false;
        }
        // The held Super's keys are a picture the shell draws over the windows: they take no key,
        // so the window in front keeps the keyboard while they show and every chord it is holding
        // Super for still runs.
        if self.origin.borrow().link.surface() == tilo_session_core::shell::PEEK_SURFACE {
            return false;
        }
        // The shell under the windows takes the keys when one of its own controls put the caret
        // somewhere it answers: a field of the bar's, or a canvas's place, which pans with the
        // arrow keys (`ui.Map`). A shell with none leaves the keys to the window in front.
        //
        // It also takes them while the desktop's focus is its own (no window holds the
        // keyboard): an empty desktop's bar, dock and surfaces answer the person's keys, and a
        // direction they leave over hands the focus to a window.
        self.cover.over()
            || self.text_input().is_some()
            || self.widgets.takes_keys()
            || (!self.hidden() && self.origin.borrow().link.on_seat())
    }

    /// The regions the shell's translucent material records: the compositor blurs what lies
    /// below the part there and hands it back ([`Self::set_backdrops`]), so a glass bar shows
    /// the wallpaper it stands on rather than its own ground.
    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        ShellSurface::backdrops(&self.widgets)
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        ShellSurface::set_backdrops(&mut self.widgets, images);
    }

    /// A field of the shell has the focus (the bar's search, a settings
    /// field): the input method composes into it.
    fn text_input(&self) -> Option<SurfaceTextInput> {
        (!self.hidden())
            .then(|| self.widgets.text_input())
            .flatten()
    }

    fn ime(&mut self, event: SurfaceIme) {
        self.input = true;
        self.widgets.ime(event);
    }

    fn key(&mut self, key: &SurfaceKey) {
        self.input = true;
        self.took_press();
        let Some(action) = self.switcher_key(key) else {
            // A direction its views have no control left for is the desktop's: the focus leaves
            // the bar or dock for the window it points at. The desktop takes the ring away when
            // a window takes the focus; with none there, the shell keeps it.
            let handled = self.widgets.key_input(key);
            if !handled
                && let Some(step) = step_of(key)
                && let Some(leave) = &self.leave
            {
                leave(step);
            }
            return;
        };
        match action {
            SwitcherKey::Walk { back, app_scope } => {
                // Holding Alt opens it; a further Tab walks it on (and back with Shift).
                let steps = if back { -1 } else { 1 };
                let scope = if app_scope { "app" } else { "workspace" };
                self.ask(
                    "walk-switcher",
                    &[("steps", steps.into()), ("scope", scope.into())],
                );
            }
            SwitcherKey::Commit => self.ask("commit-switcher", &[]),
            SwitcherKey::Close => {
                let up = self.origin.borrow().link.surface();
                self.ask("close-surface", &[("part", up.into())]);
            }
        }
    }

    fn cursor(&self) -> CursorIcon {
        self.widgets.cursor()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        // What the window owes the app goes over before the part settles: it asks for the
        // frame that hands it over rather than sleeping on it.
        if self.owes() {
            return Some(std::time::Instant::now());
        }
        // A reading the session published that the part has not taken, or one the views wait
        // on that the session has not been asked for: the next frame takes it, or asks.
        if self.origin.borrow().due() || self.origin.borrow().link.wants() {
            return Some(std::time::Instant::now());
        }
        // A package a view read a type of and the session has not been asked for yet.
        if self.origin.borrow().link.wants_packages() {
            return Some(std::time::Instant::now());
        }
        // The session's own deadline (the switcher's panel coming up) is due as soon as it is.
        self.widgets
            .next_deadline()
            .into_iter()
            .chain(self.deadline)
            .min()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.widgets.set_frame_interval(interval);
    }

    fn prepare_frame(&mut self) -> bool {
        // What the views draw reads its pictures from the content the session lent, and the
        // pictures load on the toolkit's own threads: it is lent for the process, not this call.
        ui::lend_content(self.content.clone());
        let input = std::mem::take(&mut self.input);
        // A surface the shell shows, or a menu it opened, makes the part cover the outputs:
        // over them is what lets a press outside it close it.
        self.follow_cover();
        if input || self.due() {
            self.turn();
        }
        let mut drew = self.draw();
        // The frame's layout gave the views their space: the app hears of it now, so the next
        // frame shows what a view makes of the space it has, and a part nobody touches is told
        // it rather than waiting for the person's first press.
        if self.owes() && self.turn() {
            drew |= self.draw();
        }
        // The toolkit takes input in at its frame: what the person pressed reports there, and
        // the instance hears of it now rather than at the next input.
        if input {
            if self.turn() {
                drew |= self.draw();
            }
            // The toolkit took the input in: a menu the part opened, or a surface of the
            // shell's, may have opened or closed with it.
            self.follow_cover();
            if self.draw() {
                drew = true;
            }
        }
        // The frame laid the style's own chrome out: the room it leaves the person's widgets is
        // measured from it and the views run once more with it, so a widget never stands under
        // the bar or the dock — not on a first frame, and not while either changes size.
        if self.measure_room() && self.turn() {
            drew |= self.draw();
        }
        // What the person's wheel or fingers asked of the row in front is the pan the desktop
        // keeps for it: the row they see and the pan beside the tree are one state.
        self.present_pan_asks();
        drew
    }
}

impl super::ShellPart {
    /// Present what the person's wheel or fingers asked of the row in front: a bound scroll
    /// reports the offset it was taken to, and that is the pan the desktop keeps for the strip, so
    /// the row the person sees and the pan beside the tree are one state.
    fn present_pan_asks(&mut self) {
        let Some(desktop) = &self.floats else {
            return;
        };
        // The row's own pixels: the pan beside the tree measures each window of the row at least
        // `44rem` and the gap between two at `1.5rem`, and a rem is what the shell draws text at.
        desktop.set_pan_rem(ui::widget::arrange::rem_of(self.widgets.theme()));
        let asked = self.widgets.take_pan_asks();
        if asked.is_empty() {
            return;
        }
        let Some(pan) = desktop.presentation().pan else {
            return;
        };
        for offset in asked {
            let offset = offset.clamp(0.0, pan.max);
            // A strip that went since the person's hand moved refuses the pan: the next report
            // names the one in front then.
            let _ = desktop.dispatch(tilo_session_core::desktop::navigation::ChromeEffect::Pan {
                strip: pan.strip.clone(),
                offset,
            });
        }
    }

    /// A floating window's bar the person lifted, as its frame carries it on the toolkit's drag
    /// bus: the desktop carries that window from where the press took it, and it follows the hand
    /// until it is let go.
    fn follow_lifted_float(&self) {
        let Some(desktop) = &self.floats else {
            return;
        };
        let Some(flight) = ui::widget::drag_bus::current() else {
            return;
        };
        let (x, y) = self.pointer;
        if desktop.carried_tab().is_none() {
            let tilo_ui_base::dnd::DragPayload::Custom { kind, blob } = &flight.payload else {
                return;
            };
            // What any other lift carries (a file, a message, a place of another app) is no
            // floating window's bar: only the shell's own place names a tab it draws.
            if kind != ui::widget::RESOURCE_PAYLOAD {
                return;
            }
            let Some(Ok(Ok(tab))) = blob
                .strip_prefix(b"shell:place:")
                .map(|tab| std::str::from_utf8(tab).map(str::parse::<u64>))
            else {
                return;
            };
            let (px, py) = self.pressed_at;
            if !desktop.carry_float_by_bar(tab, ui::kurbo::Point::new(px, py)) {
                return;
            }
        }
        desktop.carry_to(ui::kurbo::Point::new(x, y));
    }
}

/// The direction `key` asks for, when it is an arrow pressed (or repeating): the step the
/// shell's views move the focus by, and the one they leave over when they have no control left
/// that way.
fn step_of(key: &SurfaceKey) -> Option<super::FocusStep> {
    use winit::keyboard::{Key, NamedKey};
    if !key.pressed {
        return None;
    }
    let (name, _) = tilo_ui_base::keys::from_keysym(key.keysym, None);
    match name {
        Key::Named(NamedKey::ArrowUp) => Some(super::FocusStep::Up),
        Key::Named(NamedKey::ArrowDown) => Some(super::FocusStep::Down),
        Key::Named(NamedKey::ArrowLeft) => Some(super::FocusStep::Left),
        Key::Named(NamedKey::ArrowRight) => Some(super::FocusStep::Right),
        _ => None,
    }
}

/// A shadow a floating window's card casts on the windows under it, as the compositor draws it
/// round the card.
fn frame_of(
    shadow: &tilo_ui_base::tokens::Shadow,
    radius: f64,
) -> tilo_session_core::surface::WindowFrame {
    let colour = |color: vello::peniko::Color| color.to_rgba8().to_u8_array();
    tilo_session_core::surface::WindowFrame {
        radius,
        shadow: (shadow.blur > 0.0).then(|| tilo_session_core::surface::Shadow {
            offset_y: f64::from(shadow.offset_y),
            blur: f64::from(shadow.blur),
            rgba: colour(shadow.color),
        }),
        edge: shadow.outline.map(|line| tilo_session_core::surface::Edge {
            width: f64::from(line.width),
            rgba: colour(line.color),
        }),
    }
}
