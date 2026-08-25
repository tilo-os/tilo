//! The rendered first-login identity ceremony.
//!
//! Chrome, not an app: the compositor draws this before the desktop exists.
//! An identity is the thing every later signature rests on, so the screen
//! that creates it cannot be something an application could imitate.
//!
//! The decisions all live in [`crate::onboarding::Onboarding`]; this file is
//! only the composition. Every element is a shared `ui` widget — `Card`,
//! `Banner`, `Text`, `Button`, `Checkbox` — so the ceremony inherits the
//! system theme, focus order and accessibility the same way the rest of the
//! desktop does.

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Banner, Button, Card, Checkbox, Column, Row, Spacer, Text, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::onboarding::{
    MintedIdentity, Onboarding, Screen, ACKNOWLEDGEMENT, DONE_BODY, DONE_TITLE, INTRO_BODY,
    INTRO_POINTS, INTRO_TITLE, NO_RECOVERY_POINTS, NO_RECOVERY_TITLE,
};

/// Measure for the ceremony's column, in logical px. Body text at 13px wants
/// roughly this width to land near 70 characters a line; wider and the eye
/// loses the start of the next line, which is the last thing you want on the
/// one screen that has to actually be read.
const CONTENT_WIDTH: f32 = 620.0;

pub const KEY_BEGIN: &str = "onboarding.begin";
pub const KEY_UNDERSTOOD: &str = "onboarding.understood";
pub const KEY_CREATE: &str = "onboarding.create";
pub const KEY_FINISH: &str = "onboarding.finish";
/// The name field's backing `Text` object.
///
/// A `TextInput` bound by path splices into an object that already exists
/// and never makes one, so [`OnboardingView::sync`] seeds it. Without that
/// the field takes keystrokes and drops them at the next structural
/// rebuild, which is exactly how a field reads as dead.
pub const KEY_DISPLAY_NAME: &str = "onboarding.display_name";

type Minter = Box<dyn Fn(&str) -> Result<MintedIdentity, String> + Send>;

/// The ceremony widget.
pub struct OnboardingView {
    flow: Onboarding,
    minter: Minter,
    /// Set when minting failed, shown in place of the success screen. A
    /// failure here is loud: booting on into a desktop that silently has no
    /// identity is the behaviour this whole flow replaces.
    error: Option<String>,
    /// Raised when the user has finished and the desktop may start.
    finished: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl OnboardingView {
    /// Build a ceremony that mints into `identity_dir`.
    pub fn new(identity_dir: PathBuf) -> Self {
        Self::with_minter(Box::new(move |name: &str| {
            crate::onboarding::mint_into(&identity_dir, name).map_err(|e| e.to_string())
        }))
    }

    /// Build with an injected minter, so tests drive the whole widget
    /// without writing a key to disk.
    pub fn with_minter(minter: Minter) -> Self {
        Self {
            flow: Onboarding::new(),
            minter,
            error: None,
            finished: false,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn flow(&self) -> &Onboarding {
        &self.flow
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// True once the user has seen the result and dismissed it.
    pub fn finished(&self) -> bool {
        self.finished
    }

    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let body: Box<dyn Widget> = match self.flow.screen() {
            Screen::Intro => self.intro(theme),
            Screen::NoRecovery => self.no_recovery(theme),
            Screen::Done => self.done(theme),
        };
        Box::new(Column::new().with_no_padding().with_gap(16.0).push(body))
    }

    fn intro(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut list = Column::new().with_no_padding().with_gap(10.0);
        for point in INTRO_POINTS {
            list = list.push(bullet(point));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(heading(INTRO_TITLE))
                .push(paragraph(INTRO_BODY, theme))
                .push(Box::new(Card::titled("What it is for", Box::new(list))))
                .push(actions(vec![Box::new(
                    Button::new("onboarding.begin.button", "Continue", Some(KEY_BEGIN))
                        .with_style(ui::widget::ButtonStyle::Primary),
                )])),
        )
    }

    fn no_recovery(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut list = Column::new().with_no_padding().with_gap(14.0);
        for point in NO_RECOVERY_POINTS {
            list = list.push(bullet(point));
        }
        // The one question this ceremony asks about the *person*. It sits
        // here rather than on a screen of its own because this is already
        // where the person makes the one real decision, and a name is not
        // worth a fourth screen.
        //
        // A shared `TextInput`, not a painted box: a hand-rolled field
        // would have no selection, no clipboard, no caret navigation, no
        // focus ring and no accessible role — see `ui/HIG.md` §8.
        let label = crate::shell::i18n::t("onboarding-name-label");
        let name_field = ui::widget::TextInput::new(Vec::new())
            .with_id("onboarding.name")
            .with_on_change_path(KEY_DISPLAY_NAME)
            .with_placeholder(&crate::shell::i18n::t("onboarding-name-placeholder"))
            .with_accessible_name(&label);
        let name_block = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(Box::new(Text::new(&label).with_size(13.0)))
            .push(Box::new(name_field))
            .push(Box::new(
                Text::new(&crate::shell::i18n::t("onboarding-name-hint"))
                    .with_size(11.0)
                    .with_color(theme.text_dim),
            ));
        // The gate. Disabled until the box is ticked, so the primary action
        // cannot be clicked past the consequence.
        let create = Button::new("onboarding.create.button", "Create my identity", Some(KEY_CREATE))
            .with_style(ui::widget::ButtonStyle::Primary)
            .with_enabled(self.flow.can_create());
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Banner::warning(NO_RECOVERY_TITLE)
                        .with_message("Read this before continuing — it cannot be undone later."),
                ))
                .push(Box::new(Card::new(Box::new(list))))
                .push(Box::new(Card::new(Box::new(name_block))))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Checkbox::new("onboarding.understood.box", KEY_UNDERSTOOD)
                                .with_id("onboarding.understood")
                                .with_accessible_name(ACKNOWLEDGEMENT),
                        ))
                        .push(Box::new(Text::new(ACKNOWLEDGEMENT).with_size(13.0))),
                ))
                .push(actions(vec![Box::new(create)])),
        )
    }

    fn done(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(err) = &self.error {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(20.0)
                    .push(Box::new(
                        Banner::error("Your identity could not be created")
                            .with_message(err.clone()),
                    ))
                    .push(paragraph(
                        "The desktop will start, but nothing you do will be signed until this \
                         is fixed. Your edits will still save; other people just will not be \
                         able to tell they came from you.",
                        theme,
                    ))
                    .push(actions(vec![Box::new(Button::new(
                        "onboarding.finish.button",
                        "Continue anyway",
                        Some(KEY_FINISH),
                    ))])),
            );
        }
        let minted = self.flow.minted();
        let fingerprint = minted
            .map(|m| m.grouped_fingerprint())
            .unwrap_or_default();
        let path = minted
            .map(|m| m.path.display().to_string())
            .unwrap_or_default();
        let detail = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(Box::new(
                Text::new(&fingerprint)
                    .with_size(15.0)
                    .with_font_family(ui::text_render::FontChoice::Monospace),
            ))
            .push(Box::new(
                Text::new(&format!("Stored at {path}, readable only by you."))
                    .with_size(11.0)
                    .with_color(theme.text_dim),
            ));
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(Banner::success(DONE_TITLE)))
                .push(paragraph(DONE_BODY, theme))
                .push(Box::new(Card::titled("Fingerprint", Box::new(detail))))
                // Repeated deliberately. This is the last moment the user is
                // looking at the subject, and it is the one fact they need to
                // carry away from the whole ceremony.
                .push(paragraph(
                    "There is still no way to recover this key. If this device goes, so does \
                     this identity.",
                    theme,
                ))
                .push(actions(vec![Box::new(
                    Button::new("onboarding.finish.button", "Start using qOS", Some(KEY_FINISH))
                        .with_style(ui::widget::ButtonStyle::Primary),
                )])),
        )
    }
}

fn heading(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_size(26.0))
}

/// Secondary body copy. Takes the live theme rather than a hard-coded one:
/// a dark-theme dim grey on a light background is barely legible, and OS
/// chrome has no business pinning a palette.
fn paragraph(text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_size(13.0).with_color(theme.text_dim))
}

/// A list item.
///
/// The marker is part of the same `Text` rather than a sibling in a `Row`:
/// these lines wrap, and a marker in its own column detaches from its
/// sentence and floats onto a line of its own.
fn bullet(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(&format!("•   {text}")).with_size(13.0))
}

/// A right-aligned action row, so the primary button lands where the eye
/// finishes reading.
fn actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for b in buttons {
        row = row.push(b);
    }
    Box::new(row)
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

/// Whatever is in the `Text` object at `key`, or the empty string.
fn read_text(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    match state.get(root, key) {
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => {
            state.text(&obj).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Give the name field something to splice into, filled in with the
/// account name.
///
/// Pre-filled rather than empty on purpose. The field can be left as it
/// is, and then the record carries a name the machine already had rather
/// than the empty string that rendered as "Unknown participant"; a person
/// who wants a different one types over what they can see. An empty box
/// with a silent fallback would decide the same thing without showing it.
///
/// Returns whether it seeded, so the caller can mark the composition
/// stale exactly once.
fn seed_name_field(state: &mut AutoCommit, root: &ObjId) -> bool {
    if matches!(
        state.get(root, KEY_DISPLAY_NAME),
        Ok(Some((Value::Object(automerge::ObjType::Text), _)))
    ) {
        return false;
    }
    let Ok(obj) = state.put_object(root, KEY_DISPLAY_NAME, automerge::ObjType::Text) else {
        return false;
    };
    let seed = crate::onboarding::account_name();
    if !seed.is_empty() {
        let _ = state.splice_text(&obj, 0, 0, &seed);
    }
    true
}

impl Widget for OnboardingView {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let before = self.flow.screen();

        if read_bool(state, root, KEY_BEGIN) {
            let _ = state.put(root, KEY_BEGIN, ScalarValue::Boolean(false));
            self.flow.begin();
        }

        if seed_name_field(state, root) {
            self.structure_dirty = true;
        }
        let typed = read_text(state, root, KEY_DISPLAY_NAME);
        if typed != self.flow.display_name() {
            self.flow.set_display_name(typed);
        }

        // The checkbox drives the gate directly rather than being latched, so
        // unticking really does close it again.
        let understood = read_bool(state, root, KEY_UNDERSTOOD);
        if understood != self.flow.acknowledged() {
            self.flow.set_acknowledged(understood);
            self.structure_dirty = true;
        }

        if read_bool(state, root, KEY_CREATE) {
            let _ = state.put(root, KEY_CREATE, ScalarValue::Boolean(false));
            // `can_create` is checked inside `complete` too — this is the
            // outer guard, that one is the invariant.
            if self.flow.can_create() {
                match (self.minter)(self.flow.display_name()) {
                    Ok(minted) => {
                        self.flow.complete(minted);
                    }
                    Err(e) => {
                        tracing::error!(target: "qos::onboarding", error = %e, "identity minting failed");
                        self.error = Some(e);
                        // Force the result screen so the failure is shown
                        // rather than leaving the user on a dead button.
                        self.flow.set_acknowledged(true);
                        self.flow.complete(MintedIdentity {
                            fingerprint: String::new(),
                            path: PathBuf::new(),
                        });
                    }
                }
            }
        }

        if read_bool(state, root, KEY_FINISH) {
            let _ = state.put(root, KEY_FINISH, ScalarValue::Boolean(false));
            self.finished = true;
        }

        if self.flow.screen() != before {
            self.structure_dirty = true;
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition(theme);
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Hold the text to a comfortable measure and let the root centre it.
        // This is a full-screen ceremony, so without both the copy would run
        // the width of a 4K display and sit hard against the top edge.
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::length(CONTENT_WIDTH);
            // Auto height, or the column claims the full viewport and there
            // is nothing left for the root to centre.
            cs.size.height = taffy::Dimension::auto();
            cs.flex_grow = 0.0;
            cs.flex_shrink = 0.0;
            let _ = taffy.set_style(child, cs);
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            justify_content: Some(taffy::JustifyContent::Center),
            align_items: Some(taffy::AlignItems::Center),
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
                    c.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        cn,
                        taffy,
                    );
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.as_ref().map(|c| c.as_ref()).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "OnboardingView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("onboarding")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn view() -> (OnboardingView, Arc<AtomicUsize>) {
        let (v, calls, _) = view_recording_names();
        (v, calls)
    }

    /// The same view, plus what the ceremony handed the minter — which is
    /// the only thing that decides what the record on disk is called.
    #[allow(clippy::type_complexity)]
    fn view_recording_names() -> (
        OnboardingView,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        // The copy under test is the source language. Without this the
        // ceremony renders in whatever locale the machine running the
        // tests happens to have, and an assertion about wording compares
        // against a translation.
        crate::shell::i18n::set_locale("en-US");
        let calls = Arc::new(AtomicUsize::new(0));
        let names: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let c = calls.clone();
        let n = names.clone();
        let v = OnboardingView::with_minter(Box::new(move |name: &str| {
            c.fetch_add(1, Ordering::SeqCst);
            n.lock().unwrap().push(name.to_string());
            Ok(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/home/user/.config/qos/identity/primary.json"),
            })
        }));
        (v, calls, names)
    }


    fn press(doc: &mut AutoCommit, key: &str) {
        let _ = doc.put(&automerge::ROOT, key, ScalarValue::Boolean(true));
    }

    /// One keystroke, delivered the way the App delivers one.
    fn send_key(
        v: &mut OnboardingView,
        doc: &mut AutoCommit,
        taffy: &TaffyTree<()>,
        node: NodeId,
        key: winit::keyboard::Key,
    ) {
        let event = ui::headless::key_event(key, winit::event::ElementState::Pressed);
        let mut handled = false;
        let mut focus_target = None;
        v.handle_event(
            &event,
            1.0,
            &Modifiers::default(),
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            taffy,
        );
    }

    /// The whole point: clicking through without ticking the box must not
    /// produce a key.
    #[test]
    fn pressing_create_without_acknowledging_mints_nothing() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.flow().screen(), Screen::NoRecovery);

        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no key was minted");
        assert_eq!(v.flow().screen(), Screen::NoRecovery, "still on the gate");
    }

    #[test]
    fn acknowledging_then_creating_mints_exactly_once() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(v.flow().can_create());
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        // A second sync must not re-fire — the key is consumed.
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(v.flow().screen(), Screen::Done);
        assert!(v.flow().is_done());
    }

    #[test]
    fn unticking_the_box_closes_the_gate_again() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        let _ = doc.put(&automerge::ROOT, KEY_UNDERSTOOD, ScalarValue::Boolean(false));
        v.sync(&mut doc, &automerge::ROOT);
        assert!(!v.flow().can_create());
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A minting failure must be shown, not swallowed into a desktop that
    /// silently has no identity.
    #[test]
    fn a_failed_mint_is_surfaced_rather_than_hidden() {
        let mut v = OnboardingView::with_minter(Box::new(|_| Err("disk full".to_string())));
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.error(), Some("disk full"));
        assert_eq!(v.flow().screen(), Screen::Done, "the failure gets a screen");
    }

    /// Lay the ceremony out for its current screen and hand back the tree,
    /// so events can be dispatched into it the way the App dispatches them.
    ///
    /// The re-sync after the build is not decoration: `build_layout`
    /// recreates every child, so the field it just made has never read the
    /// document and holds an empty string. `App::rebuild_structure` does
    /// exactly this for the same reason — without it a keystroke splices
    /// at offset zero of text the widget cannot see.
    fn lay_out(v: &mut OnboardingView, doc: &mut AutoCommit) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = v.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(node, {
                use taffy::prelude::TaffyMaxContent;
                taffy::Size::MAX_CONTENT
            })
            .expect("the ceremony lays out");
        v.apply_layout(&taffy, node, Point::ZERO);
        v.sync(doc, &automerge::ROOT);
        (taffy, node)
    }

    /// The widget in `w`'s subtree that reports `id`, if any.
    fn by_test_id<'a>(w: &'a mut (dyn Widget + 'a), id: &str) -> Option<&'a mut (dyn Widget + 'a)> {
        if w.test_id() == Some(id) {
            return Some(w);
        }
        for child in w.children_mut() {
            // Reborrow through a raw pointer: the borrow checker cannot see
            // that the returned reference comes from this child alone.
            let ptr: *mut (dyn Widget + '_) = child;
            if let Some(found) = by_test_id(unsafe { &mut *ptr }, id) {
                return Some(found);
            }
        }
        None
    }

    /// The headline for the first half: a person types a name into the
    /// ceremony and *that* is what gets minted.
    ///
    /// The keystrokes go through `Widget::handle_event` — the same call the
    /// App makes — rather than through `set_display_name`, which would
    /// prove a setter works and say nothing about whether anybody can
    /// reach it.
    #[test]
    fn the_name_a_person_types_is_the_name_that_gets_minted() {
        let (mut v, calls, names) = view_recording_names();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.flow().screen(), Screen::NoRecovery);

        let (taffy, node) = lay_out(&mut v, &mut doc);
        by_test_id(&mut v as &mut dyn Widget, "onboarding.name")
            .expect("the ceremony has a name field")
            .set_focus(true);

        // The field opens pre-filled with the account name; clear it the
        // way a person retyping their own name does. `End` first: a
        // CRDT-bound field takes its text in `sync` and parks the caret at
        // the start, so a bare Backspace here would delete nothing.
        let seed = crate::onboarding::account_name();
        send_key(
            &mut v,
            &mut doc,
            &taffy,
            node,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::End),
        );
        for _ in 0..seed.chars().count() {
            send_key(
                &mut v,
                &mut doc,
                &taffy,
                node,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace),
            );
        }
        for ch in "Ada Lovelace".chars() {
            send_key(
                &mut v,
                &mut doc,
                &taffy,
                node,
                winit::keyboard::Key::Character(ch.to_string().into()),
            );
        }

        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            v.flow().display_name(),
            "Ada Lovelace",
            "the typed name did not reach the ceremony's state"
        );

        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);

        assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one key was minted");
        assert_eq!(
            names.lock().unwrap().as_slice(),
            ["Ada Lovelace".to_string()],
            "the ceremony minted under a different name than the one typed"
        );
    }

    /// The field must be a shared `TextInput`, focusable and announced to a
    /// screen reader. A painted box would be neither — and `ui/HIG.md` §8
    /// exists because that is what keeps happening.
    #[test]
    fn the_name_field_is_a_real_focusable_widget() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        let _ = lay_out(&mut v, &mut doc);

        let field = by_test_id(&mut v as &mut dyn Widget, "onboarding.name")
            .expect("no widget in the ceremony reports the name field's id");
        assert!(
            field.is_focusable(),
            "the name field cannot be reached by keyboard"
        );
        let node = field
            .accessibility()
            .expect("the name field has no accessibility node at all");
        assert_eq!(
            node.role,
            ui::a11y::Role::TextInput,
            "the name field announces itself as {:?}, not a text field",
            node.role
        );
        assert_eq!(
            node.name.as_deref(),
            Some("Display name"),
            "the name field announces itself as {:?}",
            node.name
        );
    }

    /// Pre-filled, not blank. The person sees the answer that is about to
    /// be used instead of an empty box with a fallback they never saw.
    #[test]
    fn the_field_opens_holding_the_account_name() {
        let seed = crate::onboarding::account_name();
        assert!(
            !seed.is_empty(),
            "no $USER / $USERNAME here, so the pre-fill is unobservable and \
             this test proves nothing"
        );
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.flow().display_name(), seed);
    }

    /// A name is not a password: an empty field must not wedge first boot.
    #[test]
    fn an_empty_name_does_not_block_the_gate() {
        let (mut v, calls) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        v.flow.set_display_name("");
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(v.flow().can_create(), "a blank name closed the gate");
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn finishing_releases_the_desktop() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        assert!(!v.finished());
        press(&mut doc, KEY_FINISH);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(v.finished());
    }

    /// Every screen must lay out. A ceremony that panics on the screen after
    /// the one under test is worse than no ceremony.
    #[test]
    fn every_screen_builds_a_layout() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        for step in [None, Some(KEY_BEGIN), Some(KEY_UNDERSTOOD), Some(KEY_CREATE)] {
            if let Some(k) = step {
                press(&mut doc, k);
                v.sync(&mut doc, &automerge::ROOT);
            }
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = v.build_layout(&mut taffy, &theme, &mut node_map);
        }
    }
}
