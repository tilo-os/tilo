// What a window's own content offers the shell by name, and the press that
// comes back (`crate::window_targets`).
//
// A window drawn by another process publishes nothing of the shell's: the app
// describes its controls over its `shell-ui` connection and answers a press on
// one through `App::dispatch_a11y_action`, which is the path an assistive
// technology's activation takes in the same window. One path, so a control a
// person presses, a screen reader activates and a session names are the same
// control.

#[cfg(feature = "native")]
use crate::window_targets::{MAX_TARGETS, TargetAction, WindowTarget};

impl App {
    /// The controls of this window's content as its accessibility tree
    /// exports them now, in reading order, each with where it stands in
    /// logical pixels and how wide and tall it is, and what it does
    /// ([`WindowTarget::identity`]) when it runs an app action: what a
    /// surface of the session names and presses, a sheet's and an app's
    /// window alike.
    ///
    /// The same list [`Self::window_targets`] hands a window's own app to
    /// the shell, read with one rule.
    #[cfg(feature = "native")]
    #[must_use]
    pub fn window_controls(&mut self) -> Vec<crate::window_targets::WindowControl> {
        self.window_targets()
            .into_iter()
            .map(|target| {
                let [x0, y0, x1, y1] = target.rect;
                crate::window_targets::WindowControl {
                    name: target.name,
                    identity: target.identity,
                    rect: (x0, y0, x1 - x0, y1 - y0),
                    level: target.level,
                    selected: target.selected,
                }
            })
            .collect()
    }

    /// The controls of this window's content as its accessibility tree
    /// exports them now, in reading order: named ones, with where each is
    /// drawn and what it answers.
    ///
    /// Read from the tree itself, so a control the window does not draw, one
    /// it hides, and one it answers nothing on is never offered.
    ///
    /// What the window says beside its controls — a status line, whose seat
    /// is the person's, a heading — and the regions it names, as a map of the
    /// world stands in the Maps window, follow them, as far as the room left
    /// allows: a person reads it there, and so does an agent or a test that
    /// asks what the window shows. The controls come first, so a long page of
    /// words never crowds out one of them; and a control's own words are not
    /// said again.
    #[cfg(feature = "native")]
    fn window_targets(&mut self) -> Vec<WindowTarget> {
        let mut targets = Vec::new();
        let mut words: Vec<WindowTarget> = Vec::new();
        for (_, described) in self.build_a11y_tree_update(1.0).nodes {
            if targets.len() == MAX_TARGETS {
                break;
            }
            if described.role() == crate::accesskit::Role::Window || described.is_hidden() {
                continue;
            }
            // A control is known by its name, or — where it invokes an app action or edits a
            // field and the toolkit gives it no words of its own — by what it does.
            let identity = described
                .author_id()
                .map(str::trim)
                .filter(|identity| !identity.is_empty());
            // Words the window says carry them as their value where they have no label; a
            // region it names says what stands there (the map of the world, a board).
            let says = matches!(
                described.role(),
                crate::accesskit::Role::Label
                    | crate::accesskit::Role::Paragraph
                    | crate::accesskit::Role::Heading
                    | crate::accesskit::Role::Group
            );
            let name = described
                .label()
                .or_else(|| says.then(|| described.value()).flatten())
                .map(str::trim)
                .filter(|name| !name.is_empty());
            if name.is_none() && identity.is_none() {
                continue;
            }
            let Some(bounds) = described.bounds() else {
                continue;
            };
            if !(bounds.x0.is_finite()
                && bounds.y0.is_finite()
                && bounds.x1.is_finite()
                && bounds.y1.is_finite())
                || bounds.x1 <= bounds.x0
                || bounds.y1 <= bounds.y0
            {
                continue;
            }
            let actions: Vec<TargetAction> =
                [TargetAction::Activate, TargetAction::ShowContextMenu]
                    .into_iter()
                    .filter(|action| described.supports_action(action.accesskit()))
                    .collect();
            // What a region says — a heading, a sentence, a place that holds nothing — is words
            // however it is reached: a region that can hold the focus is still no control, and
            // its line under the title is what a person reads there.
            if identity.is_none() && says {
                if let Some(name) = name {
                    words.push(WindowTarget {
                        role: format!("{:?}", described.role()),
                        name: name.to_owned(),
                        identity: None,
                        rect: [bounds.x0, bounds.y0, bounds.x1, bounds.y1],
                        actions: Vec::new(),
                        level: described.level(),
                        selected: false,
                    });
                    // What a region says stands there beyond its name — an empty state's
                    // message under its title — is what a person reads there, so a caller
                    // reads it too.
                    if let Some(said) = described.description() {
                        let said = said.trim();
                        if !said.is_empty() && said != name {
                            words.push(WindowTarget {
                                role: format!("{:?}", described.role()),
                                name: said.to_owned(),
                                identity: None,
                                rect: [bounds.x0, bounds.y0, bounds.x1, bounds.y1],
                                actions: Vec::new(),
                                level: described.level(),
                                selected: false,
                            });
                        }
                    }
                }
                continue;
            }
            // One operation offered twice as the same control is one control to name: where a
            // window shows the same button in more than one place — the bar's primary and the
            // empty state that presents it — a caller that asks for it gets one answer, the first
            // the window reads. Two different controls that run one action stay two: a composer's
            // field sends on Enter and its Send button sends on a press, and a person, a screen
            // reader and an agent each reach the one they mean.
            let role = format!("{:?}", described.role());
            if let Some(identity) = identity
                && targets.iter().any(|known: &WindowTarget| {
                    known.identity.as_deref() == Some(identity)
                        && known.role == role
                        && known.name == name.unwrap_or_default()
                })
            {
                continue;
            }
            targets.push(WindowTarget {
                role,
                name: name.unwrap_or_default().to_owned(),
                identity: identity.map(str::to_owned),
                rect: [bounds.x0, bounds.y0, bounds.x1, bounds.y1],
                actions,
                level: described.level(),
                // A strip's chosen segment carries its choice as a toggle's on state, a swatch as
                // a list item's selected one: both mean the same to a caller, that this control
                // is the chosen one.
                selected: described.is_selected().unwrap_or(false)
                    || matches!(described.toggled(), Some(crate::accesskit::Toggled::True)),
            });
        }
        for said in words {
            if targets.len() == MAX_TARGETS {
                break;
            }
            // The words a control carries are its name already: a button's label is the
            // button, not a line of its own beside it.
            let [x0, y0, x1, y1] = said.rect;
            let carried = targets.iter().any(|control: &WindowTarget| {
                let [cx0, cy0, cx1, cy1] = control.rect;
                control.name == said.name && x0 >= cx0 && y0 >= cy0 && x1 <= cx1 && y1 <= cy1
            });
            if !carried {
                targets.push(said);
            }
        }
        targets
    }

    /// Act on the control `name` of this window's content, of `role`, as
    /// `action` says, through the assistive technology path: a control that
    /// does not handle it itself is pressed where it is drawn, as the app's
    /// own window answers a screen reader. A control the window no longer
    /// shows acts nothing.
    ///
    /// It is found in the tree as it stands now, not by an id a description
    /// handed out earlier: a layout between the two renumbers the window's
    /// nodes, and the name it goes by is what the person and the session see.
    /// A caller that names it by `identity` names what it does instead, which
    /// the window's composition does not move and no language renames.
    #[cfg(feature = "native")]
    fn press_window_target(
        &mut self,
        name: &str,
        role: &str,
        identity: Option<&str>,
        action: TargetAction,
    ) -> bool {
        let verb = match action {
            TargetAction::Activate => tilo_ui_base::a11y::Action::Activate,
            TargetAction::ShowContextMenu => tilo_ui_base::a11y::Action::ShowContextMenu,
        };
        // The name a caller reads is the one described, trimmed as it was
        // listed, so a description padded around its text still presses. What
        // it does names it on its own: the role is the caller's reading of it,
        // and a control that does one thing is the one control.
        let shows = |described: &crate::accesskit::Node| {
            let known = match identity {
                Some(identity) => described.author_id() == Some(identity),
                None => {
                    described.label().map(str::trim) == Some(name)
                        && format!("{:?}", described.role()) == role
                }
            };
            known && described.supports_action(action.accesskit())
        };
        let Some(node) = self
            .build_a11y_tree_update(1.0)
            .nodes
            .into_iter()
            .find(|(_, described)| shows(described))
            .map(|(node, _)| node)
        else {
            return false;
        };
        let outcome = self.dispatch_a11y_action(node, &verb);
        if let Some((x, y)) = outcome {
            self.dispatch_semantic_click(x, y);
        }
        true
    }

    /// Act on the control that does `identity` in this window's content, as
    /// `action` says: the one route [`Self::press_window_target`] takes, found
    /// by what the control does.
    #[cfg(feature = "native")]
    pub fn press_window_control(&mut self, identity: &str, action: TargetAction) -> bool {
        self.press_window_target("", "", Some(identity), action)
    }

    /// Tell the shell what the window's content offers by name, when the tree
    /// it is read from changed: a pass that changed nothing sends nothing and
    /// walks nothing.
    #[cfg(feature = "native")]
    fn send_window_targets(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        instance: Option<u64>,
    ) -> Result<(), String> {
        let Some(targets) = self.window_targets_to_tell() else {
            return Ok(());
        };
        client.send(
            &crate::floating_surface::FloatingSurfaceClientMessage::WindowTargets {
                targets,
                instance,
            },
        )
    }

    /// Tell the shell what assistive technology reads of this window, when the
    /// tree it is read from changed: the same revision the controls are read
    /// at, so a pass that changed nothing sends nothing and walks nothing. The
    /// session publishes it for this window over AT-SPI and sends back what a
    /// client asks ([`Self::receive_accessibility_action`]); off a session the
    /// window publishes it itself, through the platform adapter.
    #[cfg(feature = "native")]
    fn send_accessibility_tree(
        &mut self,
        client: &mut FloatingSurfaceServiceClient,
        instance: Option<u64>,
    ) -> Result<(), String> {
        if !self.semantic_tree_current() {
            self.refresh_semantic_tree();
        }
        let revision = self.accessibility.semantic_tree_revision;
        if self.floating.accessibility_told == Some(revision) {
            return Ok(());
        }
        self.floating.accessibility_told = Some(revision);
        let name = self
            .window
            .shown_title
            .clone()
            .or_else(|| self.window.title.clone())
            .unwrap_or_default();
        let scale = self.accessibility.export_scale;
        let mut tree = self.build_a11y_tree_update(scale);
        if !name.is_empty() {
            tilo_ui_base::accesskit_bridge::name_window(&mut tree, &name);
        }
        client.send(
            &crate::floating_surface::FloatingSurfaceClientMessage::AccessibilityTree {
                tree,
                name,
                instance,
            },
        )
    }

    /// What this window's content offers by name, when it differs from what
    /// the shell was told last: `None` for a pass whose tree changed nothing.
    /// What it answers is what the shell is told from now on.
    #[cfg(feature = "native")]
    fn window_targets_to_tell(&mut self) -> Option<Vec<WindowTarget>> {
        // A tree its own consumers already refreshed on this pass is read as
        // it stands; one something marked stale is walked here, as they would.
        if !self.semantic_tree_current() {
            self.refresh_semantic_tree();
        }
        let revision = self.accessibility.semantic_tree_revision;
        // What the window shows is its tree as the last layout pass placed it: a control stands
        // where that pass put it. A pass that laid the tree out again is read again, though the
        // tree says the same words: the shell is told what the window shows, never what it
        // showed before the paint.
        let at = (revision, self.layout.layouts);
        if self.floating.window_targets_told == Some(at) {
            return None;
        }
        // What this window's own controls are, as they stand at `at`.
        let targets = self.window_targets();
        self.floating.window_targets_told = Some(at);
        if self.floating.window_targets_sent.was(&targets) {
            return None;
        }
        self.floating.window_targets_sent = Told::Was(targets.clone());
        Some(targets)
    }

    /// A surface app's window, which the session places itself and reaches
    /// over the surface's actions socket rather than a `shell-ui` connection
    /// ([`crate::surface_peer`]): it is told what the window offers by name,
    /// and the presses it asked of those controls are acted on here, as a
    /// desktop window acts on the shell's. Whether a press changed anything.
    #[cfg(all(feature = "native", feature = "model", feature = "local-daemons"))]
    pub(super) fn serve_surface_window(&mut self) -> bool {
        if !crate::surface_peer::hosted() {
            return false;
        }
        let mut pressed = false;
        for press in crate::surface_peer::take_presses() {
            pressed |= self.press_window_target(
                &press.name,
                &press.role,
                press.identity.as_deref(),
                press.action,
            );
        }
        if let Some(targets) = self.window_targets_to_tell() {
            crate::surface_peer::publish_targets(targets);
        }
        pressed
    }
}
