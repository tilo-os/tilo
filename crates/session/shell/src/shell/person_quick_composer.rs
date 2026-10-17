//! Private drafts anchored to a certified remote person's actual cursor.
//! No Chat document is touched: unavailable private transport must not fall
//! back to a public room channel, a profile, or a fabricated successful send.

#[cfg(test)]
mod action_tests;
mod actions;

/// Tracks only real input to the retained shared editor. Focus alone is not
/// editing; selection, a context-menu press and IME preedit are.
#[derive(Default)]
struct ComposerAnchor {
    frozen: bool,
    pointer: Option<vello::kurbo::Point>,
}

impl ComposerAnchor {
    fn observe(
        &mut self,
        editor: &dyn Widget,
        event: &winit::event::WindowEvent,
        ctx: &ui::widget::EventCtx<'_>,
    ) {
        use winit::event::{ElementState, Ime, TouchPhase, WindowEvent};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(ctx.logical(position));
            }
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                self.frozen |= self.pointer.is_some_and(|p| editor.get_rect().contains(p));
            }
            WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
                self.frozen |= editor.get_rect().contains(ctx.logical(touch.location));
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                use winit::keyboard::{Key, NamedKey};
                let editing = matches!(
                    event.logical_key,
                    Key::Character(_)
                        | Key::Named(
                            NamedKey::ArrowLeft
                                | NamedKey::ArrowRight
                                | NamedKey::ArrowUp
                                | NamedKey::ArrowDown
                                | NamedKey::Home
                                | NamedKey::End
                                | NamedKey::PageUp
                                | NamedKey::PageDown
                                | NamedKey::Backspace
                                | NamedKey::Delete
                                | NamedKey::Enter
                                | NamedKey::Insert
                                | NamedKey::ContextMenu
                        )
                ) || event.text.as_ref().is_some_and(|text| !text.is_empty());
                self.frozen |= editor.is_focused() && editing;
            }
            WindowEvent::Ime(Ime::Preedit(..) | Ime::Commit(..)) => {
                self.frozen |= editor.is_focused();
            }
            _ => {}
        }
    }
}

fn composer_editor<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if widget.test_id() == Some(id) {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| composer_editor(child, id))
}
use super::i18n::{t, t_args};
use std::collections::HashMap;
use ui::widget::{
    Button, Column, Container, Decorator, Popover, PopoverSide, Row, Text, TextInput, Widget,
};
use ui::{LocalState, Theme};
use vello::kurbo::Rect;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PersonDraftOwner {
    pub workspace: String,
    pub person_key: String,
    pub caller: String,
    pub identity_revision: u64,
}

pub(crate) struct PersonQuickComposer {
    anchor: ComposerAnchor,
    popover: Popover<Container>,
    owner: Option<PersonDraftOwner>,
    draft: LocalState<String>,
    title: LocalState<String>,
    unavailable: LocalState<String>,
    close: LocalState<bool>,
    // A renewed device certificate must not orphan the same person's draft.
    // The revision fences invocation, not private conversation identity.
    drafts: HashMap<(String, String, String), String>,
    focus_reset: bool,
    enabled: LocalState<bool>,
    send: LocalState<bool>,
    expand: LocalState<bool>,
    expansion_editor: Option<ui::private_draft::Editor>,
    generation: u64,
    composition_generation: u64,
    call: Option<actions::Call>,
}

impl Default for PersonQuickComposer {
    fn default() -> Self {
        let draft = LocalState::new(String::new());
        let title = LocalState::new(String::new());
        let unavailable = LocalState::new(t("person-quick-unavailable"));
        let close = LocalState::new(false);
        let closing = close.clone();
        let enabled = LocalState::new(false);
        let send = LocalState::new(false);
        let expand = LocalState::new(false);
        let sending = send.clone();
        let expanding = expand.clone();
        let bar = Row::new()
            .no_wrap()
            .push(Box::new(
                Text::from_state(&title, Clone::clone).with_role(ui::TextRole::Caption),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(0.0)
                    .with_child(Box::new(
                        TextInput::from_state(&draft)
                            .with_id("person-quick-editor")
                            .with_placeholder(t("person-quick-placeholder"))
                            .with_accessible_name(t("person-quick-editor-name")),
                    )),
            ))
            .push(Box::new(
                Button::icon_only(
                    "person-quick-send",
                    ui::icons::Icon::Send,
                    &t("person-quick-send"),
                )
                .enabled_from(&enabled)
                .on_click(move || sending.set(true)),
            ))
            .push(Box::new(
                Button::icon_only(
                    "person-quick-expand",
                    ui::icons::Icon::MessageSquare,
                    &t("person-quick-expand"),
                )
                .enabled_from(&enabled)
                .on_click(move || expanding.set(true)),
            ))
            .push(Box::new(
                Button::icon_only(
                    "person-quick-close",
                    ui::icons::Icon::X,
                    &t("person-quick-close"),
                )
                .on_click(move || closing.set(true)),
            ));
        let content = Column::new()
            .with_no_padding()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(bar))
            .push(Box::new(
                Text::from_state(&unavailable, Clone::clone)
                    .with_id("person-quick-status")
                    .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                    .dimmed(),
            ));
        let popover = Popover::with_content(
            Container::new()
                .with_no_padding()
                .with_child(Box::new(content)),
        )
        .transparent_content()
        .side(PopoverSide::Right)
        .fallback_side(PopoverSide::Below)
        .confined_to_viewport()
        .with_label(t("person-quick-accessible-name"));
        Self {
            anchor: ComposerAnchor::default(),
            popover,
            owner: None,
            draft,
            title,
            unavailable,
            close,
            drafts: HashMap::new(),
            focus_reset: false,
            enabled,
            send,
            expand,
            expansion_editor: None,
            generation: 0,
            composition_generation: 0,
            call: None,
        }
    }
}

impl PersonQuickComposer {
    pub(crate) fn connect(&mut self, source: super::certified_action_session::ActionSessionSource) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        if let Ok(session) = {
            let session = source.open(&owner.workspace, Some(&owner.caller));
            drop(source);
            session
        } {
            self.enabled.set(true);
            // Opening is a real pair open, not a profile or empty UI promise.
            self.call = Some(actions::Call::new(session, owner, None));
            self.enabled.set(false);
        }
    }

    pub(crate) fn deadline(&self) -> Option<std::time::Instant> {
        self.call.as_ref().map(|call| call.deadline)
    }

    pub(crate) fn pump(
        &mut self,
        source: Option<super::certified_action_session::ActionSessionSource>,
    ) -> bool {
        let mut changed = false;
        if let Some(mut call) = self.call.take() {
            if let Some(accepted) = call.pump() {
                self.finish_call(&call, accepted);
                changed = true;
            } else {
                self.call = Some(call);
            }
        }
        let send = self.send.get();
        let expand = self.expand.get();
        self.send.set(false);
        self.expand.set(false);
        if !(send || expand) || !self.is_open() || !self.enabled.get() {
            return changed;
        }
        let Some(owner) = self.owner.clone() else {
            return changed;
        };
        let text = send.then(|| self.draft.get());
        if text
            .as_ref()
            .is_some_and(|text| text.trim().is_empty() || text.len() > 16384)
        {
            return changed;
        }
        let editor = if expand && !send {
            self.expansion_editor
                .take()
                .or_else(|| self.editor_snapshot())
        } else {
            None
        };
        // Button blur may already have removed display-only composition. Restore
        // before any fallible transport/staging operation, including source lookup.
        if let Some(editor) = &editor {
            restore_preedit(&mut self.popover, editor);
        }
        if let Some(session) = source
            .filter(|s| s.identity_revision(&owner.workspace) == Some(owner.identity_revision))
            .and_then(|s| s.open(&owner.workspace, Some(&owner.caller)).ok())
        {
            let call = if send {
                Ok(actions::Call::new(session, owner, text))
            } else {
                editor
                    .ok_or_else(|| "private editor unavailable".to_owned())
                    .and_then(|editor| self.stage_expansion(session, owner, editor))
            };
            match call {
                Ok(call) => {
                    self.call = Some(call);
                    self.enabled.set(false);
                }
                Err(_) => self.unavailable.set(t("person-quick-unavailable")),
            }
        } else {
            self.unavailable.set(t("person-quick-unavailable"));
        }
        true
    }

    fn stage_expansion(
        &mut self,
        session: super::certified_action_session::DesktopActionSession,
        owner: PersonDraftOwner,
        editor: ui::private_draft::Editor,
    ) -> Result<actions::Call, String> {
        restore_preedit(&mut self.popover, &editor);
        actions::Call::expand(
            session,
            owner,
            actions::InputContext {
                generation: self.generation,
                composition_generation: self.composition_generation,
                editor,
            },
        )
    }

    fn finish_call(&mut self, call: &actions::Call, accepted: bool) {
        if let Some(handoff) = &call.handoff {
            let source = &handoff.source;
            let same_context = self.is_open()
                && self.owner.as_ref() == Some(&call.owner)
                && self.generation == source.generation;
            if same_context {
                self.complete(&call.owner, None, accepted);
            }
            let unchanged = same_context
                && self.composition_generation == source.composition_generation
                && self
                    .editor_snapshot()
                    .is_some_and(|current| current == source.editor);
            if !unchanged {
                return;
            }
            self.complete(
                &call.owner,
                accepted.then_some(source.editor.text.as_str()),
                accepted,
            );
            if accepted {
                self.dismiss();
            }
        } else {
            self.complete(&call.owner, call.text.as_deref(), accepted);
        }
    }

    fn editor_snapshot(&self) -> Option<ui::private_draft::Editor> {
        Some(
            composer_editor(&self.popover, "person-quick-editor")?
                .as_any()?
                .downcast_ref::<TextInput>()?
                .private_draft_editor(),
        )
    }

    fn complete(&mut self, owner: &PersonDraftOwner, text: Option<&str>, accepted: bool) {
        if accepted && let Some(text) = text {
            let key = Self::draft_key(owner);
            if self.drafts.get(&key).is_some_and(|draft| draft == text) {
                self.drafts.remove(&key);
            }
            if self.owner.as_ref() == Some(owner) && self.draft.get() == text {
                self.draft.set(String::new());
            }
        }
        if self.owner.as_ref() == Some(owner) {
            self.enabled.set(true);
            self.unavailable.set(if accepted {
                String::new()
            } else {
                t("person-quick-unavailable")
            });
        }
    }

    fn draft_key(owner: &PersonDraftOwner) -> (String, String, String) {
        (
            owner.workspace.clone(),
            owner.person_key.clone(),
            owner.caller.clone(),
        )
    }

    pub(crate) fn open(
        &mut self,
        owner: PersonDraftOwner,
        label: &str,
        marker: Rect,
        viewport: Rect,
    ) {
        self.generation = next_generation(self.generation);
        self.save_draft();
        self.draft.set(
            self.drafts
                .get(&Self::draft_key(&owner))
                .cloned()
                .unwrap_or_default(),
        );
        self.owner = Some(owner);
        self.enabled.set(false);
        self.send.set(false);
        self.expand.set(false);
        self.expansion_editor = None;
        self.call = None;
        self.anchor = ComposerAnchor::default();
        self.title
            .set(t_args("person-quick-title", &[("person", label.into())]));
        self.unavailable.set(t("person-quick-unavailable"));
        self.close.set(false);
        self.popover.set_viewport(viewport);
        self.popover.set_anchor(marker);
        self.popover.set_avoid(Some(marker));
        self.popover.set_open(true);
        self.focus_reset = true;
    }
    pub(crate) fn follow_anchor(&mut self, marker: Rect) {
        if self.is_open() && !self.anchor.frozen {
            self.popover.set_anchor(marker);
            self.popover.set_avoid(Some(marker));
        }
    }

    pub(crate) fn owner(&self) -> Option<&PersonDraftOwner> {
        self.owner.as_ref()
    }
    pub(crate) fn is_open(&self) -> bool {
        self.popover.is_open()
    }
    fn save_draft(&mut self) {
        if let Some(owner) = &self.owner {
            self.drafts.insert(Self::draft_key(owner), self.draft.get());
        }
    }
    pub(crate) fn dismiss(&mut self) {
        self.generation = next_generation(self.generation);
        self.save_draft();
        self.popover.close();
        self.enabled.set(false);
        // A lost context cannot apply a late owner receipt to a private draft.
        // Dropping observation never cancels or retries an uncertain send.
        self.call = None;
        self.focus_reset = true;
    }
}

fn next_generation(generation: u64) -> u64 {
    // Never reuse a context identifier: an old acknowledgement must not erase a draft.
    generation
        .checked_add(1)
        .unwrap_or_else(|| std::process::abort())
}

fn restore_preedit(widget: &mut dyn Widget, editor: &ui::private_draft::Editor) -> bool {
    if widget.test_id() == Some("person-quick-editor") {
        if let Some(input) = widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<TextInput>())
        {
            input.restore_private_preedit(&editor.preedit, editor.preedit_cursor);
            return true;
        }
        return false;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| restore_preedit(child, editor))
}

impl Decorator for PersonQuickComposer {
    type Inner = Popover<Container>;
    fn inner(&self) -> &Self::Inner {
        &self.popover
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.popover
    }
    fn name(&self) -> &'static str {
        "PersonQuickComposer"
    }
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.save_draft();
        if self.close.get() {
            self.close.set(false);
            self.dismiss();
        }
        self.popover.sync(state, path);
    }
    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.save_draft();
        if self.close.get() {
            self.close.set(false);
            self.dismiss();
        }
        self.popover.update(state, path)
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let width = theme.tokens.metrics.panel_width_sheet * theme.text_scale.max(1.0);
        let capacity = self.popover.available_content_width(theme);
        self.popover.content_mut().set_width(width.min(capacity));
        self.popover.build_layout(tree, theme, nodes)
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if !self.is_open() {
            return None;
        }
        composer_editor(&self.popover, "person-quick-editor")?.ime_cursor_rect()
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset) | self.popover.take_focus_reset()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let open = self.is_open();
        if open && matches!(event, winit::event::WindowEvent::Ime(_)) {
            self.composition_generation = next_generation(self.composition_generation);
        }
        if open && let Some(editor) = composer_editor(&self.popover, "person-quick-editor") {
            self.anchor.observe(editor, event, ctx);
        }
        // A button can blur its sibling editor. Capture composition before
        // dispatch, not after blur has cleared the display-only preedit.
        if open
            && composer_editor(&self.popover, "person-quick-editor").is_some_and(Widget::is_focused)
        {
            self.expansion_editor = self.editor_snapshot();
        }
        self.popover.handle_event(event, ctx);
        if !self.expand.get()
            && composer_editor(&self.popover, "person-quick-editor").is_some_and(Widget::is_focused)
        {
            self.expansion_editor = self.editor_snapshot();
        }
        if open && !self.is_open() {
            self.dismiss();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn focus(widget: &mut dyn Widget) -> bool {
        if widget.test_id() == Some("person-quick-editor") {
            widget.set_focus(true);
            return true;
        }
        widget.children_mut().into_iter().any(focus)
    }

    #[test]
    fn painted_anchor_follows_until_real_editor_input_and_stays_frozen() {
        use ui::synthetic_input as input;
        use winit::event::{ElementState, Ime, MouseButton, WindowEvent};

        for event in [
            input::char_key("x"),
            input::named_key(winit::keyboard::NamedKey::ArrowLeft),
            WindowEvent::Ime(Ime::Commit("x".into())),
            WindowEvent::Ime(Ime::Preedit("composing".into(), Some((0, 9)))),
            input::mouse_press(),
            input::mouse_input(ElementState::Pressed, MouseButton::Right),
            input::mouse_input(ElementState::Pressed, MouseButton::Middle),
        ] {
            let marker = Rect::new(10.0, 10.0, 42.0, 42.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut doc = automerge::AutoCommit::new();
            let mut frame = ui::headless::FrameHarness::for_root(
                ui::State::new(),
                {
                    let mut composer = PersonQuickComposer::default();
                    composer.open(owner("alice"), "Alice", marker, viewport);
                    Widget::sync(&mut composer, &doc, &automerge::ROOT);
                    assert!(edit(&mut composer, "private draft", &mut doc));
                    Widget::sync(&mut composer, &doc, &automerge::ROOT);
                    composer
                },
                (800, 600),
                Theme::dark(),
            );
            let initial = frame.root::<PersonQuickComposer>().popover.rect();
            let moved = Rect::new(300.0, 220.0, 332.0, 252.0);
            {
                let composer = frame.root_mut::<PersonQuickComposer>();
                composer.follow_anchor(moved);
            }
            frame.recompose().unwrap();
            let followed = frame.root::<PersonQuickComposer>().popover.rect();
            assert_ne!(
                initial, followed,
                "untouched editor follows its painted cursor"
            );
            // Focus by itself must not freeze the anchor. The event below
            // reaches the real retained TextInput through the popover.
            assert!(focus(frame.root_mut::<PersonQuickComposer>()));
            frame.recompose().unwrap();
            assert!(!frame.root::<PersonQuickComposer>().anchor.frozen);
            let editor =
                composer_editor(frame.root::<PersonQuickComposer>(), "person-quick-editor")
                    .unwrap()
                    .get_rect()
                    .center();
            frame.dispatch_to_root(&input::cursor_moved(editor.x, editor.y));
            frame.dispatch_to_root(&event);
            assert!(
                frame.root::<PersonQuickComposer>().anchor.frozen,
                "{event:?}"
            );
            let draft = frame.root::<PersonQuickComposer>().draft.get();
            if matches!(&event, WindowEvent::Ime(Ime::Preedit(..))) {
                assert_eq!(
                    draft, "private draft",
                    "preedit freezes before a committed text change"
                );
            }
            if matches!(&event, WindowEvent::KeyboardInput { event, .. } if event.text.as_deref() == Some("x"))
            {
                assert!(draft.contains('x'), "the shared editor received the typing");
                assert_eq!(draft.len(), "private draft".len() + 1);
            }
            let later = Rect::new(500.0, 400.0, 532.0, 432.0);
            {
                let composer = frame.root_mut::<PersonQuickComposer>();
                composer.follow_anchor(later);
            }
            frame.recompose().unwrap();
            assert_eq!(frame.root::<PersonQuickComposer>().popover.rect(), followed);
            assert_eq!(frame.root::<PersonQuickComposer>().draft.get(), draft);
            frame.dispatch_to_root(&input::cursor_moved(790.0, 590.0));
            frame.dispatch_to_root(&input::mouse_press());
            assert_dismissed_editor(
                frame.root_mut::<PersonQuickComposer>(),
                later,
                followed,
                &draft,
            );
        }
    }

    fn assert_dismissed_editor(
        composer: &mut PersonQuickComposer,
        later: Rect,
        followed: Rect,
        draft: &str,
    ) {
        assert!(!composer.is_open(), "outside click only dismisses");
        composer.follow_anchor(later);
        assert_eq!(
            composer.popover.rect(),
            followed,
            "closed composer never retargets"
        );
        assert!(
            Widget::children(composer).is_empty(),
            "closed editor has no hit targets"
        );
        assert!(
            Widget::ime_cursor_rect(composer).is_none(),
            "closed editor has no IME caret"
        );
        assert_eq!(composer.draft.get(), draft, "dismissal preserves the draft");
    }

    fn measured_person_controls(composer: &PersonQuickComposer) -> (Rect, Rect) {
        let bar = composer.popover.rect();
        assert!(Rect::new(0.0, 0.0, 800.0, 600.0).contains_rect(bar));
        (
            named(composer, "person-quick-editor").unwrap().get_rect(),
            named(composer, "person-quick-send").unwrap().get_rect(),
        )
    }

    fn owner(person_key: &str) -> PersonDraftOwner {
        PersonDraftOwner {
            workspace: "11111111-1111-4111-8111-111111111111".into(),
            person_key: person_key.into(),
            caller: "local".into(),
            identity_revision: 1,
        }
    }
    fn named<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if widget.test_id() == Some(id) {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| named(child, id))
    }

    fn edit(widget: &mut dyn Widget, value: &str, doc: &mut automerge::AutoCommit) -> bool {
        if widget.test_id() == Some("person-quick-editor") {
            return widget.perform_action(
                &ui::a11y::Action::SetValue(value.into()),
                &[],
                &mut ui::widget::Edits::lend(doc),
                &automerge::ROOT,
            );
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| edit(child, value, doc))
    }

    #[test]
    fn shared_editor_never_writes_the_workspace_document_and_unavailable_actions_are_disabled() {
        let mut composer = PersonQuickComposer::default();
        composer.open(
            owner("alice"),
            "Alice",
            Rect::new(10.0, 10.0, 42.0, 42.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        let mut doc = automerge::AutoCommit::new();
        let before = doc.save();
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert!(edit(&mut composer, "Only the intended recipient", &mut doc));
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert_eq!(composer.draft.get(), "Only the intended recipient");
        assert_eq!(
            before,
            doc.save(),
            "private drafts must not enter a room-shared document"
        );
        assert!(!named(&composer, "person-quick-send").unwrap().is_enabled());
        assert!(
            !named(&composer, "person-quick-expand")
                .unwrap()
                .is_enabled()
        );
        composer.dismiss();
        composer.open(
            owner("alice"),
            "Alice",
            Rect::new(10.0, 10.0, 42.0, 42.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        assert_eq!(composer.draft.get(), "Only the intended recipient");
        drop(composer);
    }

    #[test]
    fn transparent_person_popover_uses_the_production_layout_in_both_themes() {
        for theme in [Theme::light(), Theme::dark()] {
            for marker in [
                Rect::new(10.0, 10.0, 42.0, 42.0),
                Rect::new(760.0, 560.0, 792.0, 592.0),
            ] {
                let frame = ui::headless::FrameHarness::for_root(
                    ui::State::new(),
                    {
                        let mut composer = PersonQuickComposer::default();
                        composer.open(
                            owner("alice"),
                            "Alice",
                            marker,
                            Rect::new(0.0, 0.0, 800.0, 600.0),
                        );
                        composer
                    },
                    (800, 600),
                    theme.clone(),
                );
                let (editor, send) = measured_person_controls(frame.root::<PersonQuickComposer>());
                assert!(editor.width() > 0.0);
                assert!(
                    send.x0 > editor.x1,
                    "shared control spacing leaves transparent desktop"
                );
                let rgba = frame.render_rgba_transparent();
                let pixel = ui::offscreen::OffscreenRenderer::sample(
                    &rgba,
                    800,
                    ui::num::u32_of_f64(editor.x1.midpoint(send.x0)),
                    ui::num::u32_of_f64(editor.center().y),
                );
                assert_eq!(pixel.3, 0, "no outer panel between the controls");
                assert!(
                    rgba.chunks_exact(4).any(|pixel| pixel[3] > 0),
                    "the real composer painted"
                );
            }
        }
    }

    #[test]
    fn only_matching_owner_acceptance_clears_the_submitted_draft() {
        let mut composer = PersonQuickComposer::default();
        let owner = owner("alice");
        let marker = Rect::new(10.0, 10.0, 42.0, 42.0);
        let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
        composer.open(owner.clone(), "Alice", marker, viewport);
        composer.draft.set("private".into());
        composer.save_draft();
        composer.complete(&owner, Some("private"), false);
        assert_eq!(
            composer.draft.get(),
            "private",
            "offline/refused/uncertain is not acceptance"
        );
        composer.complete(&owner, None, true);
        assert_eq!(
            composer.draft.get(),
            "private",
            "opening Chat never sends a draft"
        );
        composer.draft.set("new edit".into());
        composer.save_draft();
        composer.complete(&owner, Some("private"), true);
        assert_eq!(
            composer.draft.get(),
            "new edit",
            "late send cannot erase subsequent typing"
        );
        composer.complete(&owner, Some("new edit"), true);
        assert_eq!(composer.draft.get(), "");
        composer.dismiss();
        composer.open(owner, "Renamed", marker, viewport);
        assert_eq!(
            composer.draft.get(),
            "",
            "accepted draft stays cleared after reopening"
        );
        drop(composer);
    }

    #[test]
    fn unavailable_source_never_clears_or_crosses_a_draft() {
        let mut composer = PersonQuickComposer::default();
        let marker = Rect::new(10.0, 10.0, 42.0, 42.0);
        let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
        composer.open(owner("alice"), "Alice", marker, viewport);
        composer.draft.set("private draft".into());
        composer.dismiss();
        composer.open(owner("bob"), "Bob", marker, viewport);
        assert_eq!(composer.draft.get(), "");
        composer.open(owner("alice"), "Renamed", marker, viewport);
        assert_eq!(composer.draft.get(), "private draft");
        let mut renewed = owner("alice");
        renewed.identity_revision = 2;
        composer.open(renewed, "Alice", marker, viewport);
        assert_eq!(
            composer.draft.get(),
            "private draft",
            "certificate renewal preserves same-person draft"
        );
        let mut other_caller = owner("alice");
        other_caller.caller = "another-person".into();
        composer.open(other_caller, "Alice", marker, viewport);
        assert_eq!(
            composer.draft.get(),
            "",
            "never expose another selected person's private draft"
        );
        let mut other_workspace = owner("alice");
        other_workspace.workspace = "22222222-2222-4222-8222-222222222222".into();
        composer.open(other_workspace, "Alice", marker, viewport);
        assert_eq!(composer.draft.get(), "");
        drop(composer);
    }
}
