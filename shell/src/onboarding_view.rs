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

pub const KEY_BEGIN: &str = "onboarding.begin";
pub const KEY_UNDERSTOOD: &str = "onboarding.understood";
pub const KEY_CREATE: &str = "onboarding.create";
pub const KEY_FINISH: &str = "onboarding.finish";

type Minter = Box<dyn Fn() -> Result<MintedIdentity, String> + Send>;

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
        Self::with_minter(Box::new(move || {
            crate::onboarding::mint_into(&identity_dir).map_err(|e| e.to_string())
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

    fn build_composition(&self) -> Box<dyn Widget> {
        let body: Box<dyn Widget> = match self.flow.screen() {
            Screen::Intro => self.intro(),
            Screen::NoRecovery => self.no_recovery(),
            Screen::Done => self.done(),
        };
        Box::new(
            Column::new()
                .with_gap(16.0)
                .with_vertical_padding(48.0)
                .with_horizontal_padding(64.0, 64.0)
                .push(body),
        )
    }

    fn intro(&self) -> Box<dyn Widget> {
        let mut list = Column::new().with_no_padding().with_intrinsic_height().with_gap(10.0);
        for point in INTRO_POINTS {
            list = list.push(bullet(point));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(heading(INTRO_TITLE))
                .push(paragraph(INTRO_BODY))
                .push(Box::new(Card::titled("What it is for", Box::new(list))))
                .push(actions(vec![Box::new(Button::new(
                    "onboarding.begin.button",
                    "Continue",
                    Some(KEY_BEGIN),
                ))])),
        )
    }

    fn no_recovery(&self) -> Box<dyn Widget> {
        let mut list = Column::new().with_no_padding().with_intrinsic_height().with_gap(12.0);
        for point in NO_RECOVERY_POINTS {
            list = list.push(bullet(point));
        }
        // The gate. Disabled until the box is ticked, so the primary action
        // cannot be clicked past the consequence.
        let create = Button::new("onboarding.create.button", "Create my identity", Some(KEY_CREATE))
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
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Checkbox::new("onboarding.understood.box", KEY_UNDERSTOOD)
                                .with_accessible_name(ACKNOWLEDGEMENT),
                        ))
                        .push(Box::new(Text::new(ACKNOWLEDGEMENT).with_size(13.0))),
                ))
                .push(actions(vec![Box::new(create)])),
        )
    }

    fn done(&self) -> Box<dyn Widget> {
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
                    .with_color(Theme::dark().text_dim),
            ));
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(Banner::success(DONE_TITLE)))
                .push(paragraph(DONE_BODY))
                .push(Box::new(Card::titled("Fingerprint", Box::new(detail))))
                // Repeated deliberately. This is the last moment the user is
                // looking at the subject, and it is the one fact they need to
                // carry away from the whole ceremony.
                .push(paragraph(
                    "There is still no way to recover this key. If this device goes, so does \
                     this identity.",
                ))
                .push(actions(vec![Box::new(Button::new(
                    "onboarding.finish.button",
                    "Start using qOS",
                    Some(KEY_FINISH),
                ))])),
        )
    }
}

fn heading(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_size(26.0))
}

fn paragraph(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_size(13.0).with_color(Theme::dark().text_dim))
}

fn bullet(text: &str) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(Box::new(
                Text::new("•").with_size(13.0).with_color(Theme::dark().text_dim),
            ))
            .push(Box::new(Text::new(text).with_size(13.0))),
    )
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

impl Widget for OnboardingView {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let before = self.flow.screen();

        if read_bool(state, root, KEY_BEGIN) {
            let _ = state.put(root, KEY_BEGIN, ScalarValue::Boolean(false));
            self.flow.begin();
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
                match (self.minter)() {
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
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
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
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let v = OnboardingView::with_minter(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/home/user/.config/qos/identity/primary.json"),
            })
        }));
        (v, calls)
    }

    fn press(doc: &mut AutoCommit, key: &str) {
        let _ = doc.put(&automerge::ROOT, key, ScalarValue::Boolean(true));
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
        let mut v = OnboardingView::with_minter(Box::new(|| Err("disk full".to_string())));
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
