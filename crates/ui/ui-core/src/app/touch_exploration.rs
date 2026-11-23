// Explore by touch, and the order the built-in screen reader walks
// (`crate::screen_reader_touch`, docs/security/sign-in.md §3.4).

impl App {
    /// Run `read` with the screen reader and the order it walks: the
    /// retained semantic tree's nodes in reading order, a widget's synthetic
    /// children (a list's rows, a keyboard's keys) among them, each one that
    /// says something and stands on the screen. The order is read again
    /// only after the tree changed. `None` while the reader is off.
    fn with_screen_reader_order<R>(
        &mut self,
        read: impl FnOnce(
            &mut crate::screen_reader::ScreenReader,
            &[crate::screen_reader::ReadingItem],
        ) -> R,
    ) -> Option<R> {
        self.accessibility.screen_reader.as_ref()?;
        let _ = self.refresh_semantic_tree();
        let revision = self.accessibility.semantic_tree_revision;
        if self
            .accessibility
            .reading_order
            .as_ref()
            .is_none_or(|(read_at, _)| *read_at != revision)
        {
            let order = screen_reader_order(&self.accessibility.semantic_tree);
            self.accessibility.reading_order = Some((revision, order));
        }
        let AccessibilityRuntime {
            screen_reader,
            reading_order,
            ..
        } = &mut *self.accessibility;
        let (reader, (_, order)) = screen_reader.as_mut().zip(reading_order.as_ref())?;
        Some(read(reader, order))
    }

    /// A finger on the screen while the built-in screen reader is on: it
    /// explores rather than presses ([`crate::screen_reader_touch`]), and
    /// no widget sees it. `false`, touching nothing, while the reader is
    /// off: the touch then goes to the widgets as it always does.
    /// `scale` turns the contact's physical position into logical pixels.
    #[cfg(feature = "native")]
    pub(crate) fn explore_by_touch(&mut self, touch: &winit::event::Touch, scale: f64) -> bool {
        if self.accessibility.screen_reader.is_none() {
            return false;
        }
        let _entry = self.enter_wake_scope();
        let at = vello::kurbo::Point::new(touch.location.x / scale, touch.location.y / scale);
        let gestures =
            self.accessibility
                .touch_explorer
                .touch(touch.id, touch.phase, at, crate::time::now());
        for gesture in gestures {
            self.apply_explore_gesture(gesture);
        }
        true
    }

    /// Read what a finger's timing completed by `now`: a resting finger
    /// begins to explore, a lone tap is read. `true` when something was.
    fn tick_touch_exploration(&mut self, now: crate::time::Instant) -> bool {
        if self.accessibility.screen_reader.is_none() {
            return false;
        }
        let gestures = self.accessibility.touch_explorer.poll(now);
        let any = !gestures.is_empty();
        for gesture in gestures {
            self.apply_explore_gesture(gesture);
        }
        any
    }

    fn apply_explore_gesture(&mut self, gesture: crate::screen_reader_touch::ExploreGesture) {
        use crate::screen_reader_touch::ExploreGesture;
        match gesture {
            ExploreGesture::Explore(at) => {
                self.with_screen_reader_order(|reader, order| {
                    reader.explore_in(order, at);
                });
            }
            ExploreGesture::Next => {
                self.with_screen_reader_order(|reader, order| {
                    reader.read_next_in(order);
                });
            }
            ExploreGesture::Previous => {
                self.with_screen_reader_order(|reader, order| {
                    reader.read_prev_in(order);
                });
            }
            ExploreGesture::Activate => self.activate_screen_reader_cursor(),
        }
    }

    /// A double tap: activate the item the reading cursor is on through its
    /// accessible action, the path an assistive technology's request takes
    /// ([`Self::invoke_semantic_action`]), so a key is pressed as a tap on
    /// it presses it. An item that cannot be activated but can take focus
    /// is focused, as a double tap on a text field does.
    fn activate_screen_reader_cursor(&mut self) {
        let Some(id) = self
            .with_screen_reader_order(|reader, order| {
                reader.cursor_item(order).and_then(|item| item.id.clone())
            })
            .flatten()
        else {
            return;
        };
        let Some(action) = self.semantic_tree().node(&id).and_then(|node| {
            ["activate", "focus"]
                .into_iter()
                .find(|name| node.action(name).is_some())
        }) else {
            return;
        };
        if let Err(error) = self.invoke_semantic_action(&id, action, None) {
            tracing::debug!(%error, id = id.as_str(), "the screen reader could not activate");
        }
    }
}

/// The nodes of `tree` the screen reader stops at, in reading order: each
/// that says something, not hidden nor inside something hidden, and placed
/// on the screen, itself or through what it is part of (a synthetic row with
/// no rect of its own is read where its list is). The action catalogue's
/// resource nodes, which stand nowhere, are left out.
fn screen_reader_order(
    tree: &crate::a11y_tree::SemanticTree,
) -> Vec<crate::screen_reader::ReadingItem> {
    let mut order = Vec::new();
    // Depth-first, children in reading order; the flag says whether an
    // ancestor stands on the screen.
    let mut stack = vec![(tree.root().clone(), false)];
    while let Some((id, placed_above)) = stack.pop() {
        let Some(node) = tree.node(&id) else {
            continue;
        };
        if node.state.hidden {
            continue;
        }
        let placed = placed_above || node.bounds.is_some();
        if placed && id != *tree.root() {
            let mut access = crate::a11y::AccessNode::new(node.role).with_state(node.state);
            access.name.clone_from(&node.name);
            access.value.clone_from(&node.value);
            access.bounds = node.bounds.map(Into::into);
            access.pos_in_set = node.place.pos_in_set;
            access.set_size = node.place.set_size;
            access.level = node.place.level;
            if !crate::screen_reader::describe(&access).is_empty() {
                order.push(crate::screen_reader::ReadingItem {
                    id: Some(id.clone()),
                    node: access,
                });
            }
        }
        stack.extend(
            node.children
                .iter()
                .rev()
                .map(|child| (child.clone(), placed)),
        );
    }
    order
}
