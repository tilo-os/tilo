//! The rendered first-login identity ceremony.
//!
//! Chrome, not an app: the compositor draws this before the desktop
//! exists, since the screen that creates an identity cannot be something
//! an application could imitate.
//!
//! The decisions all live in [`crate::onboarding::Onboarding`]; this
//! file is only the composition, built from shared `ui` widgets (`Card`,
//! `Banner`, `Text`, `Button`, `Checkbox`) so the ceremony inherits the
//! system theme, focus order and accessibility like the rest of the desktop.

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Banner, Button, Card, Checkbox, Column, Row, Spacer, Text, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::onboarding::{
    ACKNOWLEDGEMENT, DONE_BODY, DONE_TITLE, INTRO_BODY, INTRO_POINTS, INTRO_TITLE, MintedIdentity,
    NO_RECOVERY_POINTS, NO_RECOVERY_TITLE, Onboarding, Screen,
};

/// Measure for the ceremony's column, in logical px — body text at 13px
/// wants roughly this width to land near 70 characters a line, or the eye loses the start of the next line.
const CONTENT_WIDTH: f32 = 620.0;

pub const KEY_BEGIN: &str = "onboarding.begin";
pub const KEY_CONNECT: &str = "onboarding.connect";
pub const KEY_CONNECT_PATH: &str = "onboarding.connect.path";
pub const KEY_IMPORT: &str = "onboarding.import";
pub const KEY_UNDERSTOOD: &str = "onboarding.understood";
pub const KEY_CREATE: &str = "onboarding.create";
pub const KEY_FINISH: &str = "onboarding.finish";

type Minter = Box<dyn Fn() -> Result<MintedIdentity, String> + Send>;
type Importer = Box<dyn Fn(&PathBuf) -> Result<MintedIdentity, String> + Send>;

pub struct OnboardingView {
    flow: Onboarding,
    minter: Minter,
    importer: Importer,
    /// Set when minting failed, shown in place of the success screen — loud,
    /// since booting on into a desktop that silently has no identity is what this flow replaces.
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
        let mint_dir = identity_dir.clone();
        let import_dir = identity_dir;
        Self::with_handlers(
            Box::new(move || crate::onboarding::mint_into(&mint_dir).map_err(|e| e.to_string())),
            Box::new(move |source| {
                let store = crate::identity_registry::IdentityRegistryStore::at(&import_dir);
                let profile = store.import(source).map_err(|error| error.to_string())?;
                crate::firstboot::ensure_device_certificate_for_record(
                    &profile.record_path,
                    &crate::firstboot::collaboration_identity_paths(),
                )
                .map_err(|error| error.to_string())?;
                crate::device_fleet::DeviceFleetStore::at(
                    &import_dir,
                    crate::firstboot::collaboration_identity_paths(),
                )
                .register_current_device(&profile.id)
                .map_err(|error| error.to_string())?;
                Ok(MintedIdentity {
                    fingerprint: profile.id,
                    path: profile.record_path,
                })
            }),
        )
    }

    /// Build with an injected minter, so tests drive the whole widget without writing a key to disk.
    pub fn with_minter(minter: Minter) -> Self {
        Self::with_handlers(
            minter,
            Box::new(|_| Err("identity import is unavailable in this test".to_string())),
        )
    }

    fn with_handlers(minter: Minter, importer: Importer) -> Self {
        Self {
            flow: Onboarding::new(),
            minter,
            importer,
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
            Screen::Connect => self.connect(theme),
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
                .push(actions(vec![
                    Box::new(Button::new(
                        "onboarding.connect.button",
                        "Connect existing identity",
                        Some(KEY_CONNECT),
                    )),
                    Box::new(
                        Button::new(
                            "onboarding.begin.button",
                            "Create new identity",
                            Some(KEY_BEGIN),
                        )
                        .with_style(ui::widget::ButtonStyle::Primary),
                    ),
                ])),
        )
    }

    fn connect(&self, theme: &Theme) -> Box<dyn Widget> {
        let path = TextInput::new(Vec::new())
            .with_id("onboarding.connect.path.field")
            .with_on_change_path(KEY_CONNECT_PATH)
            .with_placeholder("/path/to/primary.json")
            .with_font_size(13.0);
        let mut body = Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(heading("Connect an existing identity"))
                .push(paragraph(
                    "Choose a qOS identity record exported from another device. It is validated and copied into this user's private identity store before use.",
                    theme,
                ));
        if let Some(error) = &self.error {
            body = body.push(Box::new(
                Banner::error("That identity could not be connected").with_message(error.clone()),
            ));
        }
        Box::new(
            body.push(Box::new(Card::titled("Identity record", Box::new(path))))
                .push(actions(vec![Box::new(
                    Button::new(
                        "onboarding.import.button",
                        "Connect identity",
                        Some(KEY_IMPORT),
                    )
                    .with_style(ui::widget::ButtonStyle::Primary),
                )])),
        )
    }

    fn no_recovery(&self, _theme: &Theme) -> Box<dyn Widget> {
        let mut list = Column::new().with_no_padding().with_gap(14.0);
        for point in NO_RECOVERY_POINTS {
            list = list.push(bullet(point));
        }
        // The gate. Disabled until the box is ticked, so the primary action can't be clicked past the consequence.
        let create = Button::new(
            "onboarding.create.button",
            "Create my identity",
            Some(KEY_CREATE),
        )
        .with_style(ui::widget::ButtonStyle::Primary)
        .with_enabled(self.flow.can_create());
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(Banner::warning(NO_RECOVERY_TITLE).with_message(
                    "Read this before continuing — it cannot be undone later.",
                )))
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
            .map(super::onboarding::MintedIdentity::grouped_fingerprint)
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
                // Repeated deliberately: the last moment the user is looking at the
                // subject is the one fact they need to carry away from the whole ceremony.
                .push(paragraph(
                    "There is still no way to recover this key. If this device goes, so does \
                     this identity.",
                    theme,
                ))
                .push(actions(vec![Box::new(
                    Button::new(
                        "onboarding.finish.button",
                        "Start using qOS",
                        Some(KEY_FINISH),
                    )
                    .with_style(ui::widget::ButtonStyle::Primary),
                )])),
        )
    }
}

fn heading(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_size(26.0))
}

/// Secondary body copy. Takes the live theme rather than a hard-coded
/// one — a dark-theme dim grey on a light background is barely legible.
fn paragraph(text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_size(13.0).with_color(theme.text_dim))
}

/// A list item.
///
/// The marker is part of the same `Text` rather than a sibling in a
/// `Row`: these lines wrap, and a marker in its own column detaches from its sentence.
fn bullet(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(&format!("•   {text}")).with_size(13.0))
}

/// A right-aligned action row, so the primary button lands where the eye finishes reading.
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

fn read_string(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(text) => text.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

impl Widget for OnboardingView {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let before = self.flow.screen();

        if read_bool(state, root, KEY_BEGIN) {
            let _ = state.put(root, KEY_BEGIN, ScalarValue::Boolean(false));
            self.flow.begin();
        }

        if read_bool(state, root, KEY_CONNECT) {
            let _ = state.put(root, KEY_CONNECT, ScalarValue::Boolean(false));
            self.flow.begin_connect();
        }

        if read_bool(state, root, KEY_IMPORT) {
            let _ = state.put(root, KEY_IMPORT, ScalarValue::Boolean(false));
            let source = PathBuf::from(read_string(state, root, KEY_CONNECT_PATH));
            if self.flow.screen() == Screen::Connect {
                match (self.importer)(&source) {
                    Ok(identity) => {
                        self.error = None;
                        self.flow.complete_connected(identity);
                    }
                    Err(error) => {
                        tracing::error!(target: "qos::onboarding", %error, "identity import failed");
                        self.error = Some(error);
                    }
                }
            }
        }

        // The checkbox drives the gate directly rather than being latched, so unticking really does close it again.
        let understood = read_bool(state, root, KEY_UNDERSTOOD);
        if understood != self.flow.acknowledged() {
            self.flow.set_acknowledged(understood);
            self.structure_dirty = true;
        }

        if read_bool(state, root, KEY_CREATE) {
            let _ = state.put(root, KEY_CREATE, ScalarValue::Boolean(false));
            // `can_create` is checked inside `complete` too — this is the outer guard, that one is the invariant.
            if self.flow.can_create() {
                match (self.minter)() {
                    Ok(minted) => {
                        self.flow.complete(minted);
                    }
                    Err(e) => {
                        tracing::error!(target: "qos::onboarding", error = %e, "identity minting failed");
                        self.error = Some(e);
                        // Force the result screen so the failure is shown rather than leaving the user on a dead button.
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

        // Hold the text to a comfortable measure and let the root centre it —
        // full-screen, so without this the copy would run the width of a 4K display.
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::length(CONTENT_WIDTH);
            // Auto height, or the column claims the full viewport and there's nothing left for the root to centre.
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
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
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
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
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

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(std::convert::AsRef::as_ref)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
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
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// The whole point: clicking through without ticking the box must not produce a key.
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
        let _ = doc.put(
            &automerge::ROOT,
            KEY_UNDERSTOOD,
            ScalarValue::Boolean(false),
        );
        v.sync(&mut doc, &automerge::ROOT);
        assert!(!v.flow().can_create());
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A minting failure must be shown, not swallowed into a desktop that silently has no identity.
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

    #[test]
    fn connect_existing_imports_without_minting() {
        let imported = Arc::new(AtomicUsize::new(0));
        let imported_for_handler = imported.clone();
        let mut view = OnboardingView::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |path| {
                imported_for_handler.fetch_add(1, Ordering::SeqCst);
                assert_eq!(path, &PathBuf::from("/media/backup/primary.json"));
                Ok(MintedIdentity {
                    fingerprint: "cd".repeat(32),
                    path: PathBuf::from("/home/user/.config/qos/identity/cd.json"),
                })
            }),
        );
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_CONNECT);
        let _ = doc.put(
            &automerge::ROOT,
            KEY_CONNECT_PATH,
            ScalarValue::Str("/media/backup/primary.json".into()),
        );
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(view.flow().screen(), Screen::Connect);
        press(&mut doc, KEY_IMPORT);
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(imported.load(Ordering::SeqCst), 1);
        assert_eq!(view.flow().screen(), Screen::Done);
        assert!(view.flow().is_done());
    }

    #[test]
    fn failed_connect_stays_on_connect_screen_and_shows_error() {
        let mut view = OnboardingView::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(|_| Err("not a qOS identity".to_string())),
        );
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_CONNECT);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_IMPORT);
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(view.flow().screen(), Screen::Connect);
        assert_eq!(view.error(), Some("not a qOS identity"));
    }

    /// Every screen must lay out — a ceremony that panics on the screen after the one under test is worse than no ceremony.
    #[test]
    fn every_screen_builds_a_layout() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        for step in [
            None,
            Some(KEY_BEGIN),
            Some(KEY_UNDERSTOOD),
            Some(KEY_CREATE),
        ] {
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
