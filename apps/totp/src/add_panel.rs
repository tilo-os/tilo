//! The "Add account" modal — the app's second screen.
//!
//! The flow is built around how people actually add a 2FA account, easiest
//! path first:
//!
//!  1. **Scan a QR image** — the primary, obvious affordance: decode the QR
//!     out of a screenshot or photo file. This works today, fully offline,
//!     via [`crate::qr`]. Choosing the file goes through the host file
//!     chooser (the XDG portal), never an app-drawn picker.
//!  2. **Paste a setup link** — drop an `otpauth://` URI in and every field
//!     auto-fills.
//!
//! Only when neither of those fits does the user reach for **manual entry**:
//! issuer + account label + secret — the three fields almost everyone
//! needs. The fiddly cryptographic parameters (algorithm, digit count,
//! rotation period) live behind a collapsed **Advanced options** disclosure
//! so the default screen reads as calm, not as a form dump.
//!
//! The panel is driven imperatively by [`crate::app::AuthenticatorApp`]
//! (laid out by hand, not via taffy) because it is a transient overlay. It
//! owns its [`ui::widget::TextInput`] fields in local mode and manages focus
//! among them itself, forwarding key/pointer events to the focused field.
//! Results surface as an [`AddPanelEvent`] drained by the app.

use taffy::NodeId;
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::color_compat::ColorCompat;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, EventCtx, SegmentedFace, TextInput, TextInputKind, ToggleGroup, Widget,
};

use crate::model::{self, Account, Algorithm};

/// Outcome the app acts on.
#[derive(Clone, Debug)]
pub enum AddPanelEvent {
    Cancel,
    Save(Account),
    /// Replace an existing account's local secret and shared metadata. The
    /// stable id is retained, so search links, collaboration references and
    /// history all continue to identify the same account.
    Edit(Account),
    /// Attach a newly-entered local secret to an already-shared account id.
    /// This does not create a CRDT/action-history mutation.
    Provision(Account),
    /// The user asked to import a QR code from an image file. The app routes
    /// this to the **host** file chooser (the XDG Desktop Portal — the app
    /// never draws its own picker, HIG §5), then runs the decode and calls
    /// [`AddAccountPanel::apply_uri`] with the result.
    PickImageForQr,
}

/// Which text field currently has the caret. Ordered for Tab cycling; the
/// advanced fields only participate while the section is expanded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Uri,
    Issuer,
    Label,
    Secret,
    Period,
}

const MODAL_W: f64 = 460.0;
/// Authored 1× height of a text field; the measured rule is the input's
/// line box plus `s3` above and below, never shorter than this.
const INPUT_H: f64 = 42.0;
const PAD: f64 = 28.0;
/// Authored 1× sizes the measured rules fall back to (and never go below).
const CARD_MIN_H: f64 = 56.0;
/// Gap between the two import-method cards — siblings, so `s3` (HIG §7b),
/// spelled from `Spacing::BASE` because layout runs without a theme.
const CARD_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s3 as f64;
/// Gap between chips inside the Algorithm / Digits segmented rows (`s2`).
const CHIP_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s2 as f64;
const LABEL_GAP_MIN: f64 = 18.0;
/// Space between one field's bottom and the next label's band: the fields
/// are siblings of equal weight, so the `s3` step (HIG §7b); the label band
/// above each field already carries its own `s1` of air.
const ROW_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s3 as f64;
const ADV_HEADER_MIN_H: f64 = 26.0;
const TITLE_BLOCK_MIN_H: f64 = 74.0;
const FOOTER_MIN_H: f64 = 44.0;
/// The footer is the next group after the form body, so `s6`.
const FOOTER_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s6 as f64;
const DIVIDER_BLOCK_MIN_H: f64 = 30.0;
/// Authored footer button width at 1×: the pair shares it, growing only when
/// the wider measured label needs more.
const BUTTON_MAX_W: f64 = 140.0;
const PARSE_MIN_W: f64 = 84.0;
/// Renderer-relative text sizes, shared by measurement and paint.
const TITLE_SIZE: f32 = 22.0;
const SUBTITLE_SIZE: f32 = 13.0;
const LABEL_SIZE: f32 = 12.0;
const CARD_TITLE_SIZE: f32 = 15.0;
const CARD_SUB_SIZE: f32 = 12.0;
const DIVIDER_SIZE: f32 = 11.0;
const ADV_SIZE: f32 = 13.0;
/// The shared `ButtonFace` paints its label at the base type step; the
/// footer buttons are measured with the same size.
const BUTTON_LABEL_SIZE: f32 = 14.0;

/// Sizes the panel is laid out with, measured on paint with the styles the
/// panel paints with. `Default` is the authored 1× rule set, used before the
/// first paint and by the geometry tests (which never paint).
#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelMetrics {
    /// Text scale the sizes were measured at (grows icon tiles with them).
    scale: f64,
    /// The theme's spacing steps the bands are padded with. `layout` runs
    /// without a theme, so it reads the steps the last paint measured against
    /// (the base tokens before the first paint) — under a custom theme or a
    /// UI scale the reserved bands and the painted ones then still agree.
    s1: f64,
    s2: f64,
    s3: f64,
    s6: f64,
    /// Line boxes of the title block, field labels, method-card text, the
    /// divider caption and the Advanced header.
    title_h: f64,
    subtitle_h: f64,
    label_h: f64,
    card_title_h: f64,
    card_sub_h: f64,
    divider_h: f64,
    adv_h: f64,
    /// A text field's line box (the shared input's own body line).
    input_line_h: f64,
    /// A footer button's label line box, and the measured widths of the
    /// Cancel / primary labels.
    button_label_h: f64,
    cancel_label_w: f64,
    save_label_w: f64,
    fill_label_w: f64,
}

impl Default for PanelMetrics {
    fn default() -> Self {
        // 1× line boxes (size × 1.25), label widths at 14 px, base tokens.
        Self {
            scale: 1.0,
            s1: 4.0,
            s2: 8.0,
            s3: 12.0,
            s6: 24.0,
            title_h: 27.5,
            subtitle_h: 16.25,
            label_h: 15.0,
            card_title_h: 18.75,
            card_sub_h: 15.0,
            divider_h: 13.75,
            adv_h: 16.25,
            input_line_h: 17.5,
            button_label_h: 17.5,
            cancel_label_w: 46.0,
            save_label_w: 84.0,
            fill_label_w: 20.0,
        }
    }
}

impl PanelMetrics {
    /// Measure with the styles the panel paints with. `subtitle` is the
    /// current explanation line, measured wrapped to `content_w` so the
    /// title block makes room when it takes two lines at a larger scale.
    fn measure(
        tr: &mut TextRenderer,
        theme: &Theme,
        save_label: &str,
        subtitle: &str,
        content_w: f32,
    ) -> Self {
        let h = |tr: &mut TextRenderer, size: f32, weight: TextWeight| {
            f64::from(tr.measure_weighted("Ag", size, None, weight).1)
        };
        let sp = &theme.tokens.spacing;
        Self {
            scale: f64::from(theme.text_scale).max(1.0),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            s6: f64::from(sp.s6),
            title_h: h(tr, TITLE_SIZE, TextWeight::Bold),
            subtitle_h: f64::from(tr.measure(subtitle, SUBTITLE_SIZE, Some(content_w)).1),
            label_h: h(tr, LABEL_SIZE, TextWeight::Medium),
            card_title_h: h(tr, CARD_TITLE_SIZE, TextWeight::Semibold),
            card_sub_h: h(tr, CARD_SUB_SIZE, TextWeight::Regular),
            divider_h: h(tr, DIVIDER_SIZE, TextWeight::Regular),
            adv_h: h(tr, ADV_SIZE, TextWeight::Medium),
            input_line_h: h(tr, BUTTON_LABEL_SIZE, TextWeight::Regular),
            button_label_h: h(tr, BUTTON_LABEL_SIZE, TextWeight::Medium),
            cancel_label_w: f64::from(
                tr.measure_weighted(
                    &crate::i18n::t("totp-add-cancel"),
                    BUTTON_LABEL_SIZE,
                    None,
                    TextWeight::Medium,
                )
                .0,
            ),
            save_label_w: f64::from(
                tr.measure_weighted(save_label, BUTTON_LABEL_SIZE, None, TextWeight::Medium)
                    .0,
            ),
            fill_label_w: f64::from(
                tr.measure_weighted(
                    &crate::i18n::t("totp-add-fill"),
                    BUTTON_LABEL_SIZE,
                    None,
                    TextWeight::Medium,
                )
                .0,
            ),
        }
    }

    /// Height of a text field: its line box plus `s3` above and below.
    fn input_h(&self, s3: f64) -> f64 {
        (self.input_line_h + 2.0 * s3).max(INPUT_H)
    }
    /// Band a field label occupies above its field.
    fn label_gap(&self, s1: f64) -> f64 {
        (self.label_h + s1).max(LABEL_GAP_MIN)
    }
    /// Height of a method card: title + description plus `s2` above and below.
    fn card_h(&self, s2: f64) -> f64 {
        (self.card_title_h + self.card_sub_h + 2.0 * s2).max(CARD_MIN_H)
    }
    fn adv_header_h(&self, s1: f64) -> f64 {
        (self.adv_h + 2.0 * s1).max(ADV_HEADER_MIN_H)
    }
    /// The fixed title block: `s6` above the title, the subtitle directly
    /// under it, `s2` below.
    fn title_block_h(&self, s6: f64, s2: f64) -> f64 {
        (s6 + self.title_h + self.subtitle_h + s2).max(TITLE_BLOCK_MIN_H)
    }
    fn footer_h(&self, s3: f64) -> f64 {
        (self.button_label_h + 2.0 * s3).max(FOOTER_MIN_H)
    }
    fn divider_block_h(&self, s2: f64) -> f64 {
        (self.divider_h + 2.0 * s2).max(DIVIDER_BLOCK_MIN_H)
    }
    /// Width a button needs for `label_w`: the label plus `s6` each side.
    /// `layout` starts from the authored width and grows to this only when
    /// the label needs it, capped so the footer pair always fits.
    fn button_w(label_w: f64, s6: f64) -> f64 {
        label_w + 2.0 * s6
    }
}

// `open`, `reveal_advanced`, `uri_expanded` and `advanced_open` are four
// independent UI-disclosure flags that toggle separately, not a
// combinatorial state machine — consolidating them into an enum would lose
// that independence rather than gain clarity.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent disclosure flags, not a combinatorial state machine"
)]
pub struct AddAccountPanel {
    open: bool,
    window: Rect,
    modal: Rect,
    /// Vertical scroll of the body region between the fixed title and footer,
    /// so the form is reachable even in a short window.
    scroll: f64,
    max_scroll: f64,
    /// Set when the Advanced section is revealed so the next layout scrolls
    /// its newly-shown fields into view rather than leaving them below the
    /// fold. Consumed (cleared) by [`layout`].
    reveal_advanced: bool,
    /// Clip rect for the scrolling body (fixed title/footer draw outside it).
    body_clip: Rect,
    /// Text sizes measured on the last paint with the styles the panel
    /// paints with; `layout` reads them (the 1× rules until the first paint)
    /// so a larger text scale grows rows, labels and buttons instead of
    /// stacking them over each other.
    metrics: Option<PanelMetrics>,
    /// What `metrics` was measured for — `(renderer scale, theme scale,
    /// catalogue revision, editing, provisioning, content width)`; an
    /// unchanged key skips the lookups and measures on the next paint.
    metrics_key: Option<(u32, u32, u64, bool, bool, u32)>,

    // Easy-path entry.
    uri: TextInput,
    /// The paste-a-link row is itself disclosed: collapsed to a single
    /// tappable method until the user chooses it.
    uri_expanded: bool,

    // Manual fields (the three most accounts need).
    issuer: TextInput,
    label: TextInput,
    secret: TextInput,

    // Advanced parameters, hidden until disclosed.
    advanced_open: bool,
    algorithm: Algorithm,
    digits: u8,
    period: TextInput,

    focused: Option<Field>,
    last_mouse: Point,
    error: String,
    event: Option<AddPanelEvent>,
    /// The stable account identity being edited, if this panel is an edit
    /// rather than a new-account flow. The secret remains only in this local
    /// form and the encrypted vault; it is never copied to the CRDT.
    editing: Option<crate::model::AccountId>,
    /// Immutable identity guard for local setup of a shared account.
    provisioning: Option<Account>,

    // Laid-out hit rects, recomputed in `layout`.
    image_card_rect: Rect,
    link_card_rect: Rect,
    uri_rect: Rect,
    parse_rect: Rect,
    issuer_rect: Rect,
    label_rect: Rect,
    secret_rect: Rect,
    advanced_header_rect: Rect,
    alg_rects: [Rect; 3],
    digit_rects: [Rect; 2],
    period_rect: Rect,
    cancel_rect: Rect,
    save_rect: Rect,
}

impl Default for AddAccountPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AddAccountPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            window: Rect::ZERO,
            modal: Rect::ZERO,
            scroll: 0.0,
            max_scroll: 0.0,
            reveal_advanced: false,
            body_clip: Rect::ZERO,
            metrics: None,
            metrics_key: None,
            // Placeholders belong to the shared field so they share its font,
            // padding and centring with the text the user types.
            uri: TextInput::local()
                .with_kind(TextInputKind::Url)
                .with_placeholder("otpauth://totp/Issuer:account?secret=\u{2026}"),
            uri_expanded: false,
            issuer: TextInput::local()
                .with_kind(TextInputKind::Name)
                .with_placeholder("GitHub"),
            label: TextInput::local()
                .with_kind(TextInputKind::Name)
                .with_placeholder("ada@example.com"),
            secret: TextInput::local()
                .with_kind(TextInputKind::Password)
                .with_placeholder("JBSWY3DPEHPK3PXP"),
            advanced_open: false,
            algorithm: Algorithm::Sha1,
            digits: 6,
            period: TextInput::local()
                .with_kind(TextInputKind::Number)
                .with_initial_value("30"),
            focused: None,
            last_mouse: Point::ZERO,
            error: String::new(),
            event: None,
            editing: None,
            provisioning: None,
            image_card_rect: Rect::ZERO,
            link_card_rect: Rect::ZERO,
            uri_rect: Rect::ZERO,
            parse_rect: Rect::ZERO,
            issuer_rect: Rect::ZERO,
            label_rect: Rect::ZERO,
            secret_rect: Rect::ZERO,
            advanced_header_rect: Rect::ZERO,
            alg_rects: [Rect::ZERO; 3],
            digit_rects: [Rect::ZERO; 2],
            period_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            save_rect: Rect::ZERO,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the panel, resetting every field to defaults and collapsing the
    /// disclosures so each visit starts from the calm default state.
    pub fn open(&mut self) {
        self.open = true;
        self.error.clear();
        self.editing = None;
        self.provisioning = None;
        self.scroll = 0.0;
        self.reveal_advanced = false;
        self.uri.set_text("");
        self.uri_expanded = false;
        self.issuer.set_text("");
        self.label.set_text("");
        self.secret.set_text("");
        self.advanced_open = false;
        self.algorithm = Algorithm::Sha1;
        self.digits = 6;
        self.period.set_text("30");
        self.set_focus(None);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.set_focus(None);
    }

    /// Open with the Advanced-options section already expanded. Used by the
    /// `--shots` renderer to capture the tall, scrolling form; runtime opens
    /// via `open` with Advanced collapsed.
    pub fn open_with_advanced(&mut self) {
        self.open();
        self.advanced_open = true;
        self.reveal_advanced = true;
    }

    /// Open the same calm form for an existing account. This is intentionally
    /// local: only this device's encrypted vault supplies the secret field.
    pub fn open_for_edit(&mut self, account: &Account) {
        self.open();
        self.editing = Some(account.id);
        self.issuer.set_text(&account.issuer);
        self.label.set_text(&account.label);
        self.secret.set_text(&account.secret);
        self.period.set_text(&account.period.to_string());
        self.algorithm = account.algorithm;
        self.digits = account.digits;
        self.advanced_open =
            account.algorithm != Algorithm::Sha1 || account.digits != 6 || account.period != 30;
        self.set_focus(Some(Field::Issuer));
    }

    /// Provision the secret for this exact existing resource. The form starts
    /// with the shared identity, then accepts only local credential material.
    pub fn open_for_provision(&mut self, account: &Account) {
        self.open();
        self.editing = Some(account.id);
        self.provisioning = Some(account.clone());
        self.issuer.set_text(&account.issuer);
        self.label.set_text(&account.label);
        self.period.set_text(&account.period.to_string());
        self.algorithm = account.algorithm;
        self.digits = account.digits;
        self.set_focus(Some(Field::Secret));
    }

    /// The panel's heading for the current flow.
    fn title(&self) -> String {
        crate::i18n::t(if self.is_provisioning() {
            "totp-add-title-provision"
        } else if self.is_editing() {
            "totp-add-title-edit"
        } else {
            "totp-add-title-add"
        })
    }

    /// The calm one-line explanation under the heading. One source for the
    /// chrome painter and the measurement that sizes the title block (the
    /// line wraps within the card at a larger text scale).
    fn subtitle(&self) -> String {
        crate::i18n::t(if self.is_provisioning() {
            "totp-add-subtitle-provision"
        } else if self.is_editing() {
            "totp-add-subtitle-edit"
        } else {
            "totp-add-subtitle-add"
        })
    }

    /// Catalogue key of the footer's primary label for the current flow. One
    /// source for the footer painter (the shared button face resolves the
    /// key itself) and the measurement that sizes its button.
    fn save_label_key(&self) -> &'static str {
        if self.is_provisioning() {
            "totp-add-save-provision"
        } else if self.is_editing() {
            "totp-add-save-edit"
        } else {
            "totp-add-save-add"
        }
    }

    fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    fn is_provisioning(&self) -> bool {
        self.provisioning.is_some()
    }

    pub fn take_event(&mut self) -> Option<AddPanelEvent> {
        self.event.take()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "dialog semantics are kept with its controls"
    )]
    pub fn accessibility(&self) -> AccessNode {
        let input =
            |name: &str, value: &str, rect: Rect, focused: bool, protected: bool, target: &str| {
                let node = AccessNode::new(Role::TextInput)
                    .with_name(name)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(focused)
                            .protected(protected),
                    )
                    .with_bounds(rect)
                    .with_action_target(target);
                if protected {
                    node
                } else {
                    node.with_value(value)
                }
            };
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Scan a QR image")
                .with_state(AccessState::new().focusable(true))
                .with_bounds(self.image_card_rect)
                .with_action_target("totp.dialog.scan"),
            AccessNode::new(Role::Button)
                .with_name("Paste a setup link")
                .with_state(AccessState::new().focusable(true))
                .with_bounds(self.link_card_rect)
                .with_action_target("totp.dialog.link"),
            input(
                "Issuer",
                self.issuer.value(),
                self.issuer_rect,
                self.focused == Some(Field::Issuer),
                false,
                "totp.dialog.issuer",
            ),
            input(
                "Account name",
                self.label.value(),
                self.label_rect,
                self.focused == Some(Field::Label),
                false,
                "totp.dialog.label",
            ),
            input(
                "Secret key",
                "",
                self.secret_rect,
                self.focused == Some(Field::Secret),
                true,
                "totp.dialog.secret",
            ),
            AccessNode::new(Role::Button)
                .with_name("Advanced options")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .expanded(self.advanced_open),
                )
                .with_bounds(self.advanced_header_rect)
                .with_action_target("totp.dialog.advanced"),
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_state(AccessState::new().focusable(true))
                .with_bounds(self.cancel_rect)
                .with_action_target("totp.dialog.cancel"),
            AccessNode::new(Role::Button)
                .with_name(if self.is_provisioning() {
                    "Set up securely"
                } else if self.is_editing() {
                    "Save changes"
                } else {
                    "Add account"
                })
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .disabled(self.secret.value().trim().is_empty()),
                )
                .with_bounds(self.save_rect)
                .with_action_target("totp.dialog.save"),
        ];
        if self.uri_expanded {
            children.push(input(
                "Setup link",
                self.uri.value(),
                self.uri_rect,
                self.focused == Some(Field::Uri),
                true,
                "totp.dialog.uri",
            ));
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Fill from setup link")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.parse_rect)
                    .with_action_target("totp.dialog.fill"),
            );
        }
        if self.advanced_open {
            for (index, algorithm) in Algorithm::ALL.iter().enumerate() {
                if let Some(rect) = self.alg_rects.get(index) {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(algorithm.as_str())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(*algorithm == self.algorithm),
                            )
                            .with_bounds(*rect)
                            .with_action_target(format!("totp.dialog.algorithm:{index}")),
                    );
                }
            }
            for (index, digits) in [6_u8, 8].iter().enumerate() {
                if let Some(rect) = self.digit_rects.get(index) {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("{digits} digits"))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(*digits == self.digits),
                            )
                            .with_bounds(*rect)
                            .with_action_target(format!("totp.dialog.digits:{digits}")),
                    );
                }
            }
            children.push(input(
                "Period seconds",
                self.period.value(),
                self.period_rect,
                self.focused == Some(Field::Period),
                false,
                "totp.dialog.period",
            ));
        }
        if !self.error.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(&self.error)
                    .with_live(Politeness::Assertive),
            );
        }
        AccessNode::new(Role::Dialog)
            .with_name(if self.is_provisioning() {
                "Set up account on this device"
            } else if self.is_editing() {
                "Edit account"
            } else {
                "Add account"
            })
            .with_state(AccessState::new().modal(true))
            .with_bounds(self.modal)
            .with_children(children)
    }

    pub fn perform_a11y_target(&mut self, target: &str, action: &A11yAction) -> bool {
        let field = match target {
            "totp.dialog.uri" => Some(Field::Uri),
            "totp.dialog.issuer" => Some(Field::Issuer),
            "totp.dialog.label" => Some(Field::Label),
            "totp.dialog.secret" => Some(Field::Secret),
            "totp.dialog.period" => Some(Field::Period),
            _ => None,
        };
        if let Some(field) = field {
            match action {
                A11yAction::Focus | A11yAction::Activate => self.set_focus(Some(field)),
                A11yAction::SetValue(value) => {
                    self.field_mut(field).set_text(value);
                    self.error.clear();
                }
                _ => return false,
            }
            return true;
        }
        if !matches!(action, A11yAction::Activate) {
            return false;
        }
        match target {
            "totp.dialog.scan" => self.event = Some(AddPanelEvent::PickImageForQr),
            "totp.dialog.link" => self.expand_uri(),
            "totp.dialog.fill" => self.parse_uri_field(),
            "totp.dialog.advanced" => self.toggle_advanced(),
            "totp.dialog.cancel" => {
                self.close();
                self.event = Some(AddPanelEvent::Cancel);
            }
            "totp.dialog.save" => self.try_save(),
            "totp.dialog.algorithm:0" => self.algorithm = Algorithm::Sha1,
            "totp.dialog.algorithm:1" => self.algorithm = Algorithm::Sha256,
            "totp.dialog.algorithm:2" => self.algorithm = Algorithm::Sha512,
            "totp.dialog.digits:6" => self.digits = 6,
            "totp.dialog.digits:8" => self.digits = 8,
            _ => return false,
        }
        true
    }

    /// Order text fields participate in Tab cycling, given the current
    /// disclosure state. The URI field joins only when the paste row is
    /// expanded; the period field only when Advanced is open.
    fn field_order(&self) -> Vec<Field> {
        let mut order = Vec::with_capacity(5);
        if self.uri_expanded {
            order.push(Field::Uri);
        }
        order.push(Field::Issuer);
        order.push(Field::Label);
        order.push(Field::Secret);
        if self.advanced_open {
            order.push(Field::Period);
        }
        order
    }

    fn field_mut(&mut self, f: Field) -> &mut TextInput {
        match f {
            Field::Uri => &mut self.uri,
            Field::Issuer => &mut self.issuer,
            Field::Label => &mut self.label,
            Field::Secret => &mut self.secret,
            Field::Period => &mut self.period,
        }
    }

    fn set_focus(&mut self, f: Option<Field>) {
        for field in [
            Field::Uri,
            Field::Issuer,
            Field::Label,
            Field::Secret,
            Field::Period,
        ] {
            let on = Some(field) == f;
            self.field_mut(field).set_focus(on);
        }
        self.focused = f;
    }

    fn focus_next(&mut self, backward: bool) {
        let order = self.field_order();
        if order.is_empty() {
            return;
        }
        let cur = self
            .focused
            .and_then(|f| order.iter().position(|x| *x == f))
            .unwrap_or(0);
        let n = order.len();
        let next = if backward {
            (cur + n - 1) % n
        } else {
            (cur + 1) % n
        };
        // `next` is always `< n == order.len()` by construction (both
        // branches above compute a value mod `n`), so this is unreachable in
        // practice — `.get()` documents that instead of indexing directly.
        if let Some(&field) = order.get(next) {
            self.set_focus(Some(field));
        }
    }

    /// Reveal the paste-a-link row and focus its field.
    fn expand_uri(&mut self) {
        self.uri_expanded = true;
        self.error.clear();
        self.set_focus(Some(Field::Uri));
    }

    fn toggle_advanced(&mut self) {
        self.advanced_open = !self.advanced_open;
        if self.advanced_open {
            // Reveal the newly-shown Algorithm/Digits/Period fields on the
            // next layout instead of leaving them below the scroll fold.
            self.reveal_advanced = true;
        } else if self.focused == Some(Field::Period) {
            self.set_focus(None);
        }
    }

    // ---- layout ------------------------------------------------------

    /// Recompute the modal + every field rect for the given window.
    ///
    /// The modal grows to fit its content up to the available height; beyond
    /// that the body between the fixed title and footer scrolls, so the form
    /// is fully reachable even in a short window. Body rects are laid out
    /// already offset by the current scroll so hit-testing and drawing share
    /// one geometry.
    /// The metrics `layout` runs on: the last paint's measurements, or the
    /// authored 1× rules before anything has been painted.
    fn metrics(&self) -> PanelMetrics {
        self.metrics.unwrap_or_default()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one top-down pass that must mirror its own natural-height accumulation exactly; splitting it would separate the two halves that have to agree"
    )]
    pub fn layout(&mut self, window: Rect) {
        self.window = window;
        let m = self.metrics();
        // The theme's spacing steps, carried in the metrics the last paint
        // measured against (base tokens before the first paint).
        let (s1, s2, s3, s6) = (m.s1, m.s2, m.s3, m.s6);
        let w = MODAL_W.min(window.width() - 48.0);
        let left_pad = PAD;
        let content_w = w - left_pad * 2.0;

        // Every band is its measured text plus token pads, never shorter
        // than the authored 1× band, so 200% text grows the form instead of
        // painting labels over fields and fields over each other.
        let input_h = m.input_h(s3);
        let card_h = m.card_h(s2);
        let card_gap = CARD_GAP;
        let label_gap = m.label_gap(s1);
        let row_gap = ROW_GAP;
        let adv_header_h = m.adv_header_h(s1);
        let divider_h = m.divider_block_h(s2);
        // The fixed title block holds the "Add account" heading and its
        // subtitle; the scrolling body starts below it.
        let title_h = m.title_block_h(s6, s2);
        let footer_h = m.footer_h(s3);
        let footer_gap = FOOTER_GAP;

        // Natural body height (title/footer excluded). Mirrors the running-y
        // accumulation below exactly so the scroll range is precise.
        let mut body_natural = card_h * 2.0 + card_gap; // two method cards
        if self.uri_expanded {
            body_natural += label_gap + input_h + s3; // gap + uri row
        }
        body_natural += divider_h; // "or enter details" divider
        body_natural += (label_gap + input_h + row_gap) * 3.0; // issuer/label/secret
        body_natural += adv_header_h;
        if self.advanced_open {
            body_natural += row_gap + label_gap + input_h + row_gap; // through the algorithm row
            body_natural += label_gap + input_h; // digits + period row
        }

        let natural_h = title_h + body_natural + footer_gap + footer_h + PAD;
        let avail_h = (window.height() - 32.0).max(320.0);
        let h = natural_h.min(avail_h).max(320.0);

        let x0 = window.center().x - w / 2.0;
        let y0 = (window.center().y - h / 2.0).max(window.y0 + 16.0);
        self.modal = Rect::new(x0, y0, x0 + w, y0 + h);

        let left = x0 + left_pad;
        let right = x0 + w - left_pad;

        // Footer pinned to the bottom of the card (outside the scroll region):
        // primary at the right edge, Cancel one `s3` before it. The two share
        // one width — the authored half-of-content cap (140 px at 1×), grown
        // only when the wider measured label needs more — so the 1× footer is
        // exactly the authored one and 200% labels still fit their buttons.
        let half = (content_w - s3) / 2.0;
        let btn_w = half
            .min(BUTTON_MAX_W)
            .max(PanelMetrics::button_w(
                m.save_label_w.max(m.cancel_label_w),
                s6,
            ))
            .min(half);
        let by = y0 + h - PAD - footer_h;
        self.save_rect = Rect::new(right - btn_w, by, right, by + footer_h);
        self.cancel_rect = Rect::new(
            right - btn_w - s3 - btn_w,
            by,
            right - btn_w - s3,
            by + footer_h,
        );

        // Scrollable body between the fixed title and the fixed footer.
        let body_top = y0 + title_h;
        let body_bottom = by - footer_gap;
        let visible_body = (body_bottom - body_top).max(0.0);
        self.max_scroll = (body_natural - visible_body).max(0.0);
        // When Advanced was just revealed, scroll so its fields (the
        // bottom-most body content) are on screen rather than below the
        // fold. Consumed here so later resizes keep the user's scroll.
        if self.reveal_advanced {
            self.scroll = self.max_scroll;
            self.reveal_advanced = false;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        self.body_clip = Rect::new(x0, body_top, x0 + w, body_bottom);

        // Lay out body rects top-down, already offset by the scroll position.
        let mut y = body_top - self.scroll;

        self.image_card_rect = Rect::new(left, y, right, y + card_h);
        y += card_h + card_gap;
        self.link_card_rect = Rect::new(left, y, right, y + card_h);
        y += card_h;

        if self.uri_expanded {
            y += label_gap;
            let parse_w = PanelMetrics::button_w(m.fill_label_w, s6).max(PARSE_MIN_W);
            self.uri_rect = Rect::new(left, y, right - parse_w - s2, y + input_h);
            self.parse_rect = Rect::new(right - parse_w, y, right, y + input_h);
            y += input_h + s3;
        } else {
            self.uri_rect = Rect::ZERO;
            self.parse_rect = Rect::ZERO;
        }

        // "Or enter details" divider band.
        y += divider_h;

        self.issuer_rect = Rect::new(left, y + label_gap, right, y + label_gap + input_h);
        y += label_gap + input_h + row_gap;
        self.label_rect = Rect::new(left, y + label_gap, right, y + label_gap + input_h);
        y += label_gap + input_h + row_gap;
        self.secret_rect = Rect::new(left, y + label_gap, right, y + label_gap + input_h);
        y += label_gap + input_h + row_gap;

        self.advanced_header_rect = Rect::new(left, y, right, y + adv_header_h);
        y += adv_header_h;

        self.layout_advanced_fields(left, right, content_w, y, input_h, label_gap);

        // Push rects into the text inputs.
        self.uri.apply_layout_external(self.uri_rect);
        self.issuer.apply_layout_external(self.issuer_rect);
        self.label.apply_layout_external(self.label_rect);
        self.secret.apply_layout_external(self.secret_rect);
        self.period.apply_layout_external(self.period_rect);
    }

    /// Lay out the Algorithm/Digits/Period rects starting at `y`, or zero
    /// them when Advanced is collapsed. Split out of [`Self::layout`] to
    /// keep that function's line count in check. `input_h` / `label_gap`
    /// are the measured bands `layout` used for the fields above.
    fn layout_advanced_fields(
        &mut self,
        left: f64,
        right: f64,
        content_w: f64,
        y: f64,
        input_h: f64,
        label_gap: f64,
    ) {
        if self.advanced_open {
            let scale = self.metrics().scale;
            // The same `ROW_GAP` the fields above use between one row and the
            // next label band, and `CHIP_GAP` between the chips of one
            // segmented row — one rhythm for the whole form (HIG §7b).
            let y = y + ROW_GAP;
            let chip_y = y + label_gap;
            let alg_w = (content_w - 2.0 * CHIP_GAP) / 3.0;
            for (i, r) in self.alg_rects.iter_mut().enumerate() {
                let cx = left + i as f64 * (alg_w + CHIP_GAP);
                *r = Rect::new(cx, chip_y, cx + alg_w, chip_y + input_h);
            }
            let y = chip_y + input_h + ROW_GAP;
            let row_y = y + label_gap;
            // The digit chips and the period field hold two-digit numbers;
            // their widths follow the text scale.
            let dig_w = 56.0 * scale;
            for (i, r) in self.digit_rects.iter_mut().enumerate() {
                let cx = left + i as f64 * (dig_w + CHIP_GAP);
                *r = Rect::new(cx, row_y, cx + dig_w, row_y + input_h);
            }
            let period_w = 88.0 * scale;
            let period_x = right - period_w;
            self.period_rect = Rect::new(period_x, row_y, right, row_y + input_h);
        } else {
            self.alg_rects = [Rect::ZERO; 3];
            self.digit_rects = [Rect::ZERO; 2];
            self.period_rect = Rect::ZERO;
        }
    }

    // ---- account building / actions ----------------------------------

    /// Build an account from the current field values, or an error string
    /// describing why it can't be saved.
    fn build_account(&self) -> Result<Account, String> {
        let secret = model::normalise_secret(self.secret.value());
        if secret.is_empty() {
            return Err("Enter a secret key".to_string());
        }
        if crate::totp::decode_secret(&secret).is_none_or(|b| b.is_empty()) {
            return Err("That secret isn\u{2019}t valid base32".to_string());
        }
        let issuer = self.issuer.value().trim().to_string();
        let label = self.label.value().trim().to_string();
        if model::contains_secret_shaped_text(&issuer) || model::contains_secret_shaped_text(&label)
        {
            return Err("Account names must not include a secret key".to_string());
        }
        if issuer.is_empty() && label.is_empty() {
            return Err("Add an issuer or account name".to_string());
        }
        let period = self
            .period
            .value()
            .trim()
            .parse::<u32>()
            .unwrap_or(30)
            .clamp(5, 300);
        let color = (issuer.len().wrapping_add(label.len()) % 7) as u8;
        Ok(Account {
            id: self.editing.unwrap_or_else(Uuid::new_v4),
            issuer,
            label,
            secret,
            algorithm: self.algorithm,
            digits: self.digits,
            period,
            color,
            order: 0,
        })
    }

    /// Apply a decoded `otpauth://` URI (from paste or QR image import) into
    /// the manual fields. On success the manual
    /// section shows the filled values so the user can review before saving;
    /// on failure the reason surfaces inline.
    pub fn apply_uri(&mut self, uri: &str) {
        match model::parse_otpauth_uri(uri, Uuid::new_v4()) {
            Ok(acc) => {
                self.issuer.set_text(&acc.issuer);
                self.label.set_text(&acc.label);
                self.secret.set_text(&acc.secret);
                self.period.set_text(&acc.period.to_string());
                self.algorithm = acc.algorithm;
                self.digits = acc.digits;
                // Surface non-default crypto params so a reviewing user can
                // see what the code carried.
                if acc.algorithm != Algorithm::Sha1 || acc.digits != 6 || acc.period != 30 {
                    self.advanced_open = true;
                }
                self.error.clear();
                self.set_focus(Some(Field::Issuer));
            }
            Err(e) => self.error = e,
        }
    }

    /// Parse whatever is currently in the URI field.
    fn parse_uri_field(&mut self) {
        let uri = self.uri.value().to_string();
        if uri.trim().is_empty() {
            self.error = "Paste an otpauth:// link first".to_string();
            return;
        }
        self.apply_uri(&uri);
    }

    fn try_save(&mut self) {
        match self.build_account() {
            Ok(mut acc) => {
                if let Some(existing) = &self.provisioning {
                    if acc.issuer.trim() != existing.issuer.trim()
                        || acc.label.trim() != existing.label.trim()
                    {
                        self.error = "This setup link names a different account. Edit shared details separately.".into();
                        return;
                    }
                    if acc.algorithm != existing.algorithm
                        || acc.digits != existing.digits
                        || acc.period != existing.period
                    {
                        self.error = "This setup link uses different TOTP settings. Update shared settings first, then set up this device.".into();
                        return;
                    }
                    acc.id = existing.id;
                    acc.color = existing.color;
                    acc.order = existing.order;
                }
                self.event = Some(if self.is_provisioning() {
                    AddPanelEvent::Provision(acc)
                } else if self.is_editing() {
                    AddPanelEvent::Edit(acc)
                } else {
                    AddPanelEvent::Save(acc)
                });
                self.open = false;
            }
            Err(e) => self.error = e,
        }
    }

    // ---- drawing -----------------------------------------------------

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.open {
            return;
        }
        // Measure with this frame's text scale and lay the panel out again
        // from those sizes before painting, so the rects that are painted
        // (and hit-tested until the next layout) are the measured ones even
        // on a single headless frame. Before the first `layout` the window is
        // empty and there is nothing sane to measure against; the authored
        // rules stand until it is known.
        if self.window.width() > 2.0 * PAD + 48.0 {
            let content_w = (MODAL_W.min(self.window.width() - 48.0) - 2.0 * PAD) as f32;
            // Labels and line boxes change only with the renderer's scale,
            // the catalogue, the flow (which picks the labels) and the width
            // the subtitle wraps to; skip the lookups and measures otherwise.
            let key = (
                text_renderer.text_scale().to_bits(),
                theme.text_scale.to_bits(),
                ui::i18n::I18nManager::revision(),
                self.is_editing(),
                self.is_provisioning(),
                content_w.to_bits(),
            );
            if self.metrics_key != Some(key) {
                self.metrics_key = Some(key);
                let measured = PanelMetrics::measure(
                    text_renderer,
                    theme,
                    &crate::i18n::t(self.save_label_key()),
                    &self.subtitle(),
                    content_w,
                );
                if self.metrics != Some(measured) {
                    self.metrics = Some(measured);
                    self.layout(self.window);
                }
            }
        }
        let presence = PresenceCtx::empty();
        self.draw_chrome(scene, transform, text_renderer, theme);

        // Everything between the fixed title and footer scrolls; clip it so
        // scrolled rows never bleed into the chrome.
        let clip = RoundedRect::from_rect(self.body_clip, 0.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        self.draw_import_methods(scene, transform, text_renderer, theme, &presence);
        self.draw_manual_fields(scene, transform, text_renderer, theme);
        self.draw_advanced_section(scene, transform, text_renderer, theme, &presence);
        scene.pop_layer();

        self.draw_footer(scene, transform, text_renderer, theme);
    }

    /// Scrim, modal card and the fixed title/subtitle — everything outside
    /// the scrolling body. Split out of [`Self::draw`] to keep that
    /// function's line count in check.
    fn draw_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Scrim.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &self.window,
        );
        // Modal card: a floating Panel, so its edge is the derived panel
        // border (HIG §9.5) — `border_standard` on `raised` is invisible on
        // the dark preset — stroked crisp on the outermost pixel row.
        let radius = f64::from(theme.tokens.radii.xl);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(self.modal, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(self.modal, radius),
        );

        let left = self.modal.x0 + PAD;
        // Title + calm subtitle, stacked from their measured line boxes (the
        // same rule `layout` sized the title block with) so the subtitle sits
        // under the title at every text scale.
        let m = self.metrics();
        let sp = &theme.tokens.spacing;
        let title_y = self.modal.y0 + f64::from(sp.s6);
        let subtitle_y = title_y + m.title_h;
        text_renderer.draw_text_weighted(
            scene,
            &self.title(),
            TITLE_SIZE,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((left, title_y)),
            None,
            TextWeight::Bold,
        );
        text_renderer.draw_text(
            scene,
            &self.subtitle(),
            SUBTITLE_SIZE,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((left, subtitle_y)),
            Some((self.modal.width() - 2.0 * PAD) as f32),
        );
    }

    /// The two big method cards (scan a QR image / paste a link) and, when
    /// disclosed, the paste-link row. Split out of [`Self::draw`].
    fn draw_import_methods(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // --- Method cards. The image importer leads (it is the real QR
        // path today); pasting a setup link is the calm secondary. ---
        // The long description needs ~400 px of card at 1×; the threshold
        // follows the text scale so 200% text picks the short form instead of
        // running under the chevron.
        let tight = self.modal.width() < 400.0 * self.metrics().scale;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_method_card(&mut ctx, ImportMethod::Image, tight);
        self.draw_method_card(&mut ctx, ImportMethod::Link, tight);

        // Paste-link field (disclosed).
        if self.uri_expanded {
            self.uri
                .draw(scene, transform, text_renderer, theme, presence);
            let parse_hover = self.parse_rect.contains(self.last_mouse);
            Self::draw_button(
                scene,
                transform,
                text_renderer,
                theme,
                self.parse_rect,
                "totp-add-fill",
                parse_hover,
                true,
            );
        }
    }

    /// The "or enter details" divider plus the three manual fields (issuer,
    /// account label, secret). Split out of [`Self::draw`].
    fn draw_manual_fields(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // "Or enter details" divider: its rule sits on the centre of the
        // divider band `layout` reserved above the issuer label.
        let m = self.metrics();
        let sp = &theme.tokens.spacing;
        let div_y = self.issuer_rect.y0
            - m.label_gap(f64::from(sp.s1))
            - m.divider_block_h(f64::from(sp.s2)) / 2.0;
        let t = crate::i18n::t;
        self.draw_divider(
            scene,
            transform,
            text_renderer,
            theme,
            div_y,
            &t("totp-add-divider"),
        );

        // Manual fields.
        self.draw_text_field(
            scene,
            transform,
            text_renderer,
            theme,
            &t("totp-add-label-issuer"),
            Field::Issuer,
        );
        self.draw_text_field(
            scene,
            transform,
            text_renderer,
            theme,
            &t("totp-add-label-account"),
            Field::Label,
        );
        self.draw_text_field(
            scene,
            transform,
            text_renderer,
            theme,
            &t("totp-add-label-secret"),
            Field::Secret,
        );
    }

    /// The Advanced-options disclosure header and, when open, the
    /// algorithm/digits chip rows plus the period field. Split out of
    /// [`Self::draw`].
    fn draw_advanced_section(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_advanced_header(scene, transform, text_renderer, theme);
        if !self.advanced_open {
            return;
        }
        Self::draw_field_label(
            scene,
            transform,
            text_renderer,
            theme,
            &crate::i18n::t("totp-add-label-algorithm"),
            self.alg_rects[0],
        );
        let algorithm_labels: Vec<&str> = Algorithm::ALL
            .iter()
            .map(|algorithm| algorithm.as_str())
            .collect();
        let selected = Algorithm::ALL
            .iter()
            .position(|algorithm| *algorithm == self.algorithm)
            .unwrap_or(0);
        let hovered = self
            .alg_rects
            .iter()
            .position(|rect| rect.contains(self.last_mouse));
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &SegmentedFace::new(&self.alg_rects, &algorithm_labels, selected)
                .pill()
                .hovered(hovered),
        );
        Self::draw_field_label(
            scene,
            transform,
            text_renderer,
            theme,
            &crate::i18n::t("totp-add-label-digits"),
            self.digit_rects[0],
        );
        let digit_labels = ["6", "8"];
        let selected = usize::from(self.digits == 8);
        let hovered = self
            .digit_rects
            .iter()
            .position(|rect| rect.contains(self.last_mouse));
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &SegmentedFace::new(&self.digit_rects, &digit_labels, selected)
                .pill()
                .hovered(hovered),
        );
        Self::draw_field_label(
            scene,
            transform,
            text_renderer,
            theme,
            &crate::i18n::t("totp-add-label-period"),
            self.period_rect,
        );
        self.period
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// The scroll hint, the inline error line and the Cancel/Add-account
    /// footer buttons. Split out of [`Self::draw`].
    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // A hint that there is more below when the body is scrollable.
        if self.max_scroll > 0.5 && self.scroll < self.max_scroll - 0.5 {
            self.draw_scroll_hint(scene, transform, theme);
        }

        // Error line, its measured box one `s1` above the footer buttons.
        if !self.error.is_empty() {
            let left = self.modal.x0 + PAD;
            let width = (self.modal.width() - PAD * 2.0) as f32;
            let (_, eh) = text_renderer.measure(&self.error, LABEL_SIZE, Some(width));
            text_renderer.draw_text(
                scene,
                &self.error,
                LABEL_SIZE,
                theme.tokens.colors.semantic.danger,
                transform
                    * Affine::translate((
                        left,
                        self.cancel_rect.y0 - f64::from(eh) - f64::from(theme.tokens.spacing.s1),
                    )),
                Some(width),
            );
        }

        // Footer.
        let can_save = !self.secret.value().trim().is_empty();
        let cancel_hover = self.cancel_rect.contains(self.last_mouse);
        let save_hover = self.save_rect.contains(self.last_mouse);
        // The shared face resolves catalogue keys itself, so the keys are
        // handed over (a resolved string would be translated a second time
        // and, under the pseudo-locale, expanded twice).
        Self::draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            self.cancel_rect,
            "totp-add-cancel",
            cancel_hover,
            true,
        );
        Self::draw_primary_button(
            scene,
            transform,
            text_renderer,
            theme,
            self.save_rect,
            self.save_label_key(),
            save_hover,
            can_save,
        );
    }

    /// One big, tappable method row: an icon tile, a bold title, and a calm
    /// one-line description (the short one when `tight`). The primary card
    /// (QR) gets the accent tint so the recommended path reads first.
    fn draw_method_card(&self, ctx: &mut PaintCtx<'_>, method: ImportMethod, tight: bool) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let (rect, primary) = match method {
            ImportMethod::Image => (self.image_card_rect, true),
            ImportMethod::Link => (self.link_card_rect, false),
        };
        let hovered = rect.contains(self.last_mouse);
        let (title, subtitle) = method.copy(tight);
        let radius = f64::from(theme.tokens.radii.lg);
        let bg = if primary {
            if hovered {
                theme.accent_hover
            } else {
                theme.accent_primary
            }
        } else if hovered {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.base
        };
        if primary {
            // The accent fill is its own contrast step; no edge (HIG §9.5).
            scene.fill(
                Fill::NonZero,
                transform,
                bg,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        } else {
            // A base-filled card on the raised modal: fill + derived edge.
            ui::surface::card(scene, transform, rect, radius, bg);
        }

        // Ink on the accent card is resolved against that fill (the accent is
        // themeable, so literal white is not always readable on it).
        let on_primary = primary;
        let on_accent = theme.on_surface(bg);
        let title_color = if on_primary {
            on_accent.ink
        } else {
            theme.tokens.colors.text_primary
        };
        let sub_color = if on_primary {
            on_accent.ink_dim
        } else {
            theme.tokens.colors.text_dim
        };
        let icon_color = if on_primary {
            on_accent.ink
        } else {
            theme.accent_primary
        };

        // Icon tile on the left: the large icon token plus its `s2` breathing
        // room, scaled with the text so it keeps pace with the card's lines.
        let tile_side = (f64::from(theme.tokens.metrics.icon_lg)
            + f64::from(theme.tokens.spacing.s2))
            * f64::from(theme.text_scale).max(1.0);
        let icon_tile = Rect::new(
            rect.x0 + 14.0,
            rect.center().y - tile_side / 2.0,
            rect.x0 + 14.0 + tile_side,
            rect.center().y + tile_side / 2.0,
        );
        let tile_bg = if on_primary {
            on_accent.ink.with_alpha_factor(0.18)
        } else {
            theme.accent_primary.with_alpha_factor(0.14)
        };
        scene.fill(
            Fill::NonZero,
            transform,
            tile_bg,
            None,
            &RoundedRect::from_rect(icon_tile, f64::from(theme.tokens.radii.md)),
        );
        draw_glyph(scene, transform, icon_tile, method, icon_color);

        // Title + description as one block centred on the card, from their
        // measured line boxes, so at a larger text scale the description sits
        // under the title instead of on top of it.
        // Both lines stay single lines — the card's height was measured for
        // one line each — and are elided to the width left of the chevron,
        // so neither can run under it (the caller already prefers the short
        // description when the scaled text is tight).
        let text_x = icon_tile.x1 + 14.0;
        let text_w = (rect.x1 - text_x - 28.0).max(20.0) as f32;
        let title = text_renderer.elide_to_width(&title, CARD_TITLE_SIZE, text_w);
        let subtitle = text_renderer.elide_to_width(&subtitle, CARD_SUB_SIZE, text_w);
        let (_, th) =
            text_renderer.measure_weighted(&title, CARD_TITLE_SIZE, None, TextWeight::Semibold);
        let (_, sh) = text_renderer.measure(&subtitle, CARD_SUB_SIZE, None);
        let title_y = rect.center().y - f64::midpoint(f64::from(th), f64::from(sh));
        text_renderer.draw_text_weighted(
            scene,
            &title,
            CARD_TITLE_SIZE,
            title_color,
            transform * Affine::translate((text_x, title_y)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            &subtitle,
            CARD_SUB_SIZE,
            sub_color,
            transform * Affine::translate((text_x, title_y + f64::from(th))),
            None,
        );
        draw_card_chevron(scene, transform, theme, rect, on_primary);
    }

    /// A subtle downward chevron at the bottom of the scroll body, shown
    /// only while there is more content below the fold.
    fn draw_scroll_hint(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let cx = self.body_clip.center().x;
        let cy = self.body_clip.y1 - 7.0;
        let mut chev = BezPath::new();
        chev.move_to((cx - 6.0, cy - 3.0));
        chev.line_to((cx, cy + 3.0));
        chev.line_to((cx + 6.0, cy - 3.0));
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.tokens.colors.text_dim.with_alpha_factor(0.8),
            None,
            &chev,
        );
    }

    fn draw_advanced_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.advanced_header_rect;
        let hovered = r.contains(self.last_mouse);
        // Disclosure triangle.
        let tx = r.x0;
        let ty = r.center().y;
        let mut tri = BezPath::new();
        if self.advanced_open {
            tri.move_to((tx, ty - 4.0));
            tri.line_to((tx + 10.0, ty - 4.0));
            tri.line_to((tx + 5.0, ty + 4.0));
        } else {
            tri.move_to((tx + 1.0, ty - 5.0));
            tri.line_to((tx + 9.0, ty));
            tri.line_to((tx + 1.0, ty + 5.0));
        }
        tri.close_path();
        let color = if hovered {
            theme.tokens.colors.text_primary
        } else {
            theme.tokens.colors.text_dim
        };
        scene.fill(Fill::NonZero, transform, color, None, &tri);
        let label = crate::i18n::t("totp-add-advanced");
        let (_, lh) = text_renderer.measure_weighted(&label, ADV_SIZE, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            &label,
            ADV_SIZE,
            color,
            transform * Affine::translate((r.x0 + 18.0, r.center().y - f64::from(lh) / 2.0)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_field_label(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        field_rect: Rect,
    ) {
        // The label's measured line box ends one pixel above its field (the
        // 1× relationship), whatever the text scale.
        let (_, lh) = text_renderer.measure_weighted(label, LABEL_SIZE, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            label,
            LABEL_SIZE,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((field_rect.x0, field_rect.y0 - f64::from(lh) - 1.0)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_text_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        field: Field,
    ) {
        let presence = PresenceCtx::empty();
        let rect = match field {
            Field::Issuer => self.issuer_rect,
            Field::Label => self.label_rect,
            Field::Secret => self.secret_rect,
            _ => return,
        };
        Self::draw_field_label(scene, transform, text_renderer, theme, label, rect);
        // The field paints its own placeholder (set in `new`), so the hint and
        // the typed text share one font, gutter and centre line.
        self.field_mut(field)
            .draw(scene, transform, text_renderer, theme, &presence);
    }

    fn draw_divider(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        y: f64,
        text: &str,
    ) {
        let left = self.modal.x0 + PAD;
        let right = self.modal.x1 - PAD;
        let (tw, th) = text_renderer.measure(text, DIVIDER_SIZE, None);
        let cx = f64::midpoint(left, right);
        // The rule is derived against the modal it is drawn on (HIG §9.5) and
        // stops an `s2` short of the caption on both sides.
        let line = ui::surface::edge_on(theme.tokens.colors.surfaces.raised);
        let caption_gap = f64::from(theme.tokens.spacing.s2);
        let l1 = Rect::new(left, y, cx - f64::from(tw) / 2.0 - caption_gap, y + 1.0);
        let l2 = Rect::new(cx + f64::from(tw) / 2.0 + caption_gap, y, right, y + 1.0);
        scene.fill(Fill::NonZero, transform, line, None, &l1);
        scene.fill(Fill::NonZero, transform, line, None, &l2);
        // Caption centred on the rule from its measured line box.
        text_renderer.draw_text(
            scene,
            text,
            DIVIDER_SIZE,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((cx - f64::from(tw) / 2.0, y - f64::from(th) / 2.0)),
            None,
        );
    }

    fn draw_button(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        hovered: bool,
        enabled: bool,
    ) {
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(rect, label)
                .hovered(hovered)
                .disabled(!enabled),
        );
    }

    fn draw_primary_button(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        hovered: bool,
        enabled: bool,
    ) {
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(rect, label)
                .primary()
                .hovered(hovered)
                .disabled(!enabled),
        );
    }

    // ---- events ------------------------------------------------------

    pub fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if !self.open {
            return;
        }
        // The modal is fully blocking — every event is consumed.
        *ctx.handled = true;

        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.handle_cursor_moved(event, ctx);
            return;
        }

        // Wheel scrolls the body when the form is taller than the window.
        // The next layout pass re-offsets every rect from the new scroll.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            self.handle_wheel(delta, ctx.scale_factor);
            return;
        }

        // Keyboard: Esc cancels, Enter saves, Tab cycles focus. A key this
        // doesn't recognise (e.g. a character while a field is focused)
        // falls through to the forward-to-focused-field tail below.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && self.handle_key(logical_key, ctx.modifiers.state().shift_key())
        {
            return;
        }

        // Mouse press: cards / disclosures / chips / buttons / focus.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.handle_mouse_press(event, ctx);
            return;
        }

        // Forward remaining events (chars, drag) to the focused field.
        if let Some(f) = self.focused {
            let mut throwaway = None;
            self.field_mut(f)
                .handle_event(event, &mut field_ctx(ctx, &mut throwaway));
        }
    }

    /// Pointer moves drive hover state (read in [`Self::draw`]) and must
    /// reach every text field so their `is_hovered` is set before a click
    /// lands. Split out of [`Self::handle_event`] to keep that function's
    /// line count in check.
    fn handle_cursor_moved(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return;
        };
        self.last_mouse = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        for field in [
            Field::Uri,
            Field::Issuer,
            Field::Label,
            Field::Secret,
            Field::Period,
        ] {
            let mut throwaway = None;
            self.field_mut(field)
                .handle_event(event, &mut field_ctx(ctx, &mut throwaway));
        }
    }

    /// Scroll the body by one wheel tick, clamped to the current range. Split
    /// out of [`Self::handle_event`].
    fn handle_wheel(&mut self, delta: &MouseScrollDelta, scale_factor: f64) {
        if self.max_scroll <= 0.0 {
            return;
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 40.0,
            MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
        };
        self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll);
    }

    /// Esc cancels, Enter saves, Tab cycles focus. Returns `true` when the
    /// key was one of those three (fully consumed), `false` otherwise so the
    /// caller falls through to the focused-field tail. Split out of
    /// [`Self::handle_event`].
    fn handle_key(&mut self, logical_key: &Key, shift: bool) -> bool {
        match logical_key {
            Key::Named(NamedKey::Escape) => {
                self.event = Some(AddPanelEvent::Cancel);
                self.open = false;
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.try_save();
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.focus_next(shift);
                true
            }
            _ => false,
        }
    }

    /// A left click: method cards, disclosures, chips, footer buttons, and
    /// focusing a text field. Split out of [`Self::handle_event`] to keep
    /// that function's line count in check.
    fn handle_mouse_press(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let p = self.last_mouse;

        // Method cards.
        if self.image_card_rect.contains(p) {
            // The app routes to the host portal chooser + runs the decode.
            self.event = Some(AddPanelEvent::PickImageForQr);
            return;
        }
        if self.link_card_rect.contains(p) {
            self.expand_uri();
            return;
        }
        if self.uri_expanded && self.parse_rect.contains(p) {
            self.parse_uri_field();
            return;
        }
        // Advanced disclosure header.
        if self.advanced_header_rect.contains(p) {
            self.toggle_advanced();
            return;
        }
        // Footer.
        if self.cancel_rect.contains(p) {
            self.event = Some(AddPanelEvent::Cancel);
            self.open = false;
            return;
        }
        if self.save_rect.contains(p) {
            self.try_save();
            return;
        }
        // Advanced chips (only hittable when open — rects are ZERO otherwise).
        if self.advanced_open {
            for (alg, rect) in Algorithm::ALL.iter().zip(self.alg_rects.iter()) {
                if rect.contains(p) {
                    self.algorithm = *alg;
                    return;
                }
            }
            for (d, rect) in [6u8, 8u8].iter().zip(self.digit_rects.iter()) {
                if rect.contains(p) {
                    self.digits = *d;
                    return;
                }
            }
        }
        // Focus a text field if clicked.
        for field in self.field_order() {
            let r = self.field_rect(field);
            if r.contains(p) {
                self.set_focus(Some(field));
                let mut throwaway = None;
                self.field_mut(field)
                    .handle_event(event, &mut field_ctx(ctx, &mut throwaway));
                return;
            }
        }
        // Click elsewhere — drop text focus, keep modal open.
        self.set_focus(None);
    }

    fn field_rect(&self, f: Field) -> Rect {
        match f {
            Field::Uri => self.uri_rect,
            Field::Issuer => self.issuer_rect,
            Field::Label => self.label_rect,
            Field::Secret => self.secret_rect,
            Field::Period => self.period_rect,
        }
    }

    /// Test hook: run the exact production save path (validate → emit event).
    #[cfg(test)]
    pub(crate) fn save_for_test(&mut self) {
        self.try_save();
    }

    /// Test hook: current (issuer, label, secret) field values.
    #[cfg(test)]
    pub(crate) fn field_snapshot(&self) -> (String, String, String) {
        (
            self.issuer.value().to_string(),
            self.label.value().to_string(),
            self.secret.value().to_string(),
        )
    }
}

/// The two ways to add an account, each drawn as a method card. Their glyphs
/// are vector paths (no font/icon dependency) so they track the accent colour
/// the theme hands us.
#[derive(Clone, Copy)]
enum ImportMethod {
    /// Scan a QR image — the recommended path, so its card is accented.
    Image,
    /// Paste a setup link.
    Link,
}

impl ImportMethod {
    /// The card's title and description; `tight` picks the short description.
    fn copy(self, tight: bool) -> (String, String) {
        let (title, description) = match (self, tight) {
            (Self::Image, false) => ("totp-add-card-scan-title", "totp-add-card-scan-long"),
            (Self::Image, true) => ("totp-add-card-scan-title", "totp-add-card-scan-short"),
            (Self::Link, false) => ("totp-add-card-link-title", "totp-add-card-link-long"),
            (Self::Link, true) => ("totp-add-card-link-title", "totp-add-card-link-short"),
        };
        (crate::i18n::t(title), crate::i18n::t(description))
    }
}

/// The chevron affordance at the right of a method card; `on_primary` inks it
/// for the accent card.
fn draw_card_chevron(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    on_primary: bool,
) {
    let cx = rect.x1 - 22.0;
    let cy = rect.center().y;
    let mut chev = BezPath::new();
    chev.move_to((cx - 4.0, cy - 6.0));
    chev.line_to((cx + 4.0, cy));
    chev.line_to((cx - 4.0, cy + 6.0));
    let chev_color = if on_primary {
        theme
            .tokens
            .colors
            .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
    } else {
        theme.tokens.colors.text_dim
    };
    scene.stroke(&Stroke::new(2.0), transform, chev_color, None, &chev);
}

/// Draw a method-card glyph centred in `tile`, in `color`.
fn draw_glyph(scene: &mut Scene, transform: Affine, tile: Rect, glyph: ImportMethod, color: Color) {
    let c = tile.center();
    match glyph {
        ImportMethod::Image => {
            // Picture frame + sun + mountain.
            let frame = RoundedRect::new(c.x - 8.0, c.y - 7.0, c.x + 8.0, c.y + 7.0, 2.0);
            scene.stroke(&Stroke::new(1.6), transform, color, None, &frame);
            let sun = vello::kurbo::Circle::new((c.x - 3.0, c.y - 2.5), 1.8);
            scene.fill(Fill::NonZero, transform, color, None, &sun);
            let mut mtn = BezPath::new();
            mtn.move_to((c.x - 7.0, c.y + 6.0));
            mtn.line_to((c.x - 1.0, c.y - 0.5));
            mtn.line_to((c.x + 2.5, c.y + 3.0));
            mtn.line_to((c.x + 5.0, c.y + 0.5));
            mtn.line_to((c.x + 7.0, c.y + 6.0));
            scene.stroke(&Stroke::new(1.6), transform, color, None, &mtn);
        }
        ImportMethod::Link => {
            // Two interlocking link halves.
            let stroke = Stroke::new(1.8);
            let mut a = BezPath::new();
            a.move_to((c.x - 1.0, c.y - 5.0));
            a.curve_to(
                (c.x - 8.0, c.y - 5.0),
                (c.x - 8.0, c.y + 1.0),
                (c.x - 1.0, c.y + 1.0),
            );
            scene.stroke(&stroke, transform, color, None, &a);
            let mut b = BezPath::new();
            b.move_to((c.x + 1.0, c.y + 5.0));
            b.curve_to(
                (c.x + 8.0, c.y + 5.0),
                (c.x + 8.0, c.y - 1.0),
                (c.x + 1.0, c.y - 1.0),
            );
            scene.stroke(&stroke, transform, color, None, &b);
            let bar = Rect::new(c.x - 3.0, c.y - 1.0, c.x + 3.0, c.y + 1.0);
            scene.fill(Fill::NonZero, transform, color, None, &bar);
        }
    }
}

/// `ctx` for one of the panel's text fields. The modal decides focus itself,
/// so a field's focus request lands in `focus` and goes no further.
fn field_ctx<'b>(ctx: &'b mut EventCtx<'_>, focus: &'b mut Option<NodeId>) -> EventCtx<'b> {
    EventCtx {
        scale_factor: ctx.scale_factor,
        modifiers: ctx.modifiers,
        state: &mut *ctx.state,
        path: ctx.path,
        handled: &mut *ctx.handled,
        focus_target: focus,
        node_id: ctx.node_id,
        taffy: ctx.taffy,
    }
}
#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use taffy::TaffyTree;

    fn opened() -> AddAccountPanel {
        let mut p = AddAccountPanel::new();
        p.open();
        p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        p
    }

    /// Add-panel metrics at 1x and scaled text.
    #[test]
    fn add_panel_metrics_reproduce_and_scale_the_authored_bands() {
        // Case `one_x_metrics_reproduce_the_authored_bands`.
        {
            // The pre-paint rule set (also what these tests lay out with) keeps
            // every 1× band at its authored size, so nothing moves at 1×.
            let m = PanelMetrics::default();
            assert_eq!(m.input_h(12.0), INPUT_H);
            assert_eq!(m.card_h(8.0), CARD_MIN_H);
            assert_eq!(m.adv_header_h(4.0), ADV_HEADER_MIN_H);
            assert_eq!(m.footer_h(12.0), FOOTER_MIN_H);
            assert_eq!(m.divider_block_h(8.0), DIVIDER_BLOCK_MIN_H);
            // The label band is its 15 px line plus `s1`; the title block is
            // `s6` + title + subtitle + `s2` (one and two pixels over the old
            // fixed bands, so the labels no longer touch their fields).
            assert_eq!(m.label_gap(4.0), 19.0);
            assert!((m.title_block_h(24.0, 8.0) - 75.75).abs() < 1e-9);
            // The 1× footer is the authored one: two equal 140 px buttons.
            let p = opened();
            assert_eq!(p.save_rect.width(), BUTTON_MAX_W);
            assert_eq!(p.cancel_rect.width(), BUTTON_MAX_W);
            assert_eq!(p.cancel_rect.x1 + 12.0, p.save_rect.x0);
        }

        // Case `scaled_metrics_grow_every_band_and_the_form_still_lays_out`.
        {
            // 200% text: every line box doubles; the bands follow instead of the
            // labels painting over the fields (the defect at 200% before).
            let m = PanelMetrics {
                scale: 2.0,
                title_h: 55.0,
                subtitle_h: 65.0, // wrapped onto two lines
                label_h: 30.0,
                card_title_h: 37.5,
                card_sub_h: 30.0,
                divider_h: 27.5,
                adv_h: 32.5,
                input_line_h: 35.0,
                button_label_h: 35.0,
                cancel_label_w: 92.0,
                save_label_w: 168.0,
                fill_label_w: 40.0,
                ..PanelMetrics::default()
            };
            assert!(m.input_h(12.0) > INPUT_H);
            assert!(m.card_h(8.0) > CARD_MIN_H);
            assert!(
                m.label_gap(4.0) > m.label_h,
                "a label never overlaps its field"
            );
            assert!(m.title_block_h(24.0, 8.0) >= 24.0 + m.title_h + m.subtitle_h + 8.0);

            let mut p = AddAccountPanel::new();
            p.open();
            p.metrics = Some(m);
            p.layout(Rect::new(0.0, 0.0, 800.0, 900.0));
            // Fields stack below their label bands without overlapping.
            assert!(p.label_rect.y0 >= p.issuer_rect.y1 + m.label_gap(4.0));
            assert!(p.secret_rect.y0 >= p.label_rect.y1 + m.label_gap(4.0));
            // The footer pair shares one width, grown past the authored 140 px
            // for the wider label (168 + 2 × 24 = 216), and stays inside.
            assert_eq!(p.save_rect.width(), p.cancel_rect.width());
            assert!(
                p.save_rect.width() >= (m.save_label_w + 48.0).min((p.modal.width() - 68.0) / 2.0)
            );
            assert!(p.save_rect.width() > BUTTON_MAX_W);
            assert!(p.cancel_rect.x1 <= p.save_rect.x0);
            assert!(p.save_rect.y1 <= p.modal.y1);
            // The body scrolls instead of the modal overflowing the window.
            assert!(p.modal.y1 <= 900.0);
        }
    }

    /// Advanced and paste-link disclosures: default state, reveal, scroll-into-view and field order.
    #[test]
    fn add_panel_disclosures_start_collapsed_and_reveal_on_demand() {
        // Case `advanced_is_collapsed_by_default`.
        {
            let p = opened();
            assert!(!p.advanced_open, "advanced options must start hidden");
            // Advanced rects are not laid out while collapsed.
            assert_eq!(p.period_rect, Rect::ZERO);
            assert_eq!(p.alg_rects[0], Rect::ZERO);
        }

        // Case `paste_link_is_collapsed_until_chosen`.
        {
            let mut p = opened();
            assert!(!p.uri_expanded);
            assert_eq!(p.uri_rect, Rect::ZERO);
            p.expand_uri();
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            assert!(p.uri_expanded);
            assert_ne!(p.uri_rect, Rect::ZERO, "uri field lays out once disclosed");
        }

        // Case `toggling_advanced_lays_out_the_fields`.
        {
            let mut p = opened();
            p.toggle_advanced();
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            assert!(p.advanced_open);
            assert_ne!(p.period_rect, Rect::ZERO);
            assert_ne!(p.alg_rects[0], Rect::ZERO);
        }

        // Case `revealing_advanced_scrolls_its_fields_into_view`.
        {
            // In a short window the expanded Advanced section is taller than the
            // fold; toggling it open must auto-scroll so the last field lands
            // inside the visible body band instead of below it.
            let win = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut p = AddAccountPanel::new();
            p.open();
            p.layout(win);
            p.toggle_advanced();
            p.layout(win);
            assert!(p.advanced_open);
            assert!(p.max_scroll > 0.0, "advanced form overflows a 600px window");
            assert_eq!(
                p.scroll, p.max_scroll,
                "toggle scrolls the new fields into view"
            );
            assert_ne!(p.period_rect, Rect::ZERO, "period field is laid out");
            assert!(
                p.period_rect.y1 <= p.body_clip.y1 + 1.0,
                "the last advanced field is within the visible body, not below the fold"
            );
            assert!(
                p.period_rect.y0 >= p.body_clip.y0 - 1.0,
                "the period field is not clipped above the body top"
            );
        }

        // Case `revealing_advanced_is_a_one_shot_that_survives_resize`.
        {
            // After the reveal-scroll, a later relayout (e.g. a resize) must not
            // re-force the scroll — the user keeps control of their position.
            let win = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut p = AddAccountPanel::new();
            p.open();
            p.toggle_advanced();
            p.layout(win);
            assert!(!p.reveal_advanced, "the reveal flag is consumed by layout");
            // Scroll back up, then relayout: the position is preserved.
            p.scroll = 0.0;
            p.layout(win);
            assert_eq!(
                p.scroll, 0.0,
                "a plain relayout does not re-trigger the reveal"
            );
        }

        // Case `field_order_tracks_disclosure_state`.
        {
            let mut p = opened();
            // Collapsed: just the three manual fields.
            assert_eq!(
                p.field_order(),
                vec![Field::Issuer, Field::Label, Field::Secret]
            );
            p.expand_uri();
            p.toggle_advanced();
            assert_eq!(
                p.field_order(),
                vec![
                    Field::Uri,
                    Field::Issuer,
                    Field::Label,
                    Field::Secret,
                    Field::Period
                ]
            );
        }
    }

    /// The modal fits tall windows, scrolls in short ones and reflows on every layout.
    #[test]
    fn add_panel_fits_scrolls_and_reflows_with_the_window() {
        // Case `default_form_fits_a_tall_window_without_scrolling`.
        {
            let mut p = AddAccountPanel::new();
            p.open();
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            assert_eq!(
                p.max_scroll, 0.0,
                "the calm default form fits without scrolling"
            );
            assert!(
                p.save_rect.y1 <= p.modal.y1,
                "footer stays inside the modal"
            );
            assert!(p.modal.y1 <= 800.0 - 16.0, "modal fits inside the window");
        }

        // Case `tall_form_scrolls_in_a_short_window_without_overflowing`.
        {
            let win = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut p = AddAccountPanel::new();
            p.open();
            p.toggle_advanced(); // makes the form taller than a 600px window
            p.layout(win);
            assert!(
                p.max_scroll > 0.0,
                "advanced form must scroll in a short window"
            );
            // The footer is pinned inside the modal, never off-screen.
            assert!(p.save_rect.y1 <= p.modal.y1);
            assert!(p.modal.y1 <= 600.0);
            // Reset to the top of the body (revealing Advanced auto-scrolled to
            // the bottom), so we can prove the scroll mechanic independently.
            p.scroll = 0.0;
            p.layout(win);
            let period_top_before = p.period_rect.y0;
            // Scrolling to the bottom brings the period field up into the body.
            p.scroll = p.max_scroll;
            p.layout(win);
            assert!(
                p.period_rect.y0 < period_top_before,
                "scrolling moves body content upward into view"
            );
            // Fully scrolled, the last field sits within the visible body band.
            assert!(
                p.period_rect.y1 <= p.body_clip.y1 + 1.0,
                "bottom of form reachable"
            );
        }

        // Case `modal_reflows_from_the_current_window_on_every_layout`.
        {
            // Guards against caching the size from the first frame: the same
            // instance must re-derive width + scroll when the window changes.
            let mut p = AddAccountPanel::new();
            p.open();
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            let wide_w = p.modal.width();
            assert_eq!(p.max_scroll, 0.0, "roomy window needs no scroll");
            p.layout(Rect::new(0.0, 0.0, 400.0, 420.0));
            assert!(
                p.modal.width() < wide_w,
                "modal narrows to the smaller window"
            );
            assert!(p.max_scroll > 0.0, "a short window enables scrolling");
            // And back to roomy — it must recover, not stay narrow.
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            assert!((p.modal.width() - wide_w).abs() < 0.5, "modal widens back");
            assert_eq!(p.max_scroll, 0.0);
        }
    }

    /// Applying an otpauth URI, validating the secret, and saving.
    #[test]
    fn add_panel_uri_build_and_save_fill_and_validate_the_form() {
        // Case `apply_uri_fills_fields_and_reveals_nondefault_params`.
        {
            let mut p = opened();
            p.apply_uri(
            "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=45",
        );
            assert_eq!(p.issuer.value(), "Acme");
            assert_eq!(p.label.value(), "bob");
            assert_eq!(p.secret.value(), "JBSWY3DPEHPK3PXP");
            assert_eq!(p.algorithm, Algorithm::Sha256);
            assert_eq!(p.digits, 8);
            assert_eq!(p.period.value(), "45");
            // Non-default crypto params should auto-reveal Advanced for review.
            assert!(p.advanced_open, "non-default params reveal advanced");
        }

        // Case `apply_uri_keeps_advanced_collapsed_for_plain_accounts`.
        {
            let mut p = opened();
            p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP");
            assert!(!p.advanced_open, "spec-default account stays calm");
        }

        // Case `build_account_validates_secret`.
        {
            let mut p = opened();
            p.issuer.set_text("Acme");
            p.secret.set_text("not base32 !!");
            assert!(p.build_account().is_err());
            p.secret.set_text("JBSWY3DPEHPK3PXP");
            assert!(p.build_account().is_ok());
        }

        // Case `save_emits_event_and_closes`.
        {
            let mut p = opened();
            p.issuer.set_text("Acme");
            p.secret.set_text("JBSWY3DPEHPK3PXP");
            p.try_save();
            assert!(!p.is_open());
            assert!(matches!(p.take_event(), Some(AddPanelEvent::Save(_))));
        }
    }

    // ---- pointer clicks through the real `handle_event` -----------------
    // The method cards, disclosure, chips and footer buttons are only
    // reachable through `handle_event`; these drive real synthetic pointer
    // events so each control's effect is verified end to end, not by calling
    // the internal handlers directly.

    use crate::test_events::{cursor_at, left_press};

    fn dispatch(p: &mut AddAccountPanel, ev: &WindowEvent) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = crate::test_events::no_mods();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut handled = false;
        p.handle_event(
            ev,
            &mut EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Pointer clicks on the add panel's cards, chips and buttons emit the right events.
    #[test]
    fn add_panel_clicks_drive_cards_chips_fill_cancel_and_save() {
        // Case `clicking_the_scan_qr_card_emits_pick_image`.
        {
            let mut p = opened();
            let at = p.image_card_rect.center();
            dispatch(&mut p, &cursor_at(at));
            dispatch(&mut p, &left_press());
            assert!(
                matches!(p.take_event(), Some(AddPanelEvent::PickImageForQr)),
                "the QR card routes to the host image picker"
            );
        }

        // Case `clicking_the_paste_link_card_discloses_the_uri_field`.
        {
            let mut p = opened();
            assert!(!p.uri_expanded);
            let at = p.link_card_rect.center();
            dispatch(&mut p, &cursor_at(at));
            dispatch(&mut p, &left_press());
            assert!(p.uri_expanded, "the paste-link card discloses the uri row");
            assert_eq!(p.focused, Some(Field::Uri), "and focuses it");
        }

        // Case `clicking_algorithm_and_digit_chips_selects_them`.
        {
            let mut p = opened();
            p.toggle_advanced();
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            assert_eq!(p.algorithm, Algorithm::Sha1, "defaults to SHA1");
            assert_eq!(p.digits, 6, "defaults to 6 digits");
            // Click the SHA512 chip (index 2).
            let alg = p.alg_rects[2].center();
            dispatch(&mut p, &cursor_at(alg));
            dispatch(&mut p, &left_press());
            assert_eq!(
                p.algorithm,
                Algorithm::Sha512,
                "clicking a chip selects the algorithm"
            );
            // Click the 8-digit chip (index 1).
            let dig = p.digit_rects[1].center();
            dispatch(&mut p, &cursor_at(dig));
            dispatch(&mut p, &left_press());
            assert_eq!(p.digits, 8, "clicking a chip selects the digit count");
        }

        // Case `clicking_the_fill_button_parses_the_uri_field`.
        {
            let mut p = opened();
            p.expand_uri();
            p.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            p.uri
                .set_text("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP");
            let at = p.parse_rect.center();
            dispatch(&mut p, &cursor_at(at));
            dispatch(&mut p, &left_press());
            assert_eq!(
                p.issuer.value(),
                "Acme",
                "Fill parses the issuer out of the link"
            );
            assert_eq!(
                p.secret.value(),
                "JBSWY3DPEHPK3PXP",
                "Fill parses the secret"
            );
        }

        // Case `clicking_cancel_emits_cancel_and_closes`.
        {
            let mut p = opened();
            let at = p.cancel_rect.center();
            dispatch(&mut p, &cursor_at(at));
            dispatch(&mut p, &left_press());
            assert!(!p.is_open(), "Cancel closes the modal");
            assert!(matches!(p.take_event(), Some(AddPanelEvent::Cancel)));
        }

        // Case `clicking_save_with_valid_fields_emits_save`.
        {
            let mut p = opened();
            p.issuer.set_text("Acme");
            p.secret.set_text("JBSWY3DPEHPK3PXP");
            let at = p.save_rect.center();
            dispatch(&mut p, &cursor_at(at));
            dispatch(&mut p, &left_press());
            assert!(!p.is_open(), "a valid Save closes the modal");
            assert!(matches!(p.take_event(), Some(AddPanelEvent::Save(_))));
        }
    }
}
