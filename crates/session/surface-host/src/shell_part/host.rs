//! The part as the compositor's surface host: the layers it lays out, the pixels it shows of
//! the windows over it, and the material it records for the compositor to fill in below.

use super::{
    CursorIcon, HostIme, HostKey, HostLayer, HostTextInput, KeyClaim, Origin, ShellPart,
    SurfaceHost, SurfaceKey,
};

impl SurfaceHost for ShellPart {
    fn layers(&self) -> Vec<HostLayer> {
        // Aside for a fullscreen window, the window has the whole display: not a row of the
        // bar's pixels stands over it.
        if self.hidden() {
            return Vec::new();
        }
        self.widgets.layers()
    }

    /// The whole screen lies under the windows, which stand in its wells; the bar rides over
    /// them. A surface of the desk's (the overview, the switcher, the launcher) or a menu of
    /// the part's stands over the windows instead: while one covers the outputs, the part and
    /// the pictures it names are drawn above every window, as what it shows is the person's
    /// to look at rather than a backdrop behind their windows.
    fn below_windows(&self) -> bool {
        self.widgets.below_windows() && !self.cover
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

    fn claim_key(&mut self, key: &HostKey, locked: bool, _inhibited: bool) -> KeyClaim {
        // A shell's overlays answer the keyboard: Alt+Tab walks the switcher, Enter goes there,
        // and Esc closes whatever surface is up. A locked session is not the shell's to answer.
        if locked || self.surface_key(key).is_none() {
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
        // the menu it closes it, as a press outside any menu does.
        if self.cover
            && let Some((_, (ox, oy, width, height))) = self.output
        {
            let (x, y) = (x - f64::from(ox), y - f64::from(oy));
            return x >= 0.0 && y >= 0.0 && x < f64::from(width) && y < f64::from(height);
        }
        self.widgets.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.input = true;
        self.widgets.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.input = true;
        self.widgets.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input = true;
        self.widgets.pointer_button(button, pressed);
    }

    /// A press landed somewhere else — a window over the shell, or another surface of the
    /// session's: the field the person left gives the keyboard up, so what they type next
    /// reaches whatever they pressed on. The shell keeps its record (what was typed stands);
    /// only the caret goes.
    fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {
        self.input = true;
        self.widgets.clear_focus();
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.input = true;
        self.widgets.pointer_axis(horizontal, vertical);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::HostedOutput]) {
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
        self.output_scale = tilo_session_core::surface::HostedOutput::scale_at(
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
        // The desk under the windows takes the keys when one of its own controls put the caret
        // somewhere it answers: a field of the bar's, or a canvas's place, which pans with the
        // arrow keys (`ui.Map`). A shell with none leaves the keys to the window in front.
        self.cover || self.text_input().is_some() || self.widgets.takes_keys()
    }

    /// The regions the shell's translucent material records: the compositor blurs what lies
    /// below the part there and hands it back ([`Self::set_backdrops`]), so a glass bar shows
    /// the wallpaper it stands on rather than its own ground.
    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        SurfaceHost::backdrops(&self.widgets)
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        SurfaceHost::set_backdrops(&mut self.widgets, images);
    }

    /// A field of the shell has the focus (the bar's search, a settings
    /// field): the input method composes into it.
    fn text_input(&self) -> Option<HostTextInput> {
        (!self.hidden())
            .then(|| self.widgets.text_input())
            .flatten()
    }

    fn ime(&mut self, event: HostIme) {
        self.input = true;
        self.widgets.ime(event);
    }

    fn key(&mut self, key: &HostKey) {
        self.input = true;
        let Some(action) = self.surface_key(key) else {
            self.widgets.key(key);
            return;
        };
        match action {
            SurfaceKey::Walk { back, app_scope } => {
                // Holding Alt opens it; a further Tab walks it on (and back with Shift).
                let steps = if back { -1 } else { 1 };
                let scope = if app_scope { "app" } else { "workspace" };
                self.ask(
                    "walk-switcher",
                    &[("steps", steps.into()), ("scope", scope.into())],
                );
            }
            SurfaceKey::Commit => self.ask("commit-switcher", &[]),
            SurfaceKey::Close => {
                let up = self.origin.borrow().link.surface();
                self.ask("close-surface", &[("surface", up.into())]);
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
        // A surface the desk shows, or a menu it opened, makes the part cover the outputs:
        // standing over them is what lets a press outside it close it.
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
            // desk's, may have opened or closed with it.
            self.follow_cover();
            if self.draw() {
                drew = true;
            }
        }
        drew
    }
}
