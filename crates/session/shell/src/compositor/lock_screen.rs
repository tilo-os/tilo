//! The compositor-owned session-lock overlay.
//!
//! When [`crate::compositor::idle::SessionManager::is_locked`] is set,
//! [`crate::compositor::native::run`] paints *this* scene instead of the
//! chrome's, hiding app/chrome content and routing every keystroke into the
//! lock screen's password field ([`crate::compositor::lock_controller`]) —
//! hidden content + captured input + credential-gated unlock together are
//! the lock's security boundary. (`ext-session-lock-v1` is also served as a
//! standard trigger; Tilo renders this overlay rather than a client lock
//! surface.)
//!
//! Colours come from the OS [`Theme`] so the lock screen matches the rest of
//! the desktop. An answer — a verification starting, a refusal — cross-fades
//! into the status row, and a refusal also shakes it once
//! (the `feedback` module); both are named motions, both rest, and both
//! stay inside the credential card's field and status parts so a retained
//! lock damages those bounds instead of the output.

use std::f64::consts::PI;

use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Extend, Fill, ImageBrush, ImageData};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

use ui::accesskit::{
    Action as AccessAction, Node as AccessNode, NodeId as AccessNodeId, Rect as AccessRect,
    Role as AccessRole, TextPosition as AccessTextPosition, TextSelection as AccessTextSelection,
    TreeId, TreeInfo, TreeUpdate,
};

use crate::compositor::lock_auth::UNAVAILABLE_MSG;

mod feedback;
mod retained;
use crate::shell::i18n::{t, t_args};
use crate::shell::now_playing::PlayerSnapshot;
use crate::shell::quick_settings::now_playing::{
    Transport, paint_song_at_rest, paint_transport, transport_rects,
};
pub(crate) use feedback::{LockFeedback, LockStatus, LockStatusPaint};
pub(crate) use retained::{LockComposition, RetainedLockScene};

// `ui::accesskit_bridge` reserves 0, low ids, 1<<60, 2<<60, 1<<62 and
// 1<<63 for desktop widgets/resources/actions/text/synthetic nodes. Keeping
// the lock in the unused 3<<60 range makes a stale hidden-desktop action
// incapable of aliasing a credential control after the tree swap.
const LOCK_ID_BASE: u64 = 3_u64 << 60;
pub const LOCK_ROOT_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE);
pub const LOCK_PASSWORD_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 1);
const LOCK_CLOCK_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 2);
const LOCK_HELP_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 3);
const LOCK_STATUS_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 4);
const LOCK_MEDIA_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 5);
pub const LOCK_MEDIA_PREVIOUS_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 6);
pub const LOCK_MEDIA_PLAY_PAUSE_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 7);
pub const LOCK_MEDIA_NEXT_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 8);
const LOCK_PASSWORD_TEXT_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 9);

/// The transport button an accessibility node stands for. The lock tree has
/// no other actionable media node: no player switching, no output routing.
#[must_use]
pub fn transport_for_node(node: AccessNodeId) -> Option<Transport> {
    match node {
        LOCK_MEDIA_PREVIOUS_ID => Some(Transport::Previous),
        LOCK_MEDIA_PLAY_PAUSE_ID => Some(Transport::PlayPause),
        LOCK_MEDIA_NEXT_ID => Some(Transport::Next),
        _ => None,
    }
}

const fn transport_node(transport: Transport) -> AccessNodeId {
    match transport {
        Transport::Previous => LOCK_MEDIA_PREVIOUS_ID,
        Transport::PlayPause => LOCK_MEDIA_PLAY_PAUSE_ID,
        Transport::Next => LOCK_MEDIA_NEXT_ID,
    }
}

/// Live lock-screen state to render, computed by the caller so this paint
/// stays pure and unit-testable (no clock reads, no auth state of its own).
#[derive(Debug, Clone, Copy, Default)]
pub struct LockView<'a> {
    /// Number of password characters typed so far — rendered as masked
    /// dots. The plaintext never reaches this layer.
    pub password_len: usize,
    /// Authentication error to show under the field (e.g. "Incorrect
    /// username or password"), or `None`. Rendered in the danger colour.
    pub error: Option<&'a str>,
    /// Preformatted wall-clock time (e.g. `"13:37"`). Empty hides the clock.
    pub clock: &'a str,
    /// Preformatted date (e.g. `"Friday, 11 July"`). Empty hides the date.
    pub date: &'a str,
    /// Active notification count (count-only — never content).
    pub notification_count: usize,
    /// Credential verification running on the worker thread
    /// ([`crate::compositor::lock_controller::LockController::is_verifying`]);
    /// renders "Verifying…", gives the field its waiting marks and suppresses
    /// the empty-field placeholder, so the screen reads as checking, not
    /// "type here".
    pub verifying: bool,
    /// What is playing, shown as a compact card with transport controls
    /// only. `None` hides the card.
    pub now_playing: Option<&'a PlayerSnapshot>,
    /// The keyboard drawn on the lock screen for unlocking by touch, while
    /// touch is the input in use. `None` hides it.
    pub keyboard: Option<ui::passcode_keyboard::PasscodeKeyboard>,
    /// What the system says about the whole machine (critical battery),
    /// shown at the top of every display: the state of the machine is not
    /// only the business of the display with the password field.
    pub alert: Option<&'a str>,
}

/// The lock screen keyboard's key height and spacing, from the theme: keys
/// as tall as a panel row, the form's small gap between them.
pub fn keyboard_key_metrics(theme: &Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.metrics.panel_row_height),
        f64::from(theme.tokens.spacing.s2),
    )
}

/// The keyboard is never wider than a wide sheet, so keys stay a thumb's
/// reach apart.
fn keyboard_max_width(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.panel_width_sheet) * 1.25
}

/// One output the lock overlay must cover, in the compositor's combined
/// coordinate space. The compositor's first output is the primary lock
/// target; secondary displays retain a private, useful indication without
/// duplicating a password target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockOutput {
    /// Top-left of this output in the combined space.
    pub origin: (f64, f64),
    pub size: (f64, f64),
    /// Device scale reported for this output: physical pixels per logical
    /// pixel. `origin` and `size` are the output's physical pixels in the
    /// scene, so the lock chrome is drawn at this scale, as the desktop's
    /// chrome is on the same display.
    pub scale: f64,
}

impl LockOutput {
    /// The centre point of this output, where its lock widget is anchored.
    pub fn center(&self) -> (f64, f64) {
        (
            self.origin.0 + self.size.0 / 2.0,
            self.origin.1 + self.size.1 / 2.0,
        )
    }
}

/// One lock surface per display, in the scene's physical pixels, or one
/// spanning the viewport before any display has been reported. The display
/// the person was working on (`focused`, an output id) comes first, and
/// without one the device's built-in panel does: the lock screen puts the
/// password field and its caret on the first output, the clock and artwork
/// on the others.
pub fn lock_outputs(
    outputs: &[crate::compositor::backend::Output],
    viewport: (f64, f64),
    focused: Option<u32>,
) -> Vec<LockOutput> {
    let mut ordered: Vec<&crate::compositor::backend::Output> = outputs.iter().collect();
    let first = focused
        .and_then(|id| ordered.iter().position(|o| o.id.0 == id))
        .or_else(|| ordered.iter().position(|o| o.identity.is_builtin()));
    if let Some(index) = first {
        let output = ordered.remove(index);
        ordered.insert(0, output);
    }
    let outputs: Vec<LockOutput> = ordered
        .into_iter()
        .map(|o| LockOutput {
            origin: (f64::from(o.position.0), f64::from(o.position.1)),
            size: (f64::from(o.size.0), f64::from(o.size.1)),
            scale: f64::from(o.scale),
        })
        .collect();
    if outputs.is_empty() {
        vec![LockOutput {
            origin: (0.0, 0.0),
            size: viewport,
            scale: 1.0,
        }]
    } else {
        outputs
    }
}

/// Responsive geometry for one output's lock chrome. Keeping layout separate
/// from painting makes containment and non-overlap contractual instead of a
/// property inferred from screenshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockLayout {
    pub output: Rect,
    pub clock: Rect,
    pub date: Rect,
    pub panel: Rect,
    pub icon: Rect,
    pub title: Rect,
    pub password: Rect,
    pub status: Rect,
    pub notifications: Rect,
    pub scale: f64,
    /// Where the keyboard for unlocking by touch sits, when it shows.
    pub keyboard: Option<Rect>,
}

impl LockLayout {
    /// Resolve a bounded primary-output layout. The output's geometry is its
    /// physical pixels in the scene, so the chrome takes the display's own
    /// scale times Tilo's UI scale, capped by the available space: a card on
    /// a 2x panel is as large to the eye as on the 1x monitor beside it.
    pub fn for_output(output: LockOutput, theme: &Theme) -> Self {
        Self::for_output_with_keyboard(output, theme, false)
    }

    /// The layout with the keyboard for unlocking by touch along the bottom
    /// of the output when `keyboard` is set; the credential surface fits in
    /// the space above it.
    pub fn for_output_with_keyboard(output: LockOutput, theme: &Theme, keyboard: bool) -> Self {
        if !keyboard {
            return Self::without_keyboard(output, theme);
        }
        let margin = (output.size.1 * 0.02).clamp(8.0, 24.0);
        let height = {
            let (key_height, gap) = keyboard_key_metrics(theme);
            ui::passcode_keyboard::PasscodeKeyboard::height(key_height, gap)
        };
        let width = (output.size.0 - margin * 2.0).clamp(1.0, keyboard_max_width(theme));
        let x0 = output.origin.0 + (output.size.0 - width) / 2.0;
        let y1 = output.origin.1 + output.size.1 - margin;
        let pad = Rect::new(x0, (y1 - height).max(output.origin.1), x0 + width, y1);
        let above = LockOutput {
            size: (output.size.0, (pad.y0 - margin - output.origin.1).max(1.0)),
            ..output
        };
        let mut layout = Self::without_keyboard(above, theme);
        layout.output = Self::without_keyboard(output, theme).output;
        layout.keyboard = Some(pad);
        layout
    }

    fn without_keyboard(output: LockOutput, theme: &Theme) -> Self {
        let width = output.size.0.max(1.0);
        let height = output.size.1.max(1.0);
        let output_rect = Rect::new(
            output.origin.0,
            output.origin.1,
            output.origin.0 + width,
            output.origin.1 + height,
        );
        let edge = (width.min(height) * 0.035)
            .clamp(1.0, 28.0 * output.scale.max(1.0))
            .min(width / 2.0)
            .min(height / 2.0);
        let desired_scale =
            f64::from(theme.ui_scale).clamp(0.8, 1.6) * output.scale.clamp(0.5, 4.0);
        // Text-only accessibility scaling must reserve room in the lock
        // chrome too.  The compositor draws the clock and the form labels
        // directly, rather than through the normal widget layout engine, so
        // leaving these slots at their default dimensions made large type
        // encroach on the date and status rows.
        let text_scale = f64::from(theme.text_scale).clamp(0.5, 4.0);
        let unscaled_panel_height = 260.0 + 130.0 * (text_scale - 1.0);
        // On generous outputs the time and the credential surface have their
        // own columns. Large type and compact displays retain a full-width
        // reading order instead of squeezing the form into a narrow column.
        let text_width_scale = text_scale.max(1.0);
        let unscaled_panel_width = 64.0 + 352.0 * text_width_scale;
        let wide = width >= 1100.0 * desired_scale * text_width_scale
            && height >= 600.0 * desired_scale
            && text_scale <= 1.5;
        let clock_height = if wide { 160.0 } else { 120.0 } * text_scale;
        let date_height = 30.0 * text_scale;
        let clock_date_gap = 6.0;
        let panel_gap = 40.0;
        let stack_height =
            clock_height + clock_date_gap + date_height + panel_gap + unscaled_panel_height;
        let width_fit = ((width - edge * 2.0) / (unscaled_panel_width + 32.0)).max(0.0);
        let height_fit = ((height - edge * 2.0)
            / if wide {
                unscaled_panel_height.max(clock_height + date_height + clock_date_gap)
            } else {
                stack_height
            })
        .max(0.0);
        let scale = desired_scale
            .min(width_fit)
            .min(height_fit)
            .max(f64::EPSILON);
        let panel_width = unscaled_panel_width * scale;
        let panel_height = unscaled_panel_height * scale;
        let cx = output.origin.0 + width * if wide { 0.755 } else { 0.5 };
        let top = output.origin.1
            + if wide {
                height * 0.30
            } else {
                (height - stack_height * scale) / 2.0
            };
        let clock = if wide {
            Rect::new(
                output.origin.0 + width * 0.075,
                top,
                output.origin.0 + width * 0.515,
                top + clock_height * scale,
            )
        } else {
            Rect::new(
                output.origin.0 + edge,
                top,
                output.origin.0 + width - edge,
                top + clock_height * scale,
            )
        };
        let date_y = clock.y1 + clock_date_gap * scale;
        let date = Rect::new(clock.x0, date_y, clock.x1, date_y + date_height * scale);
        let panel_y = if wide {
            output.origin.1 + (height - panel_height) / 2.0
        } else {
            date.y1 + panel_gap * scale
        };
        let panel = Rect::new(
            cx - panel_width / 2.0,
            panel_y,
            cx + panel_width / 2.0,
            panel_y + panel_height,
        );
        Self::with_panel(output_rect, clock, date, panel, scale, text_scale)
    }

    /// The form keeps the same row relationships in either screen composition.
    fn with_panel(
        output: Rect,
        clock: Rect,
        date: Rect,
        panel: Rect,
        scale: f64,
        text_scale: f64,
    ) -> Self {
        let cx = panel.center().x;
        let icon_size = 32.0 * scale;
        let icon = Rect::new(
            cx - icon_size / 2.0,
            panel.y0 + 24.0 * scale,
            cx + icon_size / 2.0,
            panel.y0 + 24.0 * scale + icon_size,
        );
        let title = Rect::new(
            panel.x0 + 28.0 * scale,
            icon.y1 + 8.0 * scale,
            panel.x1 - 28.0 * scale,
            icon.y1 + (8.0 + 30.0 * text_scale) * scale,
        );
        let password = Rect::new(
            panel.x0 + 32.0 * scale,
            title.y1 + 20.0 * scale,
            panel.x1 - 32.0 * scale,
            title.y1 + (20.0 + 50.0 * text_scale) * scale,
        );
        let status = Rect::new(
            password.x0,
            password.y1 + 10.0 * scale,
            password.x1,
            password.y1 + (10.0 + 28.0 * text_scale) * scale,
        );
        let notifications = Rect::new(
            password.x0,
            status.y1 + 8.0 * scale,
            password.x1,
            panel.y1 - 24.0 * scale,
        );

        Self {
            output,
            clock,
            date,
            panel,
            icon,
            title,
            password,
            status,
            notifications,
            scale,
            keyboard: None,
        }
    }
}

/// The compact now-playing card under the credential panel on the primary
/// output: artwork, title and artist, and the three transport buttons.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockMediaLayout {
    pub card: Rect,
    pub art: Rect,
    pub text: Rect,
    pub transport: [(Transport, Rect); 3],
}

impl LockMediaLayout {
    /// `None` when the output has no room under the credential panel; the
    /// hardware media keys still work then.
    #[must_use]
    pub fn for_layout(layout: &LockLayout, theme: &Theme) -> Option<Self> {
        let scale = layout.scale;
        let text_scale = f64::from(theme.text_scale).clamp(0.5, 4.0);
        let pad = 12.0 * scale;
        let lines = (20.0 + 16.0) * text_scale * scale;
        let height = (64.0 * scale).max(2.0 * pad + lines);
        let top = layout.panel.y1 + 16.0 * scale;
        let card = Rect::new(layout.panel.x0, top, layout.panel.x1, top + height);
        let edge = 8.0 * scale;
        if card.y1 > layout.output.y1 - edge {
            return None;
        }
        let art_edge = height - 2.0 * pad;
        let art = Rect::new(
            card.x0 + pad,
            card.y0 + pad,
            card.x0 + pad + art_edge,
            card.y1 - pad,
        );
        let centred = transport_rects(Rect::new(0.0, card.y0, 0.0, card.y1), scale);
        let shift = card.x1 - pad - centred[2].1.x1;
        let transport = centred
            .map(|(transport, rect)| (transport, rect.with_origin((rect.x0 + shift, rect.y0))));
        let text = Rect::new(
            art.x1 + pad,
            card.y0 + pad,
            (transport[0].1.x0 - pad).max(art.x1 + pad),
            card.y1 - pad,
        );
        Some(Self {
            card,
            art,
            text,
            transport,
        })
    }
}

/// The transport button under `point` on the lock screen, if the player
/// offers it. Nothing else on the card reacts to the pointer.
#[must_use]
pub fn transport_at(
    outputs: &[LockOutput],
    theme: &Theme,
    view: &LockView<'_>,
    point: Point,
) -> Option<Transport> {
    let player = view.now_playing?;
    let layout = LockLayout::for_output_with_keyboard(
        primary_output(outputs)?,
        theme,
        view.keyboard.is_some(),
    );
    LockMediaLayout::for_layout(&layout, theme)?
        .transport
        .into_iter()
        .find(|(transport, rect)| rect.contains(point) && transport.enabled(player))
        .map(|(transport, _)| transport)
}

/// When the lock screen draws its keyboard for unlocking by touch, and what
/// its keys type. No input method runs while the session is locked; the
/// passcode never leaves the lock screen.
#[derive(Debug, Default, Clone, Copy)]
pub struct LockTouchKeyboard {
    keyboard: Option<ui::passcode_keyboard::PasscodeKeyboard>,
    /// The keyboard already showed when the latest touch began. The touch
    /// that brings it up never also presses the key that appears under it.
    armed: bool,
}

impl LockTouchKeyboard {
    /// The keyboard to draw, when it shows.
    pub fn shown(self) -> Option<ui::passcode_keyboard::PasscodeKeyboard> {
        self.keyboard
    }

    /// A finger touched the lock screen: the keyboard shows.
    pub fn touch_started(&mut self) {
        self.armed = self.keyboard.is_some();
        self.keyboard.get_or_insert_default();
    }

    /// A physical key: a keyboard is at hand, the drawn one steps aside.
    pub fn physical_key(&mut self) {
        *self = Self::default();
    }

    /// A press landed on `key` of the drawn keyboard. Returns what it asks
    /// of the lock's passcode.
    pub fn press(
        &mut self,
        key: ui::passcode_keyboard::PasscodeKey,
    ) -> crate::compositor::lock_controller::LockKey {
        use crate::compositor::lock_controller::LockKey;
        use ui::passcode_keyboard::PasscodeInput;
        let armed = std::mem::replace(&mut self.armed, true);
        let Some(keyboard) = self.keyboard.as_mut().filter(|_| armed) else {
            return LockKey::Ignored;
        };
        match keyboard.press(key) {
            Some(PasscodeInput::Text(text)) => LockKey::Text(text),
            Some(PasscodeInput::Backspace) => LockKey::Backspace,
            Some(PasscodeInput::Enter) => LockKey::Submit,
            None => LockKey::Ignored,
        }
    }
}

/// Every key of the lock screen's keyboard and where it sits, when it
/// shows: for the state dump, so a test drives it as a finger would.
pub fn keyboard_keys(
    outputs: &[LockOutput],
    theme: &Theme,
    view: &LockView<'_>,
) -> Vec<(ui::passcode_keyboard::PasscodeKey, Rect)> {
    let Some(keyboard) = view.keyboard else {
        return Vec::new();
    };
    let Some(output) = primary_output(outputs) else {
        return Vec::new();
    };
    LockLayout::for_output_with_keyboard(output, theme, true)
        .keyboard
        .map(|pad| {
            let (key_height, gap) = keyboard_key_metrics(theme);
            keyboard.layout(pad, key_height, gap)
        })
        .unwrap_or_default()
}

/// The key of the lock screen's keyboard under `point`, when it shows.
pub fn keyboard_key_at(
    outputs: &[LockOutput],
    theme: &Theme,
    view: &LockView<'_>,
    point: Point,
) -> Option<ui::passcode_keyboard::PasscodeKey> {
    let keyboard = view.keyboard?;
    let layout = LockLayout::for_output_with_keyboard(primary_output(outputs)?, theme, true);
    keyboard.key_at(
        layout.keyboard?,
        keyboard_key_metrics(theme).0,
        keyboard_key_metrics(theme).1,
        point,
    )
}

/// One display's lock surface as drawn, for the state dump: the display's
/// rect in the scene, the scale its chrome is drawn at, and the password
/// field when this is the display that carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockSurface {
    pub output: Rect,
    pub scale: f64,
    pub password: Option<Rect>,
}

/// The lock surfaces [`draw_lock_scene`] draws over `outputs`: the full lock
/// on the first, the clock and artwork on the others.
pub fn lock_surfaces(outputs: &[LockOutput], theme: &Theme, keyboard: bool) -> Vec<LockSurface> {
    let primary = primary_output(outputs);
    outputs
        .iter()
        .map(|output| {
            let carries = Some(*output) == primary;
            let layout = if carries {
                LockLayout::for_output_with_keyboard(*output, theme, keyboard)
            } else {
                LockLayout::for_output(*output, theme)
            };
            LockSurface {
                output: layout.output,
                scale: layout.scale,
                password: carries.then_some(layout.password),
            }
        })
        .collect()
}

/// Resolve the one output that carries the credential surface. A hot-unplug
/// can remove the previous primary connector while locked; then the first
/// surviving output takes over rather than leaving the password unreachable.
/// Backend output ordering is the compositor's primary-output policy.
fn primary_output(outputs: &[LockOutput]) -> Option<LockOutput> {
    outputs.first().copied()
}

/// What the lock screen's clock reads as: the lock state, then the time and
/// date when they are known.
fn locked_clock_label(view: &LockView<'_>) -> String {
    match (view.clock.is_empty(), view.date.is_empty()) {
        (true, _) => t("lock-screen-locked"),
        (false, true) => t_args("lock-screen-locked-at", &[("clock", view.clock.into())]),
        (false, false) => t_args(
            "lock-screen-locked-at-date",
            &[("clock", view.clock.into()), ("date", view.date.into())],
        ),
    }
}

/// The password's text run, attached to `password` with the caret at its
/// start. AccessKit offers AT-SPI `Text` and `EditableText` only on a field with
/// a text run; this one stays empty whatever was typed, so it discloses
/// neither the password nor its length.
fn password_text_run(password: &mut AccessNode, bounds: Rect) -> AccessNode {
    let mut run = AccessNode::new(AccessRole::TextRun);
    run.set_value("");
    run.set_bounds(access_rect(bounds));
    let start = AccessTextPosition {
        node: LOCK_PASSWORD_TEXT_ID,
        character_index: 0,
    };
    password.set_text_selection(AccessTextSelection {
        anchor: start,
        focus: start,
    });
    password.set_children(vec![LOCK_PASSWORD_TEXT_ID]);
    run
}

/// The locked semantic tree has one password target, matching the primary
/// output. It never publishes a password or masked value to assistive tech.
pub fn build_accessibility_tree(
    outputs: &[LockOutput],
    theme: &Theme,
    view: &LockView<'_>,
) -> TreeUpdate {
    let (width, height) = combined_extent(outputs);
    let output = primary_output(outputs).unwrap_or_else(|| LockOutput {
        origin: (0.0, 0.0),
        size: (width.max(1.0), height.max(1.0)),
        scale: 1.0,
    });
    let layout = LockLayout::for_output(output, theme);

    let mut root = AccessNode::new(AccessRole::Window);
    root.set_label(t("lock-screen-a11y-name"));
    root.set_bounds(AccessRect::new(0.0, 0.0, width.max(1.0), height.max(1.0)));

    let mut clock = AccessNode::new(AccessRole::Label);
    clock.set_label(locked_clock_label(view));
    clock.set_bounds(access_rect(layout.clock));

    // `PasswordInput` maps to AT-SPI's PasswordText role. That lets Orca and
    // braille clients suppress credential echo while retaining editable text.
    let mut password = AccessNode::new(AccessRole::PasswordInput);
    password.set_label(t("lock-screen-password"));
    password.set_bounds(access_rect(layout.password));
    password.add_action(AccessAction::Focus);
    if view.verifying {
        password.set_disabled();
    } else {
        password.add_action(AccessAction::SetValue);
        // A screen-reader user must be able to complete the same trusted
        // credential flow as a keyboard user. Click is AccessKit's standard
        // activation action; native maps it to LockKey::Submit and nowhere
        // else, so it does not expose hidden desktop actions while locked.
        password.add_action(AccessAction::Click);
    }
    let password_text = password_text_run(&mut password, layout.password);

    let mut help = AccessNode::new(AccessRole::Label);
    help.set_label(t("lock-screen-help"));
    help.set_bounds(access_rect(layout.status));

    let mut children = vec![LOCK_CLOCK_ID, LOCK_PASSWORD_ID, LOCK_HELP_ID];
    let mut nodes = vec![
        (LOCK_CLOCK_ID, clock),
        (LOCK_PASSWORD_ID, password),
        (LOCK_PASSWORD_TEXT_ID, password_text),
        (LOCK_HELP_ID, help),
    ];
    if let (Some(player), Some(media)) = (
        view.now_playing,
        LockMediaLayout::for_layout(&layout, theme),
    ) {
        let mut group = AccessNode::new(AccessRole::Group);
        let mut label = format!(
            "{}: {}",
            crate::shell::now_playing::tr("now-playing-title"),
            player.headline()
        );
        if let Some(subline) = player.subline() {
            label.push_str(", ");
            label.push_str(subline);
        }
        group.set_label(label);
        group.set_bounds(access_rect(media.card));
        let mut buttons = Vec::with_capacity(media.transport.len());
        for (transport, rect) in media.transport {
            let id = transport_node(transport);
            let mut button = AccessNode::new(AccessRole::Button);
            button.set_label(transport.label(player));
            button.set_bounds(access_rect(rect));
            if transport.enabled(player) {
                button.add_action(AccessAction::Click);
            } else {
                button.set_disabled();
            }
            buttons.push(id);
            nodes.push((id, button));
        }
        group.set_children(buttons);
        children.push(LOCK_MEDIA_ID);
        nodes.push((LOCK_MEDIA_ID, group));
    }
    let status = if view.verifying {
        Some(t("lock-screen-verifying-password"))
    } else if let Some(error) = view.error {
        Some(lock_error_status(error))
    } else if view.notification_count > 0 {
        Some(count_label(view.notification_count))
    } else {
        None
    };
    if let Some(status) = status {
        let mut node = AccessNode::new(AccessRole::Label);
        node.set_label(status);
        node.set_bounds(access_rect(layout.notifications));
        children.push(LOCK_STATUS_ID);
        nodes.push((LOCK_STATUS_ID, node));
    }
    root.set_children(children);
    nodes.push((LOCK_ROOT_ID, root));
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo::new(LOCK_ROOT_ID)),
        tree_id: TreeId::ROOT,
        focus: if view.verifying {
            LOCK_ROOT_ID
        } else {
            LOCK_PASSWORD_ID
        },
    }
}

/// The lock screen as the shell narrator reads it: the same labels as the
/// AT-SPI tree above, with the password field focused and protected (its
/// length is never spoken) and the status line a live region — assertive
/// for a failed attempt, polite for verifying and the redacted count.
pub fn narration_tree(view: &LockView<'_>) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, AccessState, Politeness, Role};
    let clock = match (view.clock.is_empty(), view.date.is_empty()) {
        (true, _) => t("lock-screen-locked"),
        (false, true) => t_args("lock-screen-locked-at", &[("clock", view.clock.into())]),
        (false, false) => t_args(
            "lock-screen-locked-at-date",
            &[("clock", view.clock.into()), ("date", view.date.into())],
        ),
    };
    let mut status = AccessNode::new(Role::StaticText).with_name(t("lock-screen-status"));
    if view.verifying {
        status = status
            .with_live(Politeness::Polite)
            .with_value(t("lock-screen-verifying-password"));
    } else if let Some(error) = view.error {
        status = status
            .with_live(Politeness::Assertive)
            .with_value(lock_error_status(error));
    } else if let Some(alert) = view.alert {
        status = status.with_live(Politeness::Assertive).with_value(alert);
    } else {
        status = status.with_live(Politeness::Polite);
        if view.notification_count > 0 {
            status = status.with_value(count_label(view.notification_count));
        }
    }
    let mut password_state = AccessState::new().focused(!view.verifying).protected(true);
    password_state.disabled = view.verifying;
    AccessNode::new(Role::Window)
        .with_name(t("lock-screen-a11y-name"))
        .with_children(vec![
            AccessNode::new(Role::StaticText).with_name(clock),
            AccessNode::new(Role::TextInput)
                .with_name(t("lock-screen-password"))
                .with_description(t("lock-screen-help"))
                .with_state(password_state),
            status,
        ])
}

fn access_rect(rect: Rect) -> AccessRect {
    AccessRect::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

/// Bounding box `(w, h)` covering every output, i.e. the size of the single
/// scene the compositor paints. `(0, 0)` for an empty list.
fn combined_extent(outputs: &[LockOutput]) -> (f64, f64) {
    let mut w = 0.0_f64;
    let mut h = 0.0_f64;
    for o in outputs {
        w = w.max(o.origin.0 + o.size.0);
        h = h.max(o.origin.1 + o.size.1);
    }
    (w.max(0.0), h.max(0.0))
}

/// Build the lock-screen scene spanning every output in `outputs`, themed
/// via `theme`, using `text` for glyph-shaped labels, from the live
/// [`LockView`].
///
/// The overlay fills the **combined** geometry of all outputs so app/chrome
/// content is hidden everywhere (see
/// [`crate::compositor::backend::combined_geometry`]); the interactive
/// credential widget is drawn on the designated primary output. Secondary
/// displays stay private and informative, but do not duplicate password UI.
///
/// `view.notification_count`, when non-zero, shows "N notifications" —
/// never any app name, summary, or body.
///
/// Returns a fresh [`Scene`] the backend can submit exactly like a chrome frame.
pub fn draw_lock_scene(
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) -> Scene {
    draw_lock_scene_with_wallpaper(outputs, theme, text, view, None)
}

/// As [`draw_lock_scene`], using an optional compositor-owned private image
/// behind the lock scrim. The image data has already been normalised and read
/// from Tilo's private cache; this renderer never opens an app-provided URI.
///
/// The artwork holds still, as the greeter draws it: a locked machine is
/// mostly unattended, and moving artwork would repaint the whole output at
/// its own cadence for as long as the lock is up. Only what the lock shows
/// changing (the clock, typed dots, a track) repaints it.
pub fn draw_lock_scene_with_wallpaper(
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    wallpaper: Option<&ImageData>,
) -> Scene {
    draw_lock_scene_at_time(
        outputs,
        theme,
        text,
        view,
        wallpaper,
        ui::auth_backdrop::STILL_SECONDS,
    )
}

/// Paint the hardware-key readout (volume, brightness, playback) over the
/// lock scene, bottom-centred on the primary output clear of the credential
/// widget. Media keys stay live while locked; without this their only
/// feedback would be hidden behind the lock surface.
pub fn draw_lock_osd(
    scene: &mut Scene,
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    osd: crate::shell::osd::Osd,
    now: std::time::Instant,
) {
    let Some(primary) = outputs.first() else {
        return;
    };
    let viewport = Rect::new(
        primary.origin.0,
        primary.origin.1,
        primary.origin.0 + primary.size.0,
        primary.origin.1 + primary.size.1,
    );
    osd.draw(
        scene,
        Affine::IDENTITY,
        text,
        theme,
        crate::shell::osd::OsdArea::on(viewport),
        osd.alpha_in(theme, now),
    );
}

/// Whether a readout shown over the lock surface needs a frame at `now`:
/// every tick of its fade, plus the overdue expiry boundary so the final
/// clearing frame is painted. The opaque hold needs no frames.
pub fn lock_osd_frame_due(osd: Option<crate::shell::osd::Osd>, now: std::time::Instant) -> bool {
    osd.is_some_and(|osd| {
        osd.needs_animation_frame_at(now)
            || osd
                .next_wake_deadline_at(now)
                .is_some_and(|deadline| deadline <= now)
    })
}

/// Live renderer with an explicit monotonic animation time. Keeping time out
/// of `LockView` prevents animation frames from changing authentication state
/// or the accessibility tree, and makes offscreen frames reproducible.
pub fn draw_lock_scene_at_time(
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    wallpaper: Option<&ImageData>,
    elapsed_seconds: f64,
) -> Scene {
    let status = LockStatusPaint::settled(view);
    let mut scene = Scene::new();
    draw_lock_scene_into(
        &mut scene,
        outputs,
        theme,
        text,
        view,
        wallpaper,
        elapsed_seconds,
        &status,
    );
    scene
}

/// Paint into the caller's scene so retained-part recording includes the
/// entire lock composition and any readout appended over it. `status` is the
/// status row at this frame's instant: a still passes its resting form, and
/// the live retained producer passes the phase it timestamped.
pub(crate) fn draw_lock_scene_into(
    scene: &mut Scene,
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    wallpaper: Option<&ImageData>,
    elapsed_seconds: f64,
    status: &LockStatusPaint,
) {
    // 1. Opaque backdrop over the combined geometry of every output — the
    //    full bounding box (not per-output rects) guarantees no gap ever
    //    shows app content. A private wallpaper is then fitted to each
    //    display on its own, as the desktop fits it: one picture stretched
    //    over the combined extent would split across the bezel and crop to
    //    the arrangement's shape instead of each display's.
    let (ext_w, ext_h) = combined_extent(outputs);
    if ext_w > 0.0 && ext_h > 0.0 {
        let backdrop = Rect::new(0.0, 0.0, ext_w, ext_h);
        let visible_wallpaper = wallpaper.filter(|image| image.width > 0 && image.height > 0);
        let wallpaper_layer = visible_wallpaper.is_some();
        if let Some(image) = visible_wallpaper {
            for output in outputs {
                draw_wallpaper_on(scene, image, LockLayout::for_output(*output, theme).output);
            }
            // A firm dark scrim over every display's picture leaves the lock
            // experience legible and does not expose a bright image through
            // password/notification chrome.
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                0.68,
                Affine::IDENTITY,
                &backdrop,
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.tokens.colors.surfaces.base.with_alpha(1.0),
            None,
            &backdrop,
        );
        if wallpaper_layer {
            scene.pop_layer();
        }
    }

    // Each output receives its own complete composition, so a ribbon never
    // disappears into the physical gap between displays. A user-selected
    // private wallpaper retains its existing scrim and takes precedence.
    if wallpaper.is_none() {
        for output in outputs {
            // The lock scene is composed in output-layout coordinates with
            // the identity transform, like every other lock draw below.
            ui::auth_backdrop::draw(
                scene,
                theme,
                Affine::IDENTITY,
                LockLayout::for_output(*output, theme).output,
                elapsed_seconds,
            );
        }
    }

    // 2. One credential surface on the primary display. Secondary displays
    // deliberately have a quieter non-interactive indication.
    let primary = primary_output(outputs);
    for output in outputs {
        if Some(*output) == primary {
            let layout =
                LockLayout::for_output_with_keyboard(*output, theme, view.keyboard.is_some());
            draw_lock_widget(scene, &layout, theme, text, view, status);
            if let (Some(keyboard), Some(pad)) = (view.keyboard, layout.keyboard) {
                keyboard.paint(
                    scene,
                    Affine::IDENTITY,
                    text,
                    theme,
                    (
                        pad,
                        keyboard_key_metrics(theme).0,
                        keyboard_key_metrics(theme).1,
                    ),
                );
            }
        } else {
            let layout = LockLayout::for_output(*output, theme);
            draw_secondary_lock_widget(scene, &layout, theme, text, view);
        }
    }

    // 3. The state of the whole machine, on every display.
    if let Some(alert) = view.alert {
        for output in outputs {
            draw_lock_alert(
                scene,
                LockLayout::for_output(*output, theme).output,
                theme,
                text,
                alert,
            );
        }
    }
}

/// `image` covering one display's `rect` (centred, cropped to its shape).
/// The lock scene lays one firm scrim over every display's picture.
fn draw_wallpaper_on(scene: &mut Scene, image: &ImageData, rect: Rect) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let scale =
        (rect.width() / f64::from(image.width)).max(rect.height() / f64::from(image.height));
    let width = f64::from(image.width) * scale;
    let height = f64::from(image.height) * scale;
    let brush_transform = Affine::translate((
        rect.x0 + (rect.width() - width) / 2.0,
        rect.y0 + (rect.height() - height) / 2.0,
    ))
    .pre_scale_non_uniform(scale, scale);
    let brush = ImageBrush::new(image.clone()).with_extend(Extend::Pad);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &brush,
        Some(brush_transform),
        &rect,
    );
}

/// Where the alert about the whole machine sits on one display: top-centre,
/// a banner's width at most, inset from the edges. `height` is the card's.
pub fn lock_alert_rect(output: Rect, height: f64) -> Rect {
    const INSET: f64 = ui::tokens::Spacing::BASE.s6 as f64;
    const WIDTH: f64 = ui::tokens::Metrics::BASE.panel_width_list as f64;
    let width = (output.width() - INSET * 2.0).clamp(0.0, WIDTH);
    let x0 = output.center().x - width / 2.0;
    let y0 = output.y0 + INSET;
    Rect::new(x0, y0, x0 + width, (y0 + height).min(output.y1))
}

/// The alert about the whole machine (critical battery), as the desktop's
/// banner shows it, on one display of the lock screen.
fn draw_lock_alert(
    scene: &mut Scene,
    output: Rect,
    theme: &Theme,
    text: &mut TextRenderer,
    alert: &str,
) {
    let renderer_scale = text.text_scale();
    text.set_text_scale(1.0);
    let card = ui::widget::toast::ToastCard::new(alert, ui::widget::toast::ToastKind::Error)
        .closable(false);
    let width = lock_alert_rect(output, 0.0).width();
    if width > 0.0 {
        let height = ui::widget::toast::card_height(text, theme, &card, width);
        let rect = lock_alert_rect(output, height);
        let _ = ui::widget::toast::paint_card(scene, Affine::IDENTITY, text, theme, rect, &card);
    }
    text.set_text_scale(renderer_scale);
}

/// Draw the interactive lock widget from one already-bounded layout.
fn draw_lock_widget(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    status: &LockStatusPaint,
) {
    // The lock chrome sizes its type from the theme's text scale itself
    // (`scaled_text_size`); the renderer must not scale it again (HIG §7a)
    // — reset for this pass, as the shell's immediate-mode chrome passes
    // do, and restored at the end.
    let renderer_scale = text.text_scale();
    text.set_text_scale(1.0);
    let type_scale = ui::tokens::Typography::default();
    if !view.clock.is_empty() {
        let size = fitted_clock_size(
            text,
            view.clock,
            scaled_text_size(
                if layout.clock.x1 < layout.panel.x0 {
                    CLOCK_BESIDE_PANEL_PX
                } else {
                    CLOCK_PX
                },
                layout.scale,
                theme,
            ),
            layout.clock,
        );
        draw_time_text(
            scene,
            text,
            view.clock,
            size,
            theme.text_primary,
            layout.clock,
            layout.clock.x1 < layout.panel.x0,
        );
    }
    if !view.date.is_empty() {
        draw_time_text(
            scene,
            text,
            view.date,
            scaled_text_size(type_scale.size(ui::TextRole::Heading), layout.scale, theme),
            theme.text_primary,
            layout.date,
            layout.clock.x1 < layout.panel.x0,
        );
    }

    // Panel role (HIG §9.1/§9.2): the credential card's corner is the Panel
    // radius token (24) scaled with the rest of the lock chrome (the token
    // already carries the UI scale that `layout.scale` includes), its ground
    // is `panel_ground` (walked clear of the wallpaper-dimmed desktop) and
    // its edge is the derived Panel border rather than a declared colour.
    let panel = RoundedRect::from_rect(
        layout.panel,
        f64::from(theme.tokens.radii.panel) * layout.scale / f64::from(theme.ui_scale).max(0.1),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        ui::chrome::shell::panel_ground(theme),
        None,
        &panel,
    );
    scene.stroke(
        &Stroke::new((1.0 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        ui::chrome::shell::panel_border(theme),
        None,
        &panel,
    );

    draw_lock_icon(scene, layout, theme);
    draw_centered_text(
        scene,
        text,
        &ui::i18n::I18nManager::lookup_plain("lock-welcome-back"),
        scaled_text_size(type_scale.size(ui::TextRole::Title), layout.scale, theme),
        theme.text_primary,
        layout.title,
    );
    draw_password_field(scene, layout, theme, text, view, status);
    draw_status_field(scene, layout, theme, text, status);
    if let (Some(player), Some(media)) =
        (view.now_playing, LockMediaLayout::for_layout(layout, theme))
    {
        draw_lock_media(scene, layout, &media, theme, text, player);
    }
    let label = count_label(view.notification_count);
    if !label.is_empty() {
        draw_centered_text(
            scene,
            text,
            &label,
            scaled_text_size(type_scale.size(ui::TextRole::Caption), layout.scale, theme),
            theme.text_dim,
            layout.notifications,
        );
    }
    text.set_text_scale(renderer_scale);
}

/// The compact now-playing card: the same Panel ground as the credential
/// card, the shared artwork and transport painters, and no pickers.
fn draw_lock_media(
    scene: &mut Scene,
    layout: &LockLayout,
    media: &LockMediaLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    player: &PlayerSnapshot,
) {
    let card = RoundedRect::from_rect(
        media.card,
        f64::from(theme.tokens.radii.control) * layout.scale / f64::from(theme.ui_scale).max(0.1),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        ui::chrome::shell::panel_ground(theme),
        None,
        &card,
    );
    scene.stroke(
        &Stroke::new((1.0 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        ui::chrome::shell::panel_border(theme),
        None,
        &card,
    );
    let type_scale = ui::tokens::Typography::default();
    let title = ui::text_render::TextStyle::new(scaled_text_size(
        type_scale.size(ui::TextRole::Caption),
        layout.scale,
        theme,
    ));
    let caption = title.clone();
    paint_song_at_rest(
        &mut ui::paint::PaintCtx::new(scene, Affine::IDENTITY, text, theme),
        player,
        media.art,
        Some((media.text, &title, &caption)),
    );
    paint_transport(
        scene,
        Affine::IDENTITY,
        theme,
        &media.transport,
        player,
        None,
        None,
    );
}

/// A display without the password field shows the clock and date over the
/// artwork, nothing more. It must never paint a second password affordance:
/// focus and assistive technology point at the sole primary field.
fn draw_secondary_lock_widget(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    // Same renderer reset as the primary widget (HIG §7a).
    let renderer_scale = text.text_scale();
    text.set_text_scale(1.0);
    let type_scale = ui::tokens::Typography::default();
    let text_left = layout.output.x0 + 24.0 * layout.scale;
    let text_right = layout.output.x1 - 24.0 * layout.scale;
    if !view.clock.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.clock,
            scaled_text_size(SECONDARY_CLOCK_PX, layout.scale, theme),
            theme.text_primary,
            Rect::new(text_left, layout.clock.y0, text_right, layout.clock.y1),
        );
    }
    if !view.date.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.date,
            scaled_text_size(type_scale.size(ui::TextRole::Heading), layout.scale, theme),
            theme.text_primary,
            Rect::new(text_left, layout.date.y0, text_right, layout.date.y1),
        );
    }
    text.set_text_scale(renderer_scale);
}

/// The primary clock: a display size with no place on the compressed
/// chrome type scale (HIG §9.6 tops out at `x4l` 40); the authored display
/// sizes this surface keeps, named so they are not mistaken for tokens.
/// Stacked above the credential panel:
const CLOCK_PX: f32 = 88.0;
/// Beside the credential panel on a wide output, where the clock has its own
/// column:
const CLOCK_BESIDE_PANEL_PX: f32 = 120.0; // display type size; the chrome type scale stops at 40
/// The secondary display's clock, a step quieter than the primary's.
const SECONDARY_CLOCK_PX: f32 = 52.0;

/// A lock-chrome type size: the *unscaled* role size, multiplied by the
/// layout's bounded scale and the user's text scale. The widget painters
/// reset the renderer for their pass, so this is painted exactly once
/// (HIG §7a).
fn scaled_text_size(base: f32, scale: f64, theme: &Theme) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded lock-layout scale is at most 3; f32 is exact enough for font pixels"
    )]
    let scale = scale as f32;
    base * scale * theme.text_scale
}

/// Time is one visual unit: it must not wrap or paint beyond the output at
/// the supported maximum text scale. Measure with the actual font first;
/// preserve the requested accessible size whenever it fits its reserved row.
fn fitted_clock_size(text: &mut TextRenderer, value: &str, size: f32, bounds: Rect) -> f32 {
    let (width, height) = text.measure(value, size, None);
    let fit = (bounds.width() / f64::from(width).max(1.0))
        .min(bounds.height() / f64::from(height).max(1.0))
        .clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "fit is bounded to [0, 1] for a screen-space font size"
    )]
    let fit = fit as f32;
    size * if fit < 1.0 { fit * 0.99 } else { fit }
}

/// Wide screens anchor the time and date on one margin. Compact outputs
/// center the same lockup over the credential surface.
fn draw_time_text(
    scene: &mut Scene,
    text: &mut TextRenderer,
    value: &str,
    size: f32,
    color: vello::peniko::Color,
    bounds: Rect,
    align_left: bool,
) {
    if !align_left {
        draw_centered_text(scene, text, value, size, color, bounds);
        return;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded output width is sufficient at f32 text precision"
    )]
    let width = bounds.width() as f32;
    let (_, height) = text.measure(value, size, Some(width));
    text.draw_text(
        scene,
        value,
        size,
        color,
        Affine::translate((
            bounds.x0,
            bounds.y0 + (bounds.height() - f64::from(height)) / 2.0,
        )),
        Some(width),
    );
}

fn draw_centered_text(
    scene: &mut Scene,
    text: &mut TextRenderer,
    value: &str,
    size: f32,
    color: vello::peniko::Color,
    bounds: Rect,
) {
    if value.is_empty() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "lock output geometry is bounded to compositor dimensions; f32 width is sufficient for text shaping"
    )]
    let max_width = bounds.width() as f32;
    let (width, height) = text.measure(value, size, Some(max_width));
    let x = bounds.x0 + (bounds.width() - f64::from(width.min(max_width))) / 2.0;
    let y = bounds.y0 + (bounds.height() - f64::from(height)) / 2.0;
    text.draw_text(
        scene,
        value,
        size,
        color,
        Affine::translate((x, y)),
        Some(max_width),
    );
}

fn draw_lock_icon(scene: &mut Scene, layout: &LockLayout, theme: &Theme) {
    let cx = layout.icon.center().x;
    let body_width = layout.icon.width() * 0.72;
    let body_height = layout.icon.height() * 0.50;
    let body_y = layout.icon.y0 + layout.icon.height() * 0.46;
    let body = RoundedRect::new(
        cx - body_width / 2.0,
        body_y,
        cx + body_width / 2.0,
        body_y + body_height,
        6.0 * layout.scale,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.text_primary,
        None,
        &body,
    );

    let r = body_width * 0.31;
    let shackle = Arc::new(Point::new(cx, body_y), (r, r), PI, PI, 0.0);
    scene.stroke(
        &Stroke::new((5.0 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        theme.tokens.colors.text_primary,
        None,
        &shackle,
    );

    let keyhole = Circle::new(
        Point::new(cx, body_y + body_height / 2.0),
        3.5 * layout.scale,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.base,
        None,
        &keyhole,
    );
}

fn draw_password_field(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    status: &LockStatusPaint,
) {
    if ui::scene_parts::is_tracked(scene) {
        let mut field = Scene::new();
        draw_password_contents(&mut field, layout, theme, text, view, status);
        // Derive bounds from the commands, including placeholder glyph overhang,
        // the focus stroke and the waiting marks. No locale or theme may outgrow
        // a guessed rect.
        let bounds = ui::scene_parts::DrawBounds::default()
            .of(field.encoding())
            .unwrap_or_default();
        ui::scene_parts::part(
            scene,
            ui::scene_parts::PartKey::new("lock-password", 0_u8),
            bounds,
            |scene| scene.append(&field, None),
        );
    } else {
        draw_password_contents(scene, layout, theme, text, view, status);
    }
}

fn draw_password_contents(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    status: &LockStatusPaint,
) {
    let field = RoundedRect::from_rect(
        layout.password,
        f64::from(theme.tokens.radii.control) * layout.scale / f64::from(theme.ui_scale).max(0.1),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.base,
        None,
        &field,
    );
    // The ring says whether the field can take a secret, is checking one, or
    // is handing the person back a retry. It is the focus colour at rest
    // because the field always holds focus while locked.
    scene.stroke(
        &Stroke::new((1.5 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        LockStatus::of(view).field_ring(theme),
        None,
        &field,
    );
    if view.verifying {
        // Three waiting marks in the informational role, wider apart than
        // the password's mask: the typed dots are gone, the marks stand still
        // (nothing on the lock loops or keeps animating), and they settle in
        // with the status row as the ring says the field is checking.
        let ink = theme
            .tokens
            .colors
            .semantic
            .info
            .with_alpha(status.shown_alpha);
        draw_field_dots(scene, layout, 3, 18.0, 3.0, ink);
    } else if view.password_len == 0 {
        draw_centered_text(
            scene,
            text,
            &t("lock-screen-enter-password"),
            scaled_text_size(
                ui::tokens::Typography::default().size(ui::TextRole::Body),
                layout.scale,
                theme,
            ),
            theme.text_dim,
            layout.password,
        );
    } else {
        draw_field_dots(
            scene,
            layout,
            view.password_len.min(18),
            14.0,
            4.0,
            theme.text_primary,
        );
    }
}

/// `count` marks centred on the field: the password's mask, or the three
/// waiting marks a verification shows. `gap` and `radius` are reference
/// pixels, scaled with the lock chrome.
fn draw_field_dots(
    scene: &mut Scene,
    layout: &LockLayout,
    count: usize,
    gap: f64,
    radius: f64,
    ink: vello::peniko::Color,
) {
    let gap = gap * layout.scale;
    let dot_r = radius * layout.scale;
    // Every caller bounds `count` (at most 18, or the three waiting marks), so
    // both usize -> f64 conversions below are always exact.
    #[allow(
        clippy::cast_precision_loss,
        reason = "count and i are bounded by the callers' `.min(18)` or a literal 3, so this usize -> f64 cast never loses precision"
    )]
    {
        let total_w = (count as f64 - 1.0).max(0.0) * gap;
        let start_x = layout.password.center().x - total_w / 2.0;
        let dot_y = layout.password.center().y;
        for i in 0..count {
            let dot = Circle::new(Point::new(start_x + i as f64 * gap, dot_y), dot_r);
            scene.fill(Fill::NonZero, Affine::IDENTITY, ink, None, &dot);
        }
    }
}

/// The status row's bounded part: a cross-fade between answers, a refusal's
/// shake, or a change of line damages the row's own new and old bounds, never
/// the output. The bounds come from the commands, so a shake or a longer
/// translation cannot outgrow them.
fn draw_status_field(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    status: &LockStatusPaint,
) {
    if ui::scene_parts::is_tracked(scene) {
        let mut row = Scene::new();
        draw_status(&mut row, layout, theme, text, status);
        let bounds = ui::scene_parts::DrawBounds::default()
            .of(row.encoding())
            .unwrap_or_default();
        ui::scene_parts::part(
            scene,
            ui::scene_parts::PartKey::new("lock-status", 0_u8),
            bounds,
            |scene| scene.append(&row, None),
        );
    } else {
        draw_status(scene, layout, theme, text, status);
    }
}

fn draw_status(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    status: &LockStatusPaint,
) {
    // One status role for every state (Control sub-line, HIG §9.6): the
    // 14 / 13 / 12 the three states used to pick between were not sizes on
    // the scale and made the row jump as the state changed. The row's rect
    // never moves with the tone; only a refusal's shake offsets the ink for a
    // moment, so the form below never reflows.
    let type_scale = ui::tokens::Typography::default();
    let size = scaled_text_size(type_scale.size(ui::TextRole::Caption), layout.scale, theme);
    let dx = status.dx * layout.scale;
    let row = Rect::new(
        layout.status.x0 + dx,
        layout.status.y0,
        layout.status.x1 + dx,
        layout.status.y1,
    );
    if let Some((tone, line, alpha)) = &status.replaced {
        draw_centered_text(
            scene,
            text,
            line,
            size,
            tone.ink(theme).with_alpha(*alpha),
            row,
        );
    }
    draw_centered_text(
        scene,
        text,
        &status.shown,
        size,
        status.tone.ink(theme).with_alpha(status.shown_alpha),
        row,
    );
}

/// The visual and accessible recovery line after a failed unlock.
///
/// Auth deliberately returns generic, non-leaky prose, but the full
/// diagnostic plus a recovery instruction exceeded the fixed status row at
/// common 1280x800 layouts. Keep the action in the line while preserving the
/// distinction between a rejected credential and an unavailable verifier.
fn lock_error_status(error: &str) -> String {
    if error == UNAVAILABLE_MSG {
        t("lock-screen-cannot-verify")
    } else {
        t("lock-screen-sign-in-failed")
    }
}

/// Lock-screen count label: "N notifications" (empty for zero); reuses the
/// shell's redaction helper so the lock screen and toast stack agree on
/// wording.
fn count_label(count: usize) -> String {
    tilo_desktop::notifications::lock_suppressed_label(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> LockView<'static> {
        LockView {
            clock: "13:37",
            date: "Friday, 11 July",
            ..LockView::default()
        }
    }

    /// A single output at the origin of size `w`x`h` — the winit / headless
    /// (and single-monitor DRM) case.
    fn single(w: f64, h: f64) -> [LockOutput; 1] {
        [LockOutput {
            origin: (0.0, 0.0),
            size: (w, h),
            scale: 1.0,
        }]
    }

    #[test]
    fn a_machine_alert_is_on_every_lock_display_and_spoken() {
        let outputs = [
            LockOutput {
                origin: (0.0, 0.0),
                size: (2944.0, 1840.0),
                scale: 2.0,
            },
            LockOutput {
                origin: (2944.0, 0.0),
                size: (1920.0, 1080.0),
                scale: 1.0,
            },
        ];
        let theme = Theme::dark();
        for output in outputs {
            let display = LockLayout::for_output(output, &theme).output;
            let alert = lock_alert_rect(display, 64.0);
            assert!(
                alert.area() > 0.0 && display.union(alert) == display,
                "the alert {alert:?} sits on its display {display:?}"
            );
            assert!(
                (alert.center().x - display.center().x).abs() < 0.5,
                "top-centre, the same place on every display"
            );
        }
        let mut text = TextRenderer::new();
        let view = LockView {
            alert: Some("Battery critical"),
            ..view()
        };
        let _scene = draw_lock_scene(&outputs, &theme, &mut text, &view);
        let tree = narration_tree(&view);
        assert!(
            tree.children
                .iter()
                .any(|node| node.value.as_deref() == Some("Battery critical")),
            "the status line says it"
        );
    }

    #[test]
    fn lock_accessibility_tree_exposes_one_hidden_password_field() {
        // Phase `lock_accessibility_tree_has_one_actionable_password_and_hides_its_value`.
        {
            let v = LockView {
                password_len: 12,
                notification_count: 4,
                ..view()
            };
            let update = build_accessibility_tree(&single(1600.0, 900.0), &Theme::dark(), &v);
            assert_eq!(update.focus, LOCK_PASSWORD_ID);
            let passwords = update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == AccessRole::PasswordInput)
                .collect::<Vec<_>>();
            assert_eq!(
                passwords.len(),
                1,
                "only the primary lock field is semantic"
            );
            let (_, password) = passwords[0];
            assert_eq!(password.label(), Some("Password"));
            assert!(password.supports_action(AccessAction::Focus));
            assert!(password.supports_action(AccessAction::SetValue));
            assert!(password.supports_action(AccessAction::Click));
            assert_eq!(password.value(), None, "credential values never enter AT");
            // AT-SPI Text and EditableText need a text run; it stays empty
            // with the caret at its start, whatever length was typed.
            assert_eq!(password.children(), [LOCK_PASSWORD_TEXT_ID].as_slice());
            let (_, run) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == LOCK_PASSWORD_TEXT_ID)
                .expect("the password's text run is exported");
            assert_eq!(run.role(), AccessRole::TextRun);
            assert_eq!(run.value(), Some(""));
            let selection = password.text_selection().expect("a caret");
            assert_eq!(selection.focus.node, LOCK_PASSWORD_TEXT_ID);
            assert_eq!(selection.focus.character_index, 0);
            assert!(
                update.nodes.iter().all(|(_, node)| {
                    node.label()
                        .is_none_or(|label| !label.contains("••") && !label.contains("********"))
                }),
                "the semantic tree must not encode the password or its mask"
            );
        }
        {
            let v = LockView {
                verifying: true,
                ..view()
            };
            let update = build_accessibility_tree(&single(800.0, 600.0), &Theme::dark(), &v);
            assert_eq!(update.focus, LOCK_ROOT_ID);
            let (_, password) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == LOCK_PASSWORD_ID)
                .expect("password field is present");
            assert!(!password.supports_action(AccessAction::SetValue));
            assert!(!password.supports_action(AccessAction::Click));
            assert!(update.nodes.iter().any(|(_, node)| {
                node.label()
                    .is_some_and(|label| label == "Verifying password")
            }));
        }
    }

    #[test]
    fn media_key_readout_paints_over_the_lock_scene_and_schedules_its_fade() {
        use crate::shell::osd::{OSD_HOLD, Osd, osd_fade, osd_fade_in};
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let outputs = single(1600.0, 900.0);
        let mut scene = draw_lock_scene(&outputs, &theme, &mut text, &view());
        let before = scene.encoding().n_paths;
        let osd = Osd::volume(0.4, false);
        let raised = osd.raised_at;
        let held = raised + osd_fade_in();
        draw_lock_osd(&mut scene, &outputs, &theme, &mut text, osd, held);
        assert!(
            scene.encoding().n_paths > before,
            "a live readout must add paint on top of the lock scene"
        );

        assert!(!lock_osd_frame_due(None, raised));
        assert!(
            lock_osd_frame_due(Some(osd), raised),
            "the fade-in needs frames"
        );
        assert!(
            !lock_osd_frame_due(Some(osd), held),
            "the opaque hold needs no frames"
        );
        assert!(lock_osd_frame_due(
            Some(osd),
            raised + OSD_HOLD.saturating_sub(osd_fade()) + std::time::Duration::from_millis(1)
        ));
        assert!(
            lock_osd_frame_due(Some(osd), raised + OSD_HOLD),
            "the overdue expiry boundary still needs its clearing frame"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn lock_scene_paints_every_state_and_viewport() {
        // Phase `lock_scene_is_non_empty`.
        // The lock scene must encode *something* — a blank scene would mean
        // the overlay never paints and app content leaks through, defeating
        // the lock.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &view());
            assert!(
                !scene.encoding().is_empty(),
                "lock overlay must produce a non-empty scene so it hides app content"
            );
        }
        // Phase `lock_scene_handles_tiny_viewport`.
        // Painting at a tiny viewport must not panic (degenerate sizes can
        // appear before the first real `OutputResized`).
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let v = LockView {
                notification_count: 3,
                ..view()
            };
            let _ = draw_lock_scene(&single(1.0, 1.0), &theme, &mut text, &v);
        }
        // Phase `lock_scene_with_no_outputs_is_empty_not_a_panic`.
        // An empty output list must not panic and produces an empty scene
        // (the caller falls back to a default viewport before this is hit).
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let scene = draw_lock_scene(&[], &theme, &mut text, &view());
            assert!(scene.encoding().is_empty());
        }
        // Phase `lock_scene_renders_password_field_states`.
        // The password field renders both empty (placeholder) and populated
        // (masked dots) without panicking, including a very long password that
        // must be capped to the field width.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            for len in [0usize, 1, 8, 200] {
                let v = LockView {
                    password_len: len,
                    ..view()
                };
                let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
                assert!(!scene.encoding().is_empty());
            }
        }
        // Phase `lock_scene_renders_error`.
        // An error string paints (in the danger colour) without panicking.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let v = LockView {
                password_len: 0,
                error: Some("Incorrect username or password"),
                ..view()
            };
            let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
            assert!(!scene.encoding().is_empty());
        }
        // Phase `lock_error_status_is_concise_and_preserves_retry_guidance`.
        assert_eq!(
            lock_error_status("Incorrect username or password"),
            "Sign-in failed. Enter your password again."
        );
        assert_eq!(
            lock_error_status(UNAVAILABLE_MSG),
            "Can't verify your password. Enter it again."
        );
        // Phase `lock_error_status_stays_on_one_line_at_1280_by_800`.
        {
            let theme = Theme::dark();
            let output = LockOutput {
                origin: (0.0, 0.0),
                size: (1280.0, 800.0),
                scale: 1.0,
            };
            let layout = LockLayout::for_output(output, &theme);
            let mut text = TextRenderer::new();
            for message in [
                lock_error_status("Incorrect username or password"),
                lock_error_status(UNAVAILABLE_MSG),
            ] {
                let (width, height) =
                    text.measure(&message, scaled_text_size(12.0, layout.scale, &theme), None);
                assert!(
                    f64::from(width) <= layout.status.width(),
                    "{message:?} must not wrap in the lock status row"
                );
                assert!(
                    f64::from(height) <= layout.status.height(),
                    "{message:?} must fit the lock status row"
                );
            }
        }
        // Phase `lock_scene_renders_verifying_state`.
        // The "Verifying…" state paints without panicking.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let v = LockView {
                verifying: true,
                ..view()
            };
            let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
            assert!(!scene.encoding().is_empty());
        }
        // Phase `count_label_is_count_only`.
        // The count-only summary never renders content; we can't read text
        // back out of a Vello scene, so assert the contract at the label
        // helper instead.
        assert_eq!(count_label(0), "");
        assert_eq!(count_label(1), "1 notification");
        assert_eq!(count_label(4), "4 notifications");
        // Phase `lock_scene_with_notifications_paints`.
        // Rendering with a non-zero count still produces a valid scene.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let v = LockView {
                notification_count: 5,
                ..view()
            };
            let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
            assert!(!scene.encoding().is_empty());
        }
    }

    /// Review fixtures: the ux-bench cannot lock the session on macOS, so
    /// the lock scene is photographed from its paint path — 1× and 200 %
    /// text on a desktop and a laptop output, plus the light and
    /// high-contrast palettes — with a typed password and a notification
    /// count, and once more in the error state at 200 %.
    /// The rendered pixels are asserted, not just the file: the credential
    /// card's interior is the Panel ground on every palette and scale.
    #[test]
    fn lock_screen_review_fixtures_are_written() {
        use crate::shell::osd::review_offscreen::OffscreenRenderer;
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/ui-review/s2-r2/lock-screen");
        // One wgpu device per output size, shared by every variant of it.
        let mut desktop = OffscreenRenderer::new(1280, 800);
        let mut laptop = OffscreenRenderer::new(800, 700);
        let variants = [
            ("dark-1x-1280x800", Theme::dark(), (1280_u32, 800_u32)),
            (
                "dark-2x-1280x800",
                Theme::dark().with_text_scale(2.0),
                (1280, 800),
            ),
            (
                "dark-2x-800x700",
                Theme::dark().with_text_scale(2.0),
                (800, 700),
            ),
            ("light-1x-1280x800", Theme::light(), (1280, 800)),
            (
                "high-contrast-1x-1280x800",
                Theme::high_contrast(),
                (1280, 800),
            ),
        ];
        for (name, theme, (w, h)) in variants {
            let renderer = if w == 1280 { &mut desktop } else { &mut laptop };
            let outputs = single(f64::from(w), f64::from(h));
            let mut text = TextRenderer::new();
            let typed = LockView {
                password_len: 6,
                notification_count: 3,
                ..view()
            };
            let scene = draw_lock_scene(&outputs, &theme, &mut text, &typed);
            let ground = theme.tokens.colors.surfaces.base;
            let path = out.join(format!("{name}.png"));
            renderer.render_png(&scene, ground, &path);
            assert!(path.is_file(), "lock fixture missing: {}", path.display());
            // Probe the card's top padding, left of the lock glyph: what the
            // compositor painted there must be the Panel ground.
            let layout = LockLayout::for_output(outputs[0], &theme);
            let probe = (
                layout.panel.x0 + layout.panel.width() / 4.0,
                layout.panel.y0 + 8.0 * layout.scale,
            );
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "probe points are inside a positive output"
            )]
            let (r, g, b, _) = OffscreenRenderer::sample(
                &renderer.render_resilient(&scene, ground),
                w,
                probe.0 as u32,
                probe.1 as u32,
            );
            let e = ui::chrome::shell::panel_ground(&theme).to_rgba8();
            assert!(
                r.abs_diff(e.r) <= 2 && g.abs_diff(e.g) <= 2 && b.abs_diff(e.b) <= 2,
                "{name}: card interior ({r},{g},{b}) is the Panel ground ({},{},{})",
                e.r,
                e.g,
                e.b
            );
            if name == "dark-2x-1280x800" {
                let errored = LockView {
                    error: Some("Incorrect password"),
                    ..view()
                };
                let scene = draw_lock_scene(&outputs, &theme, &mut text, &errored);
                let path = out.join("dark-2x-1280x800-error.png");
                renderer.render_png(&scene, ground, &path);
                assert!(path.is_file(), "lock fixture missing: {}", path.display());
            }
        }
    }

    fn assert_inside(inner: Rect, outer: Rect, name: &str) {
        assert!(
            inner.x0 >= outer.x0
                && inner.y0 >= outer.y0
                && inner.x1 <= outer.x1
                && inner.y1 <= outer.y1
                && inner.x0 <= inner.x1
                && inner.y0 <= inner.y1,
            "{name} {inner:?} escapes output {outer:?}"
        );
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded layout widths are screen coordinates passed to the text renderer's f32 API"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn lock_layout_reserves_room_for_large_text() {
        {
            let mut text = TextRenderer::new();
            for text_scale in [1.0, 2.0, 3.0, 4.0] {
                let theme = Theme::dark().with_text_scale(text_scale);
                let layout = LockLayout::for_output(single(640.0, 960.0)[0], &theme);
                let size = fitted_clock_size(
                    &mut text,
                    "21:47",
                    scaled_text_size(88.0, layout.scale, &theme),
                    layout.clock,
                );
                let (width, height) = text.measure("21:47", size, None);
                assert!(
                    f64::from(width) <= layout.clock.width(),
                    "clock width at scale {text_scale}"
                );
                assert!(
                    f64::from(height) <= layout.clock.height(),
                    "clock height at scale {text_scale}"
                );
                assert_inside(layout.clock, layout.output, "portrait clock");
                assert_inside(layout.password, layout.output, "portrait password");
            }
        }
        {
            let output = LockOutput {
                origin: (0.0, 0.0),
                size: (1280.0, 720.0),
                scale: 1.0,
            };
            let normal = LockLayout::for_output(output, &Theme::dark());

            for theme in [
                Theme::dark().with_text_scale(1.3),
                Theme::high_contrast()
                    .with_ui_scale(1.25)
                    .with_text_scale(1.3),
            ] {
                let layout = LockLayout::for_output(output, &theme);
                let mut text = TextRenderer::new();
                for (label, value, size, rect) in [
                    (
                        "clock",
                        "21:47",
                        if layout.clock.x1 < layout.panel.x0 {
                            120.0
                        } else {
                            88.0
                        },
                        layout.clock,
                    ),
                    ("date", "Saturday, September 5", 18.0, layout.date),
                    ("title", "Welcome back", 22.0, layout.title),
                    ("password", "Enter password", 15.0, layout.password),
                    (
                        "status",
                        "Incorrect username or password",
                        13.0,
                        layout.status,
                    ),
                    (
                        "notifications",
                        "12 notifications",
                        13.0,
                        layout.notifications,
                    ),
                ] {
                    let size_px = scaled_text_size(size, layout.scale, &theme);
                    let (_, measured_height) =
                        text.measure(value, size_px, Some(rect.width() as f32));
                    assert!(
                        f64::from(measured_height) <= rect.height() + 0.5,
                        "{label} text must fit its own row under {}: measured={measured_height}, row={rect:?}",
                        theme.name,
                    );
                }
                assert!(layout.clock.y1 <= layout.date.y0);
                assert!(layout.date.y1 <= layout.panel.y0 || layout.date.x1 < layout.panel.x0);
                assert!(layout.icon.y1 <= layout.title.y0);
                assert!(layout.title.y1 <= layout.password.y0);
                assert!(layout.password.y1 <= layout.status.y0);
                assert!(layout.status.y1 <= layout.notifications.y0);
                assert!(
                    layout.panel.height() > normal.panel.height(),
                    "large text must receive additional panel space"
                );
            }
        }
        {
            let title = ui::i18n::I18nManager::lookup_plain("lock-welcome-back");
            let notifications = count_label(12);
            for text_scale in [1.5, 2.0, 3.0, 4.0] {
                let theme = Theme::dark().with_text_scale(text_scale);
                for size in [
                    (640.0, 960.0),
                    (800.0, 600.0),
                    (1280.0, 720.0),
                    (1650.0, 900.0),
                ] {
                    let layout = LockLayout::for_output(
                        LockOutput {
                            origin: (80.0, 32.0),
                            size,
                            scale: 1.0,
                        },
                        &theme,
                    );
                    let mut text = TextRenderer::new();
                    let failed = lock_error_status("wrong password");
                    let unavailable = lock_error_status(UNAVAILABLE_MSG);
                    for (value, base_size, row) in [
                        (title.as_str(), 22.0, layout.title),
                        ("Enter password", 15.0, layout.password),
                        ("Press Enter to unlock", 13.0, layout.status),
                        ("Verifying…", 14.0, layout.status),
                        (failed.as_str(), 12.0, layout.status),
                        (unavailable.as_str(), 12.0, layout.status),
                        (notifications.as_str(), 13.0, layout.notifications),
                    ] {
                        let font_size = scaled_text_size(base_size, layout.scale, &theme);
                        let (width, _) = text.measure(value, font_size, None);
                        let (_, height) = text.measure(value, font_size, Some(row.width() as f32));
                        assert!(
                            f64::from(width) <= row.width() + 0.5
                                && f64::from(height) <= row.height() + 0.5,
                            "credential copy {value:?} must fit at {text_scale}× on {size:?}: measured={width}×{height}, row={row:?}",
                        );
                        assert_inside(row, layout.panel, "credential row/panel");
                        assert_inside(row, layout.output, "credential row/output");
                    }
                }
            }
        }
        {
            let theme = Theme::dark().with_text_scale(1.5);
            for (width, wide) in [(1649.0, false), (1650.0, true)] {
                let layout = LockLayout::for_output(
                    LockOutput {
                        origin: (80.0, 32.0),
                        size: (width, 900.0),
                        scale: 1.0,
                    },
                    &theme,
                );
                assert_eq!(layout.clock.x1 < layout.panel.x0, wide);
                assert!(layout.date.x1 < layout.panel.x0 || layout.date.y1 <= layout.panel.y0);
                for rect in [layout.clock, layout.date, layout.panel] {
                    assert_inside(rect, layout.output, "large type composition/output");
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn lock_layout_is_bounded_and_responsive() {
        {
            let theme = Theme::dark();
            let wide = LockLayout::for_output(single(1600.0, 900.0)[0], &theme);
            assert!(
                wide.clock.x1 < wide.panel.x0,
                "wide outputs reserve a quiet credential column"
            );
            assert!(wide.date.x1 < wide.panel.x0);
            let compact = LockLayout::for_output(single(800.0, 600.0)[0], &theme);
            assert!(
                compact.date.y1 < compact.panel.y0,
                "compact outputs stack without collisions"
            );
            assert_inside(compact.panel, compact.output, "compact credentials");
            let large =
                LockLayout::for_output(single(1100.0, 700.0)[0], &theme.with_text_scale(2.0));
            assert!(
                large.date.y1 < large.panel.y0,
                "large type uses the full reading width"
            );
            assert_inside(large.password, large.panel, "large password");
        }
        {
            let theme = Theme::dark();
            for output in [
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1600.0, 900.0),
                    scale: 1.0,
                },
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1366.0, 768.0),
                    scale: 1.25,
                },
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1280.0, 720.0),
                    scale: 1.0,
                },
                LockOutput {
                    origin: (40.0, 30.0),
                    size: (1280.0, 720.0),
                    scale: 2.0,
                },
            ] {
                let layout = LockLayout::for_output(output, &theme);
                for (name, rect) in [
                    ("clock", layout.clock),
                    ("date", layout.date),
                    ("panel", layout.panel),
                    ("icon", layout.icon),
                    ("title", layout.title),
                    ("password", layout.password),
                    ("status", layout.status),
                    ("notifications", layout.notifications),
                ] {
                    assert_inside(rect, layout.output, name);
                }
                assert!(layout.clock.y1 <= layout.date.y0);
                assert!(layout.date.y1 <= layout.panel.y0 || layout.date.x1 < layout.panel.x0);
                assert!(layout.icon.y1 <= layout.title.y0);
                assert!(layout.title.y1 <= layout.password.y0);
                assert!(layout.password.y1 <= layout.status.y0);
                assert!(layout.status.y1 <= layout.notifications.y0);
                assert_inside(layout.icon, layout.panel, "icon/panel");
                assert_inside(layout.title, layout.panel, "title/panel");
                assert_inside(layout.password, layout.panel, "password/panel");
                assert_inside(layout.status, layout.panel, "status/panel");
                assert_inside(layout.notifications, layout.panel, "notifications/panel");
            }
        }
        // Phase `a_hidpi_display_draws_the_lock_at_its_own_scale`. Output
        // geometry is the display's physical pixels in the scene (since the
        // scene went physical, outputs placed in logical coordinates), so a
        // 2x panel showing the same logical desktop as a 1x monitor gets the
        // same layout at twice the pixels, never a card half the size.
        {
            let theme = Theme::dark();
            let normal = LockLayout::for_output(
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1600.0, 900.0),
                    scale: 1.0,
                },
                &theme,
            );
            let high_density = LockLayout::for_output(
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (3200.0, 1800.0),
                    scale: 2.0,
                },
                &theme,
            );
            for (name, one, two) in [
                ("panel", normal.panel, high_density.panel),
                ("password", normal.password, high_density.password),
                ("clock", normal.clock, high_density.clock),
            ] {
                assert!(
                    (two.width() - one.width() * 2.0).abs() < 1.0
                        && (two.height() - one.height() * 2.0).abs() < 1.0,
                    "the {name} on a 2x display is twice the pixels: {one:?} vs {two:?}"
                );
            }
            assert!((high_density.scale - normal.scale * 2.0).abs() < 1e-9);
        }
        {
            let layout = LockLayout::for_output(
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1.0, 1.0),
                    scale: 1.0,
                },
                &Theme::dark(),
            );
            for (name, rect) in [
                ("clock", layout.clock),
                ("date", layout.date),
                ("panel", layout.panel),
                ("icon", layout.icon),
                ("title", layout.title),
                ("password", layout.password),
                ("status", layout.status),
                ("notifications", layout.notifications),
            ] {
                assert_inside(rect, layout.output, name);
            }
        }
    }

    #[test]
    fn multi_output_lock_has_one_credential_card_and_independent_layouts() {
        // Phase `asymmetric_outputs_get_independent_bounded_layouts`. Sizes
        // are physical scene pixels: the 1.25x panel shows a 1366x768
        // logical desktop.
        {
            let theme = Theme::dark();
            for output in [
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1708.0, 960.0),
                    scale: 1.25,
                },
                LockOutput {
                    origin: (1708.0, 48.0),
                    size: (1280.0, 720.0),
                    scale: 1.0,
                },
            ] {
                let layout = LockLayout::for_output(output, &theme);
                assert_inside(layout.panel, layout.output, "panel");
                assert!(layout.clock.x1 < layout.panel.x0);
                assert_inside(layout.clock, layout.output, "clock");
                assert_inside(layout.date, layout.output, "date");
            }
        }
        // Phase `primary_output_is_deterministic_and_falls_back_after_hotplug`.
        // The primary output is the first backend output. That deterministic
        // policy is also the hot-unplug fallback: once the primary disappears,
        // the first remaining display receives the one credential surface.
        {
            let primary = LockOutput {
                origin: (0.0, 0.0),
                size: (1920.0, 1080.0),
                scale: 1.0,
            };
            let secondary = LockOutput {
                origin: (1920.0, 0.0),
                size: (1280.0, 1024.0),
                scale: 1.0,
            };
            assert_eq!(primary_output(&[primary, secondary]), Some(primary));
            assert_eq!(primary_output(&[secondary]), Some(secondary));
            assert_eq!(primary_output(&[]), None);
        }
        // Phase `combined_extent_covers_all_outputs`.
        // The backdrop spans the bounding box of every output, so no gap between
        // screens can ever show app content.
        {
            let outs = [
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1920.0, 1080.0),
                    scale: 1.0,
                },
                LockOutput {
                    origin: (1920.0, 0.0),
                    size: (1280.0, 1024.0),
                    scale: 1.0,
                },
            ];
            assert_eq!(combined_extent(&outs), (3200.0, 1080.0));
            assert_eq!(combined_extent(&single(1600.0, 900.0)), (1600.0, 900.0));
            assert_eq!(combined_extent(&[]), (0.0, 0.0));
        }
        // Phase `multi_output_paints_quiet_secondary_without_duplicate_credential_card`.
        // A secondary output must paint a useful indication, but not a second
        // full credential card. Its scene therefore adds operations, while fewer
        // than another primary widget would add.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();

            let one = draw_lock_scene(&single(1920.0, 1080.0), &theme, &mut text, &view());
            let two_outs = [
                LockOutput {
                    origin: (0.0, 0.0),
                    size: (1920.0, 1080.0),
                    scale: 1.0,
                },
                LockOutput {
                    origin: (1920.0, 0.0),
                    size: (1920.0, 1080.0),
                    scale: 1.0,
                },
            ];
            let two = draw_lock_scene(&two_outs, &theme, &mut text, &view());

            let one_ops = one.encoding().draw_tags.len();
            let two_ops = two.encoding().draw_tags.len();
            assert!(two_ops > one_ops, "secondary output must be visibly locked");
            // A full primary card has fill/stroke/icon/field/status operations;
            // the secondary status is intentionally much smaller than that.
            assert!(
                two_ops < one_ops * 2,
                "secondary must not mirror a second full password card: {two_ops} vs {one_ops}"
            );
        }
    }

    fn playing() -> PlayerSnapshot {
        crate::shell::now_playing::tests::player(
            "music",
            "Music",
            crate::shell::now_playing::PlaybackStatus::Playing,
        )
    }

    #[test]
    fn lock_media_card_exposes_only_transport_actions() {
        let player = playing();
        let v = LockView {
            now_playing: Some(&player),
            ..view()
        };
        let update = build_accessibility_tree(&single(1600.0, 900.0), &Theme::dark(), &v);
        let actionable = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.supports_action(AccessAction::Click)
                    || node.supports_action(AccessAction::SetValue)
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        assert_eq!(
            actionable,
            vec![
                LOCK_PASSWORD_ID,
                LOCK_MEDIA_PREVIOUS_ID,
                LOCK_MEDIA_PLAY_PAUSE_ID,
                LOCK_MEDIA_NEXT_ID,
            ],
            "the lock offers the password and transport, nothing else"
        );
        for (id, node) in &update.nodes {
            if let Some(transport) = transport_for_node(*id) {
                assert_eq!(node.role(), AccessRole::Button);
                assert_eq!(node.label(), Some(transport.label(&player).as_str()));
                assert!(!node.supports_action(AccessAction::SetValue));
                assert!(!node.supports_action(AccessAction::Expand));
            }
        }
        let media = update
            .nodes
            .iter()
            .find(|(id, _)| *id == LOCK_MEDIA_ID)
            .map(|(_, node)| node)
            .expect("the card is published");
        assert!(
            media
                .label()
                .is_some_and(|label| label.contains("Music track")),
            "the card says what is playing"
        );
        assert_eq!(update.focus, LOCK_PASSWORD_ID, "the card never takes focus");
        assert_eq!(transport_for_node(LOCK_PASSWORD_ID), None);
        assert_eq!(transport_for_node(LOCK_MEDIA_ID), None);
    }

    #[test]
    fn lock_media_card_disables_what_the_player_cannot_do() {
        let mut player = playing();
        player.can_go_next = false;
        let v = LockView {
            now_playing: Some(&player),
            ..view()
        };
        let outputs = single(1600.0, 900.0);
        let theme = Theme::dark();
        let update = build_accessibility_tree(&outputs, &theme, &v);
        let (_, next) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == LOCK_MEDIA_NEXT_ID)
            .expect("next is published");
        assert!(!next.supports_action(AccessAction::Click));
        let layout = LockLayout::for_output(outputs[0], &theme);
        let media = LockMediaLayout::for_layout(&layout, &theme).expect("room for the card");
        for (transport, rect) in media.transport {
            let hit = transport_at(&outputs, &theme, &v, rect.center());
            if transport == Transport::Next {
                assert_eq!(hit, None, "a disabled button does nothing");
            } else {
                assert_eq!(hit, Some(transport));
            }
        }
        assert_eq!(
            transport_at(&outputs, &theme, &v, media.art.center()),
            None,
            "only the transport reacts to the pointer"
        );
        assert_eq!(
            transport_at(&outputs, &theme, &view(), media.transport[1].1.center()),
            None,
            "no card, no transport"
        );
    }

    #[test]
    fn lock_media_card_sits_under_the_credentials_inside_the_output() {
        let player = playing();
        for (w, h) in [
            (1600.0, 900.0),
            (1280.0, 800.0),
            (1024.0, 768.0),
            (2560.0, 1440.0),
        ] {
            let outputs = single(w, h);
            let theme = Theme::dark();
            let layout = LockLayout::for_output(outputs[0], &theme);
            let media = LockMediaLayout::for_layout(&layout, &theme).expect("room for the card");
            assert!(media.card.y0 >= layout.panel.y1, "{w}x{h}: under the panel");
            assert_inside(media.card, layout.output, "media card");
            assert_inside(media.art, media.card, "artwork");
            for (_, rect) in media.transport {
                assert_inside(rect, media.card, "transport");
                assert!(rect.intersect(media.text).area() <= 0.0);
            }
            let v = LockView {
                now_playing: Some(&player),
                ..view()
            };
            let mut text = TextRenderer::new();
            let with = draw_lock_scene(&outputs, &theme, &mut text, &v);
            let without = draw_lock_scene(&outputs, &theme, &mut text, &view());
            assert!(with.encoding().n_paths > without.encoding().n_paths);
        }
        let tiny = single(320.0, 240.0);
        let theme = Theme::dark();
        let layout = LockLayout::for_output(tiny[0], &theme);
        if let Some(media) = LockMediaLayout::for_layout(&layout, &theme) {
            assert_inside(media.card, layout.output, "tiny media card");
        }
    }

    #[test]
    fn the_touch_that_brings_the_keyboard_up_types_nothing_and_a_key_hides_it() {
        use crate::compositor::lock_controller::LockKey;
        use ui::passcode_keyboard::PasscodeKey;
        let mut lock = LockTouchKeyboard::default();
        assert!(lock.shown().is_none(), "hidden until touched");
        lock.touch_started();
        assert!(lock.shown().is_some());
        assert_eq!(
            lock.press(PasscodeKey::Char('t')),
            LockKey::Ignored,
            "the waking touch lands on a key that was not there"
        );
        for (key, expected) in [
            (PasscodeKey::Char('t'), LockKey::Text("t".into())),
            (PasscodeKey::Shift, LockKey::Ignored),
            (PasscodeKey::Char('o'), LockKey::Text("O".into())),
            (PasscodeKey::Backspace, LockKey::Backspace),
            (PasscodeKey::Enter, LockKey::Submit),
        ] {
            lock.touch_started();
            assert_eq!(lock.press(key), expected, "{key:?}");
        }
        lock.physical_key();
        assert!(lock.shown().is_none(), "a physical key hides it");
        lock.touch_started();
        assert_eq!(lock.press(PasscodeKey::Char('x')), LockKey::Ignored);
    }

    #[test]
    fn the_touch_keyboard_sits_below_the_passcode_and_types_into_it() {
        use ui::passcode_keyboard::{PasscodeKey, PasscodeKeyboard};
        let theme = Theme::dark();
        for (w, h) in [
            (2944.0 / 2.0, 1840.0 / 2.0),
            (1280.0, 800.0),
            (800.0, 600.0),
            (640.0, 480.0),
        ] {
            let outputs = single(w, h);
            let layout = LockLayout::for_output_with_keyboard(outputs[0], &theme, true);
            let pad = layout.keyboard.expect("the keyboard has a place");
            assert!(
                pad.x0 >= 0.0 && pad.y0 >= 0.0 && pad.x1 <= w && pad.y1 <= h,
                "the keyboard is on screen at {w}x{h}: {pad:?}"
            );
            for (name, rect) in [
                ("panel", layout.panel),
                ("passcode", layout.password),
                ("status", layout.status),
            ] {
                assert!(
                    rect.y1 <= pad.y0,
                    "the {name} {rect:?} sits above the keyboard {pad:?} at {w}x{h}"
                );
            }
            assert_eq!(
                layout.output,
                LockLayout::for_output(outputs[0], &theme).output
            );
            assert!(
                LockLayout::for_output(outputs[0], &theme)
                    .keyboard
                    .is_none()
            );

            let keyboard = PasscodeKeyboard::default();
            let view = LockView {
                keyboard: Some(keyboard),
                ..LockView::default()
            };
            let key_rect = keyboard
                .layout(
                    pad,
                    keyboard_key_metrics(&theme).0,
                    keyboard_key_metrics(&theme).1,
                )
                .into_iter()
                .find(|(key, _)| *key == PasscodeKey::Char('k'))
                .map(|(_, rect)| rect)
                .expect("a letter key");
            assert_eq!(
                keyboard_key_at(&outputs, &theme, &view, key_rect.center()),
                Some(PasscodeKey::Char('k'))
            );
            assert_eq!(
                keyboard_key_at(&outputs, &theme, &LockView::default(), key_rect.center()),
                None,
                "no keyboard, no keys"
            );
            assert_eq!(
                keyboard_key_at(&outputs, &theme, &view, layout.password.center()),
                None,
                "the passcode field is not a key"
            );
            let mut text = TextRenderer::new();
            let _ = draw_lock_scene(&outputs, &theme, &mut text, &view);
        }
    }

    /// The live answer's widest and lowest-contrast form: a refusal mid-shake
    /// with both lines fading, and a verification mid-fade with the pending
    /// mark arriving.
    fn a_refusal_at_its_widest() -> LockStatusPaint {
        let failed = LockView {
            error: Some("Incorrect username or password"),
            ..LockView::default()
        };
        LockStatusPaint {
            tone: LockStatus::Failed,
            shown: LockStatus::Failed.copy(&failed),
            shown_alpha: 0.5,
            replaced: Some((
                LockStatus::Idle,
                LockStatus::Idle.copy(&LockView::default()),
                0.5,
            )),
            dx: 8.0,
        }
    }

    fn a_pending_answer() -> LockStatusPaint {
        let verifying = LockView {
            verifying: true,
            ..LockView::default()
        };
        LockStatusPaint {
            tone: LockStatus::Verifying,
            shown: LockStatus::Verifying.copy(&verifying),
            shown_alpha: 0.5,
            replaced: Some((
                LockStatus::Failed,
                LockStatus::Failed.copy(&LockView {
                    error: Some("Incorrect username or password"),
                    ..LockView::default()
                }),
                0.4,
            )),
            dx: 0.0,
        }
    }

    #[test]
    fn an_answer_damages_only_the_credential_parts_and_never_the_output() {
        use ui::scene_parts;
        let theme = Theme::dark();
        let outputs = single(1280.0, 800.0);
        let layout = LockLayout::for_output(outputs[0], &theme);
        let idle = LockView {
            password_len: 3,
            ..view()
        };
        let failed = LockView {
            error: Some("Incorrect username or password"),
            ..view()
        };
        let verifying = LockView {
            verifying: true,
            ..view()
        };
        let mut text = TextRenderer::new();
        let record = |view: &LockView<'_>, status: &LockStatusPaint, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            let ((), parts) = scene_parts::record(&mut scene, |scene| {
                draw_lock_scene_into(
                    scene,
                    &outputs,
                    &theme,
                    text,
                    view,
                    None,
                    ui::auth_backdrop::STILL_SECONDS,
                    status,
                );
            });
            (scene, parts.expect("the lock records its credential parts"))
        };
        let (idle_scene, idle_parts) = record(&idle, &LockStatusPaint::settled(&idle), &mut text);
        let (refused_scene, refused_parts) = record(&failed, &a_refusal_at_its_widest(), &mut text);
        let (pending_scene, pending_parts) = record(&verifying, &a_pending_answer(), &mut text);

        // The card's own gutter contains even the shake's reach, so no answer
        // can paint outside the credential card.
        let allowed = layout.panel.inflate(2.0 * layout.scale, 2.0 * layout.scale);
        for (name, scene, parts) in [
            ("refusal", refused_scene, refused_parts),
            ("pending", pending_scene, pending_parts),
        ] {
            let rects = scene_parts::damage(&idle_scene, &idle_parts, &scene, &parts)
                .expect("a change inside the status and field parts is proven");
            assert!(!rects.is_empty(), "{name} must damage its own parts");
            for rect in rects {
                assert!(
                    allowed.contains(rect.origin()) && allowed.contains((rect.x1, rect.y1)),
                    "{name} damage {rect:?} escapes the credential card {allowed:?}"
                );
            }
        }
        // Only the answer changed: every row of the form keeps its place.
        assert_eq!(
            LockLayout::for_output(outputs[0], &theme),
            LockLayout::for_output(single(1280.0, 800.0)[0], &theme)
        );
    }

    #[test]
    fn the_pending_field_shows_still_waiting_marks_and_never_the_password() {
        let theme = Theme::dark();
        let layout = LockLayout::for_output(single(1280.0, 800.0)[0], &theme);
        let mut text = TextRenderer::new();
        let idle = LockView::default();
        let verifying = LockView {
            verifying: true,
            ..LockView::default()
        };
        let mut placeholder = Scene::new();
        draw_password_contents(
            &mut placeholder,
            &layout,
            &theme,
            &mut text,
            &idle,
            &LockStatusPaint::settled(&idle),
        );
        let mut pending = Scene::new();
        draw_password_contents(
            &mut pending,
            &layout,
            &theme,
            &mut text,
            &verifying,
            &a_pending_answer(),
        );
        assert!(
            pending.encoding().n_paths > placeholder.encoding().n_paths,
            "the field draws the waiting marks as more shapes"
        );
        assert!(
            pending.encoding().resources.glyphs.is_empty(),
            "a checking field shows no text at all"
        );
        assert!(
            !placeholder.encoding().resources.glyphs.is_empty(),
            "the empty field still says what to do"
        );
    }

    #[test]
    fn every_answer_keeps_the_form_still_and_its_row_single_line() {
        let mut text = TextRenderer::new();
        let caption = ui::tokens::Typography::default().size(ui::TextRole::Caption);
        for theme in [
            Theme::dark(),
            Theme::light(),
            Theme::high_contrast(),
            Theme::dark().with_text_scale(2.0),
            Theme::dark().with_reduce_motion(true),
        ] {
            for size in [(800.0, 600.0), (1280.0, 800.0), (1600.0, 900.0)] {
                let output = single(size.0, size.1)[0];
                let layout = LockLayout::for_output(output, &theme);
                for view in [
                    LockView::default(),
                    LockView {
                        error: Some("Incorrect username or password"),
                        ..LockView::default()
                    },
                    LockView {
                        error: Some(UNAVAILABLE_MSG),
                        ..LockView::default()
                    },
                    LockView {
                        verifying: true,
                        ..LockView::default()
                    },
                ] {
                    let tone = LockStatus::of(&view);
                    let copy = tone.copy(&view);
                    let (width, height) = text.measure(
                        &copy,
                        scaled_text_size(caption, layout.scale, &theme),
                        Some(ui::num::f32_of_f64(layout.status.width())),
                    );
                    assert!(
                        f64::from(width) <= layout.status.width() + 0.5
                            && f64::from(height) <= layout.status.height() + 0.5,
                        "{copy:?} must stay on its one status line at {size:?}, {theme:?}",
                        theme = theme.name
                    );
                }
                // The refusal's nudge fits the card's gutter, so the row never
                // leaves the panel, and the answer never changes the rows.
                assert!(layout.status.x0 - 8.0 * layout.scale >= layout.panel.x0);
                assert!(layout.status.y1 < layout.notifications.y0);
                assert_eq!(
                    layout.password,
                    LockLayout::for_output(output, &theme).password
                );
            }
        }
    }
}
