//! The identity ceremony: a state machine of screens for bootstrapping
//! an identity, pairing a second device via DKG, handling a genuine
//! threshold recovery share, and minting/accepting channel invites over
//! the `threshold-identity` engine.
//!
//! A new single-device identity intentionally skips the recovery-word
//! screens. The production OOBE keeps that identity's secret behind the
//! runtime provider boundary and recovers it with the authenticated encrypted
//! portable backup instead.
//!
//! Screens are boxed and pre-built rather than one inline enum, so a
//! transition is a constant-time `Box::new` swap.
//!
//! The protocol behind the screens lives in `tilo-identity-ceremony`, so
//! hosts and tests drive it without building a `TaffyTree`.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use std::sync::Arc;

use crate::widget::TaffyTreeExt;
use taffy::{NodeId, TaffyTree};
use tilo_identity_ceremony::IdentityStore;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

pub mod add_device_host;
pub mod create_identity;
pub mod invite_accept;
pub mod invite_mint;
pub mod pair_device;
pub mod recovery_share;
pub mod recovery_verify;
mod sheet;

/// Terminal "Done" success card, on the shared sheet: avatar disc and
/// name on one row, the two closing lines under them. Falls back to a
/// generic message if the store has no record.
fn draw_done_card(
    store: &dyn IdentityStore,
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    viewport: Rect,
) {
    use sheet::{Align, FORM_SHEET_WIDTH, Role, Rows, Sheet, SheetSpec, line_height, paint_text};
    use vello::peniko::{Color, Fill};

    const TITLE: &str = "You're all set";
    const SAVED: &str = "Your identity is saved. You can close this window.";
    const NEXT: &str = "Start the desktop from a terminal to begin collaborating.";

    let (display_name, avatar) = match store.load() {
        Ok(Some(rec)) => (rec.display_name.clone(), rec.avatar_color),
        _ => ("Your identity".to_string(), [120, 120, 200]),
    };
    let signed_in = format!("Signed in as {display_name}");
    let spec = SheetSpec {
        title: TITLE,
        max_width: FORM_SHEET_WIDTH,
        actions: &[],
    };
    let sp = &theme.tokens.spacing;
    // The avatar is one `s12` disc; the name row is as tall as the disc and
    // the two closing captions sit one `s6` below it, `s1` apart.
    let avatar_d = f64::from(sp.s12);
    let caption_h = line_height(text_renderer, theme, Role::Caption);
    let plan = |mut rows: Rows| {
        let name_row = rows.row(avatar_d);
        rows.gap(sp.s6);
        let saved = rows.row(caption_h);
        rows.gap(sp.s1);
        let next = rows.row(caption_h);
        (name_row, saved, next, rows.bottom())
    };
    let body_w = Sheet::body_width(viewport, spec.max_width, theme);
    let (_, _, _, wanted) = plan(Rows::measuring(body_w));
    let sheet = Sheet::layout(viewport, &spec, wanted, text_renderer, theme);
    let (name_row, saved, next, _) = plan(Rows::new(sheet.body()));

    sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
    let avatar_centre = (name_row.x0 + avatar_d / 2.0, name_row.center().y);
    // A per-person colour is identity, not decoration (HIG §9.4), so it is
    // the one fill here the theme does not own; its initial takes the ink
    // that reads against it rather than an assumed white.
    let avatar_color = Color::from_rgb8(avatar[0], avatar[1], avatar[2]);
    scene.fill(
        Fill::NonZero,
        transform,
        avatar_color,
        None,
        &vello::kurbo::Circle::new(avatar_centre, avatar_d / 2.0),
    );
    let initial: String = display_name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let disc = Rect::new(
        name_row.x0,
        name_row.y0,
        name_row.x0 + avatar_d,
        name_row.y1,
    );
    sheet::paint_text_in(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        disc,
        &initial,
        Role::Code,
        theme.on_surface(avatar_color).ink,
        Align::Center,
        false,
    );
    let name_rect = Rect::new(
        disc.x1 + f64::from(sp.s4),
        name_row.y0,
        name_row.x1,
        name_row.y1,
    );
    paint_text(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        name_rect,
        &signed_in,
        Role::Value,
        Align::Start,
        false,
    );
    for (rect, label) in [(saved, SAVED), (next, NEXT)] {
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
            label,
            Role::Caption,
            Align::Start,
            false,
        );
    }
}

/// Identifier for the current screen in the ceremony.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityFlowScreen {
    /// Screen 1.
    CreateIdentity,
    /// Screen 2.
    RecoveryShare,
    /// Screen 2b.
    RecoveryVerify,
    /// Screen 3.
    AddDeviceHost,
    /// Screen 4.
    PairDevice,
    /// Screen 5.
    InviteMint,
    /// Screen 6.
    InviteAccept,
    /// Terminal — the host should transition to the desktop.
    Done,
}

/// Outcome the screen reports back to the container; drives the
/// state-machine transition.
#[derive(Clone, Debug)]
pub enum ScreenOutcome {
    Advance,
    /// Cancel — back to the previous screen, or close the flow at the
    /// start.
    Back,
    Stay,
    /// Branch to a specific screen (e.g. invite-accept jumping to
    /// Done on decline).
    JumpTo(IdentityFlowScreen),
}

/// The shell instantiates one of these on boot when no identity
/// exists; the example bin instantiates one programmatically with
/// `set_screen` to inspect any screen in isolation.
pub struct IdentityFlow {
    current: IdentityFlowScreen,
    /// All seven screens are pre-built and toggled via `current`, so
    /// per-screen state (text-input contents, etc.) survives navigation.
    pub create_identity: Box<create_identity::CreateIdentityScreen>,
    pub recovery_share: Box<recovery_share::RecoveryShareScreen>,
    pub recovery_verify: Box<recovery_verify::RecoveryVerifyScreen>,
    pub add_device_host: Box<add_device_host::AddDeviceHostScreen>,
    pub pair_device: Box<pair_device::PairDeviceScreen>,
    pub invite_mint: Box<invite_mint::InviteMintScreen>,
    pub invite_accept: Box<invite_accept::InviteAcceptScreen>,
    /// Persistence backend the create screen writes to on submit.
    store: Arc<dyn IdentityStore>,
    rect: Rect,
}

impl IdentityFlow {
    pub fn new(store: Arc<dyn IdentityStore>) -> Self {
        Self {
            current: IdentityFlowScreen::CreateIdentity,
            create_identity: Box::new(create_identity::CreateIdentityScreen::new()),
            recovery_share: Box::new(recovery_share::RecoveryShareScreen::new()),
            recovery_verify: Box::new(recovery_verify::RecoveryVerifyScreen::new()),
            add_device_host: Box::new(add_device_host::AddDeviceHostScreen::new()),
            pair_device: Box::new(pair_device::PairDeviceScreen::new()),
            invite_mint: Box::new(invite_mint::InviteMintScreen::new()),
            invite_accept: Box::new(invite_accept::InviteAcceptScreen::new()),
            store,
            rect: Rect::ZERO,
        }
    }

    pub fn current_screen(&self) -> IdentityFlowScreen {
        self.current
    }

    /// Used by the example bin and tests to jump straight to a screen.
    pub fn set_screen(&mut self, screen: IdentityFlowScreen) {
        self.current = screen;
    }

    pub fn advance(&mut self, outcome: ScreenOutcome) {
        let next = match (self.current, outcome) {
            (_, ScreenOutcome::Stay) => self.current,
            (_, ScreenOutcome::JumpTo(s)) => s,
            // A newly created 1-of-1 identity has no threshold recovery
            // share. Older builds routed it through RecoveryShare with words
            // derived from its public key; those words were non-secret and
            // could not recover anything. Finish creation instead. The
            // RecoveryShare screens remain available to the explicit
            // multi-device ceremony once DKG has produced a genuine share.
            (IdentityFlowScreen::CreateIdentity, ScreenOutcome::Advance) => {
                IdentityFlowScreen::Done
            }
            (IdentityFlowScreen::RecoveryVerify, ScreenOutcome::Back) => {
                IdentityFlowScreen::RecoveryShare
            }
            (IdentityFlowScreen::RecoveryShare, ScreenOutcome::Advance) => {
                IdentityFlowScreen::RecoveryVerify
            }
            (IdentityFlowScreen::RecoveryShare, ScreenOutcome::Back) => {
                IdentityFlowScreen::CreateIdentity
            }
            (IdentityFlowScreen::RecoveryVerify, ScreenOutcome::Advance)
            | (
                IdentityFlowScreen::AddDeviceHost
                | IdentityFlowScreen::PairDevice
                | IdentityFlowScreen::InviteMint
                | IdentityFlowScreen::InviteAccept,
                ScreenOutcome::Advance | ScreenOutcome::Back,
            )
            | (IdentityFlowScreen::Done, _) => IdentityFlowScreen::Done,
            (other, _) => other,
        };
        self.current = next;
    }

    /// True once the host should swap to the desktop.
    pub fn is_done(&self) -> bool {
        matches!(self.current, IdentityFlowScreen::Done)
    }

    pub fn store(&self) -> &Arc<dyn IdentityStore> {
        &self.store
    }
}

impl IdentityFlow {
    /// The screen currently shown, as a widget — `None` on the terminal
    /// card, which the flow paints itself.
    fn current_widget(&self) -> Option<&dyn Widget> {
        Some(match self.current {
            IdentityFlowScreen::CreateIdentity => &*self.create_identity,
            IdentityFlowScreen::RecoveryShare => &*self.recovery_share,
            IdentityFlowScreen::RecoveryVerify => &*self.recovery_verify,
            IdentityFlowScreen::AddDeviceHost => &*self.add_device_host,
            IdentityFlowScreen::PairDevice => &*self.pair_device,
            IdentityFlowScreen::InviteMint => &*self.invite_mint,
            IdentityFlowScreen::InviteAccept => &*self.invite_accept,
            IdentityFlowScreen::Done => return None,
        })
    }

    fn current_widget_mut(&mut self) -> Option<&mut (dyn Widget + '_)> {
        Some(match self.current {
            IdentityFlowScreen::CreateIdentity => &mut *self.create_identity,
            IdentityFlowScreen::RecoveryShare => &mut *self.recovery_share,
            IdentityFlowScreen::RecoveryVerify => &mut *self.recovery_verify,
            IdentityFlowScreen::AddDeviceHost => &mut *self.add_device_host,
            IdentityFlowScreen::PairDevice => &mut *self.pair_device,
            IdentityFlowScreen::InviteMint => &mut *self.invite_mint,
            IdentityFlowScreen::InviteAccept => &mut *self.invite_accept,
            IdentityFlowScreen::Done => return None,
        })
    }
}

impl Widget for IdentityFlow {
    /// The active screen is a child, so the headless driver, the inspector
    /// and assistive technology can reach it by its `test_id`. Without this
    /// the flow was a leaf and no screen was addressable at all.
    fn children(&self) -> Vec<&dyn Widget> {
        self.current_widget().into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.current_widget_mut().into_iter().collect()
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow")
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        match self.current {
            IdentityFlowScreen::CreateIdentity => self.create_identity.sync(state, path),
            IdentityFlowScreen::RecoveryShare => self.recovery_share.sync(state, path),
            IdentityFlowScreen::RecoveryVerify => self.recovery_verify.sync(state, path),
            IdentityFlowScreen::AddDeviceHost => self.add_device_host.sync(state, path),
            IdentityFlowScreen::PairDevice => self.pair_device.sync(state, path),
            IdentityFlowScreen::InviteMint => self.invite_mint.sync(state, path),
            IdentityFlowScreen::InviteAccept => self.invite_accept.sync(state, path),
            IdentityFlowScreen::Done => {}
        }
    }

    /// The flow is a real parent node with the active screen as its one
    /// child, matching `children()`. Returning the screen's own node here
    /// used to be enough; now that a screen has children of its own (its
    /// embedded text fields), every walk that zips `children()` against
    /// `taffy.children(node)` — focus order, accessibility — needs the two
    /// trees to agree at this level too.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let screen = match self.current_widget_mut() {
            Some(screen) => vec![screen.build_layout(taffy, theme, node_map)],
            // The terminal card owns the viewport like every other screen;
            // a default (zero-width) leaf centred it on the window's left
            // edge, half off-screen.
            None => Vec::new(),
        };
        taffy.widget_node(sheet::screen_style(), &screen, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = crate::widget::placed(&layout, origin);
        // The screen is the flow's first taffy child; its layout is
        // relative to the flow's rect.
        let screen_node = taffy
            .children(node)
            .ok()
            .and_then(|children| children.first().copied());
        let screen_origin = Point::new(self.rect.x0, self.rect.y0);
        if let (Some(screen_node), Some(screen)) = (screen_node, self.current_widget_mut()) {
            screen.apply_layout(taffy, screen_node, screen_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Identity ceremonies, invites and joins are security surfaces:
        // every look-alike letter is set apart.
        text_renderer.security_surface(|text_renderer| match self.current {
            IdentityFlowScreen::CreateIdentity => {
                self.create_identity
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::RecoveryShare => {
                self.recovery_share
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::RecoveryVerify => {
                self.recovery_verify
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::AddDeviceHost => {
                self.add_device_host
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::PairDevice => {
                self.pair_device
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::InviteMint => {
                self.invite_mint
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::InviteAccept => {
                self.invite_accept
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            IdentityFlowScreen::Done => {
                draw_done_card(
                    self.store.as_ref(),
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    self.rect,
                );
            }
        });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let outcome = match self.current {
            IdentityFlowScreen::CreateIdentity => {
                self.create_identity
                    .handle_event_screen(event, ctx, &*self.store)
            }
            IdentityFlowScreen::RecoveryShare => {
                self.recovery_share.handle_event_screen(event, ctx)
            }
            IdentityFlowScreen::RecoveryVerify => {
                self.recovery_verify.handle_event_screen(event, ctx)
            }
            IdentityFlowScreen::AddDeviceHost => {
                self.add_device_host.handle_event_screen(event, ctx)
            }
            IdentityFlowScreen::PairDevice => self.pair_device.handle_event_screen(event, ctx),
            IdentityFlowScreen::InviteMint => self.invite_mint.handle_event_screen(event, ctx),
            IdentityFlowScreen::InviteAccept => self.invite_accept.handle_event_screen(event, ctx),
            IdentityFlowScreen::Done => ScreenOutcome::Stay,
        };
        self.advance(outcome);
    }

    fn name(&self) -> &'static str {
        "IdentityFlow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }
}

/// Synthetic `winit` events for the screens' unit tests, which drive
/// `handle_event_screen` directly rather than through a harness. One copy
/// here rather than one per screen module.
#[cfg(test)]
pub(super) mod test_events {
    use taffy::{NodeId, Style, TaffyTree};
    use vello::kurbo::Point;
    use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    use crate::widget::TaffyTreeExt;

    /// A key press carrying `logical_key` and the OS's printable `text`,
    /// built the way the headless driver builds its own key events.
    pub(crate) fn key(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let text: Option<winit::keyboard::SmolStr> = text.map(Into::into);
        let event = KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                ),
                state: ElementState::Pressed,
            },
            logical_key.clone(),
            text.clone(),
            KeyLocation::Standard,
            false,
            text,
            logical_key,
        );
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    /// A printable character press.
    pub(crate) fn char(ch: &str) -> WindowEvent {
        key(Key::Character(ch.into()), Some(ch))
    }

    /// A named (non-printing) key press.
    pub(crate) fn named(k: NamedKey) -> WindowEvent {
        key(Key::Named(k), None)
    }

    pub(crate) fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    pub(crate) fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// A throwaway tree and node for `handle_event_screen`'s parameters.
    pub(crate) fn scratch_node() -> (TaffyTree<()>, NodeId) {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        (taffy, node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_identity_ceremony::MemoryIdentityStore;

    #[test]
    fn single_device_creation_never_enters_a_fake_recovery_word_screen() {
        let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
        let mut flow = IdentityFlow::new(store);

        assert_eq!(flow.current_screen(), IdentityFlowScreen::CreateIdentity);
        flow.advance(ScreenOutcome::Advance);

        assert_eq!(flow.current_screen(), IdentityFlowScreen::Done);
        assert!(flow.recovery_share.mnemonic().is_empty());
    }
}
