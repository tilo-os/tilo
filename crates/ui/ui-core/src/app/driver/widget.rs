//! What a widget's tests reach through the driver: the controls it exposes and operates, and input
//! handed to the root under a modifier state of its own.

use vello::kurbo::Point;

use super::Driver;
use winit::event::WindowEvent;

impl Driver {
    /// The control `id` names, if the current frame has it.
    #[must_use]
    pub fn control(&self, id: &str) -> Option<crate::headless::Control> {
        self.controls().into_iter().find(|control| control.id == id)
    }

    /// Operate the control `id` as a person would.
    ///
    /// # Errors
    /// When the frame has no such control, it is disabled, or it refuses the input.
    pub fn operate_control(
        &mut self,
        id: &str,
        input: &crate::headless::ControlInput,
    ) -> Result<(), String> {
        let before = self.control_failures().len();
        let notice_before = self.refusal_said();
        self.operate_control_now(id, input)?;
        let failures = self.control_failures();
        if let Some(new) = failures.get(before..).filter(|new| !new.is_empty()) {
            return Err(new.join("; "));
        }
        // A control that refuses what it was told says so over it, as a person's refused press
        // does.
        match self.refusal_said() {
            Some(said) if Some(&said) != notice_before.as_ref() => Err(said),
            _ => Ok(()),
        }
    }

    /// [`Self::operate_control`] without reading back what the app refused.
    fn operate_control_now(
        &mut self,
        id: &str,
        input: &crate::headless::ControlInput,
    ) -> Result<(), String> {
        use tilo_ui_base::a11y::Action;
        use crate::headless::ControlInput;
        let control = self
            .control(id)
            .ok_or_else(|| format!("no control {id:?} in the current frame"))?;
        if control.disabled {
            return Err(format!("control {id:?} is disabled"));
        }
        let node = accesskit::NodeId(
            id.parse()
                .map_err(|_| format!("{id:?} is not a control id"))?,
        );
        let has = |verb: &str| control.verbs.iter().any(|own| own == verb);
        let centre = || {
            control
                .bounds
                .filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
                .map(|bounds| bounds.center())
                .ok_or_else(|| format!("control {id:?} has no on-screen area"))
        };
        let verb = |name: &str, action: Action| {
            if has(name) {
                Ok(action)
            } else {
                Err(format!("control {id:?} does not answer {name}"))
            }
        };
        match input {
            ControlInput::Press if has("activate") => {
                self.route_control(id, node, &centre(), &Action::Activate)
            }
            ControlInput::Press => {
                let centre = centre()?;
                self.click_at(centre.x, centre.y);
                Ok(())
            }
            ControlInput::Focus => self.route_control(id, node, &centre(), &Action::Focus),
            ControlInput::SetValue(value) => {
                verb("set-value", Action::Focus)
                    .map_err(|error| format!("{error}; type into it instead"))?;
                match self.perform_a11y_action(node, &Action::SetValue(value.clone())) {
                    Err(error) if error.starts_with("no accessibility node") => {
                        self.route_control(id, node, &centre(), &Action::Focus)?;
                        self.select_all_text();
                        self.commit_text(value);
                        Ok(())
                    }
                    other => other,
                }
            }
            ControlInput::TypeText(text) => {
                if has("focus") {
                    self.route_control(id, node, &centre(), &Action::Focus)?;
                } else {
                    let centre = centre()?;
                    self.click_at(centre.x, centre.y);
                }
                self.commit_text(text);
                Ok(())
            }
            ControlInput::Scroll { dx, dy } => {
                let centre = centre()?;
                self.hover_at(centre.x, centre.y);
                self.scroll(*dx, *dy);
                Ok(())
            }
            ControlInput::Toggle => {
                let action = verb("activate", Action::ToggleCheck)?;
                self.route_control(id, node, &centre(), &action)
            }
            ControlInput::Increment => {
                let action = verb("increment", Action::Increment)?;
                self.route_control(id, node, &centre(), &action)
            }
            ControlInput::Decrement => {
                let action = verb("decrement", Action::Decrement)?;
                self.route_control(id, node, &centre(), &action)
            }
            ControlInput::Expand => {
                let action = verb("expand", Action::Expand)?;
                self.route_control(id, node, &centre(), &action)
            }
            ControlInput::Collapse => {
                let action = verb("collapse", Action::Collapse)?;
                self.route_control(id, node, &centre(), &action)
            }
        }
    }

    /// Act `action` on the control `node`: the widget takes it, and what it leaves to the pointer is
    /// pressed where the control stands.
    fn route_control(
        &mut self,
        id: &str,
        node: accesskit::NodeId,
        centre: &Result<Point, String>,
        action: &tilo_ui_base::a11y::Action,
    ) -> Result<(), String> {
        match self.perform_a11y_action(node, action) {
            Err(error) if error.starts_with("no accessibility node") => {
                if !matches!(
                    action,
                    tilo_ui_base::a11y::Action::Activate
                        | tilo_ui_base::a11y::Action::ToggleCheck
                        | tilo_ui_base::a11y::Action::Focus
                ) {
                    return Err(format!(
                        "control {id:?} is reachable only by pointer, which cannot {action:?}"
                    ));
                }
                let centre = centre.clone()?;
                self.click_at(centre.x, centre.y);
                Ok(())
            }
            other => other,
        }
    }

    fn select_all_text(&mut self) {
        self.set_modifiers(&["command".to_owned()]);
        self.key("a");
        self.set_modifiers(&[]);
    }

    /// Hand `event` to the app without running a frame, under `modifiers` for that one event.
    /// Whether anything handled it.
    pub fn dispatch_to_root_with(
        &mut self,
        event: &WindowEvent,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        let previous = self.app.input.modifiers;
        self.app.set_surface_modifiers(*modifiers);
        let handled = self.dispatch_to_root(event);
        self.app.set_surface_modifiers(previous);
        handled
    }
}

impl Driver {
    /// The people and agents shown over the app's frame, each as a node where its caret or
    /// selection stands: what a frame paints over the widgets is no widget of the tree, and a
    /// reader is told where it is all the same.
    pub(super) fn presence_inspection(&self) -> Vec<crate::headless::InspectionNode> {
        let mut drawn: Vec<tilo_ui_base::presence::PresenceMark> = Vec::new();
        for mark in self.app.presence_ctx.presence_marks() {
            match drawn.iter().position(|drawn| drawn.user == mark.user) {
                Some(index) => {
                    if let Some(drawn) = drawn.get_mut(index) {
                        drawn.rect = drawn.rect.union(mark.rect);
                        drawn.selection &= mark.selection;
                        if drawn.saying.is_none() {
                            drawn.saying = mark.saying;
                        }
                    }
                }
                None => drawn.push(mark),
            }
        }
        drawn
            .into_iter()
            .filter(|mark| mark.rect.width() > 0.0 && mark.rect.height() > 0.0)
            .map(|mark| {
                let spoken = tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Image)
                    .with_name(self.app.presence_ctx.mark_words(&mark))
                    .with_bounds(mark.rect);
                crate::headless::InspectionNode {
                    path: format!("presence-mark-{}", mark.user.id()),
                    widget: "PresenceMark".into(),
                    test_id: Some("presence-mark".into()),
                    focusable: false,
                    rect: [
                        mark.rect.x0,
                        mark.rect.y0,
                        mark.rect.width(),
                        mark.rect.height(),
                    ],
                    child_clip: None,
                    page_title: None,
                    layout: None,
                    paint_key: None,
                    semantics: Some(spoken),
                    children: Vec::new(),
                }
            })
            .collect()
    }
}
