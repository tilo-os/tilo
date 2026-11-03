// What a window's own content offers the desktop by name, and the press that
// comes back (`crate::window_targets`).
//
// A window drawn by another process publishes nothing of the desktop's: the app
// describes its controls over its `shell-ui` connection and answers a press on
// one through `App::dispatch_a11y_action`, which is the path an assistive
// technology's activation takes in the same window. One path, so a control a
// person presses, a screen reader activates and a session names are the same
// control.

use crate::window_targets::{MAX_TARGETS, TargetAction, WindowTarget};

impl App {
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
            let Some(name) = described
                .label()
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
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
            if actions.is_empty() {
                continue;
            }
            targets.push(WindowTarget {
                role: format!("{:?}", described.role()),
                name: name.to_owned(),
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
    #[cfg(feature = "native")]
    fn press_window_target(&mut self, name: &str, role: &str, action: TargetAction) {
        let verb = match action {
            TargetAction::Activate => crate::a11y::Action::Activate,
            TargetAction::ShowContextMenu => crate::a11y::Action::ShowContextMenu,
        };
        // The name a caller reads is the one described, trimmed as it was
        // listed, so a description padded around its text still presses.
        let shows = |described: &crate::accesskit::Node| {
            format!("{:?}", described.role()) == role
                && described.label().map(str::trim) == Some(name)
                && described.supports_action(action.accesskit())
        };
        let Some(node) = self
            .build_a11y_tree_update(1.0)
            .nodes
            .into_iter()
            .find(|(_, described)| shows(described))
            .map(|(node, _)| node)
        else {
            return;
        };
        let outcome = self.dispatch_a11y_action(node, &verb);
        if let Some((x, y)) = outcome {
            self.dispatch_semantic_click(x, y);
        }
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
