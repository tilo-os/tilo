// What a window's own content offers the desktop by name, and the press that
// comes back (`crate::window_targets`).
//
// A window drawn by another process publishes nothing of the desktop's: the app
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
    /// the desktop, read with one rule.
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
    #[cfg(feature = "native")]
    fn window_targets(&mut self) -> Vec<WindowTarget> {
        let mut targets = Vec::new();
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
            let name = described
                .label()
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
            // A control that answers nothing is still listed where it says what it does: a field
            // edits a record without answering a press, and the person reaches it by a point.
            if actions.is_empty() && identity.is_none() {
                continue;
            }
            targets.push(WindowTarget {
                role: format!("{:?}", described.role()),
                name: name.unwrap_or_default().to_owned(),
                identity: identity.map(str::to_owned),
                rect: [bounds.x0, bounds.y0, bounds.x1, bounds.y1],
                actions,
            });
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
            TargetAction::Activate => crate::a11y::Action::Activate,
            TargetAction::ShowContextMenu => crate::a11y::Action::ShowContextMenu,
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
        // A tree its own consumers already refreshed on this pass is read as
        // it stands; one something marked stale is walked here, as they would.
        if !self.semantic_tree_current() {
            self.refresh_semantic_tree();
        }
        let revision = self.accessibility.semantic_tree_revision;
        if self.floating.window_targets_told == Some(revision) {
            return Ok(());
        }
        // What this window's own controls are, as they stand at `revision`.
        let targets = self.window_targets();
        self.floating.window_targets_told = Some(revision);
        if self.floating.window_targets_sent.was(&targets) {
            return Ok(());
        }
        client.send(
            &crate::floating_surface::FloatingSurfaceClientMessage::WindowTargets {
                targets: targets.clone(),
                instance,
            },
        )?;
        self.floating.window_targets_sent = Told::Was(targets);
        Ok(())
    }
}
