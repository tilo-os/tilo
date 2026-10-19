//! "Choose who you are here": the identity picker, a floating panel over the
//! workspace that has no identity selected (or whose identity the person
//! changes, from the palette's "Use identity in this workspace").
//!
//! Its header is the workspace's name. Its body lists the person's
//! identities, each a row with their face in its colour ring, their name and
//! where else they are used ("Used in Home and 2 other workspaces"); Enter or
//! a click chooses one. A fingerprint is never what a person types: it shows
//! only behind a row's "Details". The footer's "Create an identity" opens a
//! form in place, a name and a colour (the person's own by default), which
//! creates the identity and chooses it. With no identities at all the panel
//! opens straight on that form.
//!
//! The component owns presentation, input and its painted geometry; the host
//! carries out the [`PickerRequest`]s it emits (choosing runs the same step
//! as `identity.select`) and says how they went ([`IdentityPicker::failed`],
//! [`IdentityPicker::close`]). It comes and goes on the standard
//! [`ui::motion::Appear`] motion, on the design system's
//! [`ui::widget::FloatingPanelFace`] and [`ui::widget::ListRowFace`].

use tilo_desktop::text_entry::{self, Mods};
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::num::f32_of_f64;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::tokens::PresenceHue;
use ui::widget::{
    AvatarFace, ButtonFace, ButtonRole, ControlSize, FieldFace, FloatingPanelFace, ListRowFace,
    RowLeading, TextInput, Widget as _,
};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::shell::i18n::{t, t_args};

/// Assistive targets this panel publishes start with this.
const TARGET_PREFIX: &str = "identity-picker:";
/// The panel's width.
const WIDTH: f64 = 440.0;
/// The tallest the panel grows.
const MAX_HEIGHT: f64 = ui::tokens::Metrics::BASE.sheet_max_height as f64;
/// One colour swatch of the form.
const SWATCH: f64 = 28.0;

/// One of the person's identities, as the picker shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerIdentity {
    /// The registry's id: the public key's hex, also its fingerprint.
    pub id: String,
    pub name: String,
    /// The identity's own colour.
    pub colour: [u8; 3],
    /// The other workspaces that use it, by name.
    pub used_in: Vec<String>,
    /// Whether this workspace uses it already.
    pub here: bool,
}

/// What the host carries out for the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerRequest {
    /// Use this identity in the workspace.
    Choose(String),
    /// Create an identity with this name and colour, then use it here.
    Create { name: String, colour: [u8; 3] },
}

/// What one event did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerOutcome {
    pub consumed: bool,
    pub request: Option<PickerRequest>,
}

impl PickerOutcome {
    fn consumed() -> Self {
        Self {
            consumed: true,
            request: None,
        }
    }

    fn request(request: PickerRequest) -> Self {
        Self {
            consumed: true,
            request: Some(request),
        }
    }
}

/// Which page the panel shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    List,
    Create,
}

/// Something the panel paints that a press or an assistive action lands on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// The `n`th identity's row: choosing it.
    Row(usize),
    /// The `n`th identity's "Details" disclosure.
    Details(usize),
    /// "Create an identity".
    Create,
    Name,
    Colour(usize),
    CreateConfirm,
    Back,
}

/// The identity picker.
#[derive(Default)]
pub struct IdentityPicker {
    open: bool,
    appearing: ui::motion::Appearing,
    workspace: String,
    identities: Vec<PickerIdentity>,
    mode: Mode,
    /// The row the keyboard chose.
    selected: Option<usize>,
    /// The row whose details are open.
    details: Option<usize>,
    /// The first row shown.
    scroll: usize,
    name: String,
    name_selection: Selection,
    input: Option<TextInput>,
    /// The colour the form chose; the person's own until they choose.
    colour: Option<PresenceHue>,
    /// The palette's colours as last painted, in [`PresenceHue::ALL`] order.
    palette: Vec<[u8; 3]>,
    /// Why the last request failed, or what the form needs.
    note: Option<String>,
    /// A request is on its way: the panel takes no second one.
    busy: bool,
    rect: Rect,
    pose: Option<ui::motion::Pose>,
    targets: Vec<(Target, Rect)>,
}

/// Where else `identity` is used, in a line.
#[must_use]
pub fn used_in_line(identity: &PickerIdentity) -> String {
    match identity.used_in.as_slice() {
        [] if identity.here => t("identity-picker-used-here"),
        [] => t("identity-picker-used-nowhere"),
        [only] => t_args(
            "identity-picker-used-in-one",
            &[("first", only.clone().into())],
        ),
        [first, rest @ ..] => t_args(
            "identity-picker-used-in-more",
            &[
                ("first", first.clone().into()),
                ("count", rest.len().into()),
            ],
        ),
    }
}

fn colour_of(rgb: [u8; 3]) -> Color {
    Color::from_rgb8(rgb[0], rgb[1], rgb[2])
}

fn rgb_of(colour: Color) -> [u8; 3] {
    let rgba = colour.to_rgba8();
    [rgba.r, rgba.g, rgba.b]
}

impl IdentityPicker {
    /// Open the picker for the workspace named `workspace`, listing
    /// `identities`; with none, it opens on the form that creates one.
    pub fn open(&mut self, workspace: &str, identities: Vec<PickerIdentity>) {
        self.open = true;
        self.appearing.show(true);
        self.workspace = workspace.to_owned();
        self.mode = if identities.is_empty() {
            Mode::Create
        } else {
            Mode::List
        };
        self.selected = (!identities.is_empty()).then_some(0);
        self.identities = identities;
        self.details = None;
        self.scroll = 0;
        self.set_name(String::new());
        self.colour = None;
        self.note = None;
        self.busy = false;
    }

    /// Close it; it leaves on its motion and takes no more input.
    pub fn close(&mut self) {
        self.open = false;
        self.busy = false;
        self.appearing.show(false);
    }

    /// The host could not carry out the last request: say why, and take
    /// requests again.
    pub fn failed(&mut self, reason: &str) {
        self.busy = false;
        self.note = Some(t_args(
            "identity-picker-failed",
            &[("error", reason.to_owned().into())],
        ));
    }

    /// Whether it takes input.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether anything of it is drawn: open, or still leaving.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.open || self.appearing.is_drawn()
    }

    /// Whether it moves this frame.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.appearing.is_moving()
    }

    /// Whether it shows the form that creates an identity.
    #[must_use]
    pub fn is_creating(&self) -> bool {
        self.mode == Mode::Create
    }

    /// The identities it lists.
    #[must_use]
    pub fn identities(&self) -> &[PickerIdentity] {
        &self.identities
    }

    /// The colour the form creates with, as `palette` (the theme's presence
    /// colours) paints it.
    fn chosen_hue(&self, you: PresenceHue) -> PresenceHue {
        self.colour.unwrap_or(you)
    }

    /// Route one event while it is open. Every key belongs to it: the
    /// name field shares characters with chords.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        cursor: Option<Point>,
    ) -> PickerOutcome {
        if !self.open {
            return PickerOutcome::default();
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key, modifiers.state()),
            WindowEvent::KeyboardInput { .. } => PickerOutcome::consumed(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(point) = cursor.map(|point| self.to_local(point)) else {
                    return PickerOutcome::consumed();
                };
                let hit = self
                    .targets
                    .iter()
                    .rev()
                    .find(|(_, rect)| rect.area() > 0.0 && rect.contains(point))
                    .map(|(target, _)| target.clone());
                match hit {
                    Some(target) => self.activate(&target),
                    // A press outside the panel closes it, as outside any
                    // floating panel; the app keeps asking.
                    None if !self.rect.contains(point) => {
                        self.close();
                        PickerOutcome::consumed()
                    }
                    None => PickerOutcome::consumed(),
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.mode == Mode::List => {
                let down = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y < 0.0,
                    MouseScrollDelta::PixelDelta(point) => point.y < 0.0,
                };
                let last = self.identities.len().saturating_sub(1);
                self.scroll = if down {
                    self.scroll.saturating_add(1).min(last)
                } else {
                    self.scroll.saturating_sub(1)
                };
                PickerOutcome::consumed()
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => PickerOutcome::consumed(),
            _ => PickerOutcome::default(),
        }
    }

    fn handle_key(&mut self, key: &Key, mods: ModifiersState) -> PickerOutcome {
        match (self.mode, key) {
            (Mode::Create, Key::Named(NamedKey::Escape)) if !self.identities.is_empty() => {
                self.activate(&Target::Back)
            }
            (_, Key::Named(NamedKey::Escape)) => {
                self.close();
                PickerOutcome::consumed()
            }
            (Mode::List, Key::Named(NamedKey::ArrowDown)) => {
                let count = self.identities.len();
                if count > 0 {
                    self.selected = Some(self.selected.map_or(0, |at| (at + 1).min(count - 1)));
                    self.keep_selection_in_view();
                }
                PickerOutcome::consumed()
            }
            (Mode::List, Key::Named(NamedKey::ArrowUp)) => {
                self.selected = Some(self.selected.map_or(0, |at| at.saturating_sub(1)));
                self.keep_selection_in_view();
                PickerOutcome::consumed()
            }
            (Mode::List, Key::Named(NamedKey::Enter)) => match self.selected {
                Some(row) => self.activate(&Target::Row(row)),
                None => PickerOutcome::consumed(),
            },
            (Mode::Create, Key::Named(NamedKey::Enter)) => self.activate(&Target::CreateConfirm),
            (Mode::Create, _) => {
                let _ = text_entry::apply(
                    key,
                    Mods::from_state(mods),
                    &mut self.name,
                    &mut self.name_selection,
                );
                PickerOutcome::consumed()
            }
            (Mode::List, _) => PickerOutcome::consumed(),
        }
    }

    /// Text an input method committed into the name field.
    pub fn commit_text(&mut self, text: &str) -> bool {
        if !self.open || self.mode != Mode::Create {
            return false;
        }
        text_entry::commit(text, &mut self.name, &mut self.name_selection);
        true
    }

    fn set_name(&mut self, name: String) {
        self.name_selection = Selection::collapsed(name.len());
        self.name = name;
    }

    fn keep_selection_in_view(&mut self) {
        let Some(row) = self.selected else {
            return;
        };
        let rows = self
            .targets
            .iter()
            .filter(|(target, _)| matches!(target, Target::Row(_)))
            .count()
            .max(1);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    fn activate(&mut self, target: &Target) -> PickerOutcome {
        match target {
            Target::Row(row) => {
                if self.busy {
                    return PickerOutcome::consumed();
                }
                let Some(identity) = self.identities.get(*row) else {
                    return PickerOutcome::consumed();
                };
                self.selected = Some(*row);
                self.busy = true;
                self.note = None;
                PickerOutcome::request(PickerRequest::Choose(identity.id.clone()))
            }
            Target::Details(row) => {
                self.details = if self.details == Some(*row) {
                    None
                } else {
                    Some(*row)
                };
                PickerOutcome::consumed()
            }
            Target::Create => {
                self.mode = Mode::Create;
                self.note = None;
                PickerOutcome::consumed()
            }
            Target::Back => {
                if !self.identities.is_empty() {
                    self.mode = Mode::List;
                    self.note = None;
                }
                PickerOutcome::consumed()
            }
            Target::Name => PickerOutcome::consumed(),
            Target::Colour(index) => {
                self.colour = PresenceHue::ALL.get(*index).copied();
                PickerOutcome::consumed()
            }
            Target::CreateConfirm => {
                if self.busy {
                    return PickerOutcome::consumed();
                }
                let name = self.name.trim().to_owned();
                if name.is_empty() {
                    self.note = Some(t("identity-picker-name-needed"));
                    return PickerOutcome::consumed();
                }
                let slot = self.chosen_hue(PresenceHue::Blue).slot();
                let colour = self
                    .palette
                    .get(self.colour.map_or(usize::MAX, PresenceHue::slot))
                    .or_else(|| self.palette.get(slot))
                    .copied()
                    .unwrap_or([96, 128, 224]);
                self.busy = true;
                self.note = None;
                PickerOutcome::request(PickerRequest::Create { name, colour })
            }
        }
    }

    fn to_local(&self, point: Point) -> Point {
        self.pose.map_or(point, |pose| pose.to_local(point))
    }

    /// The panel's title for assistive technology and its header.
    fn title(&self) -> String {
        if self.workspace.trim().is_empty() {
            t("identity-picker-title-fallback")
        } else {
            self.workspace.clone()
        }
    }

    /// Paint the panel over `host_rect`, centred.
    pub fn draw(
        &mut self,
        host_rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.appearing.follow(self.open, theme.motion());
        self.targets.clear();
        if !self.visible() {
            self.rect = Rect::ZERO;
            self.pose = None;
            return;
        }
        self.palette = PresenceHue::ALL
            .iter()
            .map(|hue| rgb_of(theme.tokens.colors.presence.hue(*hue)))
            .collect();
        if self.colour.is_none() {
            // The person's own colour is the form's first choice.
            self.colour = Some(theme.tokens.colors.you);
        }
        let renderer_scale = text.text_scale();
        text.set_text_scale(1.0);
        let margin = f64::from(theme.tokens.spacing.s4);
        let width = WIDTH.min(host_rect.width() - 2.0 * margin).max(0.0);
        let face = self.face(theme);
        let body = self.body_height(text, theme, width);
        let height = face
            .height_for(body, theme)
            .min(MAX_HEIGHT)
            .min(host_rect.height() - 2.0 * margin)
            .max(0.0);
        let center = host_rect.center();
        self.rect = Rect::from_center_size(center, (width, height));
        let pose = self.appearing.pose(self.rect, center);
        self.pose = Some(pose);
        if let Some(drawn) = pose.begin(scene, transform, self.rect.inflate(48.0, 48.0)) {
            self.paint(scene, drawn, text, theme, &face);
            pose.end(scene);
        }
        text.set_text_scale(renderer_scale);
    }

    fn face(&self, theme: &Theme) -> FloatingPanelFace {
        let heading = f64::from(theme.text(TextRole::Heading).line_height_px());
        let control = FieldFace::height(ControlSize::Regular, theme);
        FloatingPanelFace::new().header(heading).footer(control)
    }

    fn row_face<'a>(identity: &'a PickerIdentity, line: &'a str) -> ListRowFace<'a> {
        ListRowFace::new(&identity.name)
            .secondary(line)
            .leading(RowLeading::Avatar(
                AvatarFace::new(&identity.name, &identity.id)
                    .presence(colour_of(identity.colour))
                    .presence_ring(),
            ))
    }

    fn body_height(&self, text: &mut TextRenderer, theme: &Theme, _width: f64) -> f64 {
        let gap = f64::from(theme.tokens.spacing.s2);
        let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
        let control = FieldFace::height(ControlSize::Regular, theme);
        let note = if self.note.is_some() {
            caption + gap
        } else {
            0.0
        };
        match self.mode {
            Mode::List => {
                let rows: f64 = self
                    .identities
                    .iter()
                    .enumerate()
                    .map(|(index, identity)| {
                        let line = used_in_line(identity);
                        let row = Self::row_face(identity, &line).height(text, theme);
                        row + if self.details == Some(index) {
                            caption + gap
                        } else {
                            0.0
                        }
                    })
                    .sum();
                rows + note
            }
            Mode::Create => {
                let intro = if self.identities.is_empty() {
                    caption + gap
                } else {
                    0.0
                };
                intro + (caption + gap / 2.0) * 2.0 + control + gap + SWATCH + gap + note
            }
        }
    }

    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        face: &FloatingPanelFace,
    ) {
        let (layout, theme) = face.paint(scene, transform, self.rect, 1.0, theme);
        let theme = &theme;
        let colors = &theme.tokens.colors;
        // The header: the workspace's name.
        let heading = theme.text(TextRole::Heading);
        let title =
            text.elide_to_width_style(&self.title(), f32_of_f64(layout.header.width()), &heading);
        text.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate((layout.header.x0, layout.header.y0).into()),
            None,
            &heading,
        );
        self.paint_footer(scene, transform, text, theme, layout.footer);
        match self.mode {
            Mode::List => self.paint_list(scene, transform, text, theme, layout.body),
            Mode::Create => self.paint_form(scene, transform, text, theme, layout.body),
        }
    }

    fn paint_footer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        footer: Rect,
    ) {
        let gap = f64::from(theme.tokens.spacing.s2);
        let buttons: Vec<(Target, String, ButtonRole)> = match self.mode {
            Mode::List => vec![(
                Target::Create,
                t("identity-picker-create"),
                ButtonRole::Plain,
            )],
            Mode::Create => {
                let mut buttons = Vec::new();
                if !self.identities.is_empty() {
                    buttons.push((Target::Back, t("identity-picker-back"), ButtonRole::Plain));
                }
                buttons.push((
                    Target::CreateConfirm,
                    t("identity-picker-create-confirm"),
                    ButtonRole::Primary,
                ));
                buttons
            }
        };
        // The buttons stand at the footer's end, the primary one last.
        let mut x = footer.x1;
        for (target, label, role) in buttons.into_iter().rev() {
            let width = ButtonFace::new(Rect::ZERO, &label)
                .role(role)
                .intrinsic_width(text, theme)
                .min(footer.width());
            let rect = Rect::new(x - width, footer.y0, x, footer.y1);
            let face = ButtonFace::new(rect, &label)
                .role(role)
                .disabled(self.busy && target == Target::CreateConfirm);
            ui::widget::Button::paint(scene, transform, text, theme, &face);
            self.targets.push((target, rect));
            x = rect.x0 - gap;
        }
    }

    fn paint_note(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        at: Point,
        width: f64,
    ) -> f64 {
        let Some(note) = self.note.as_deref() else {
            return 0.0;
        };
        let caption = theme.text(TextRole::Caption);
        let shown = text.elide_to_width_style(note, f32_of_f64(width), &caption);
        text.draw_style(
            scene,
            &shown,
            theme.tokens.colors.semantic.danger,
            transform.pre_translate(at.to_vec2()),
            None,
            &caption,
        );
        f64::from(caption.line_height_px()) + f64::from(theme.tokens.spacing.s2)
    }

    fn paint_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
    ) {
        let gap = f64::from(theme.tokens.spacing.s2);
        let caption = theme.text(TextRole::Caption);
        let mut y = body.y0;
        y += self.paint_note(
            scene,
            transform,
            text,
            theme,
            Point::new(body.x0 + gap, y),
            body.width() - 2.0 * gap,
        );
        let details_label = t("identity-picker-details");
        let identities = self.identities.clone();
        for (index, identity) in identities.iter().enumerate().skip(self.scroll) {
            let line = used_in_line(identity);
            let details_w = ButtonFace::new(Rect::ZERO, &details_label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .intrinsic_width(text, theme);
            let row_face = Self::row_face(identity, &line)
                .selected(self.selected == Some(index))
                .trailing_width(details_w);
            let height = row_face.height(text, theme);
            if y + height > body.y1 + 0.5 {
                break;
            }
            let row = Rect::new(body.x0, y, body.x1, y + height);
            let mut paint = PaintCtx::new(scene, transform, text, theme);
            row_face.paint(&mut paint, row);
            self.targets.push((Target::Row(index), row));
            if let Some(trailing) = row_face.trailing_rect(row, text, theme) {
                let control = FieldFace::height(ControlSize::Small, theme);
                let rect = Rect::new(
                    trailing.x1 - details_w,
                    trailing.center().y - control / 2.0,
                    trailing.x1,
                    trailing.center().y + control / 2.0,
                );
                ui::widget::Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(rect, &details_label)
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Small),
                );
                self.targets.push((Target::Details(index), rect));
            }
            y = row.y1;
            if self.details == Some(index) {
                // The fingerprint, to compare with another device's: read
                // here, never typed.
                let fingerprint = t_args(
                    "identity-picker-fingerprint",
                    &[("fingerprint", identity.id.clone().into())],
                );
                let shown = text.elide_to_width_style(
                    &fingerprint,
                    f32_of_f64(body.width() - 2.0 * gap),
                    &caption,
                );
                text.draw_style(
                    scene,
                    &shown,
                    theme.tokens.colors.text_dim,
                    transform.pre_translate((body.x0 + gap, y).into()),
                    None,
                    &caption,
                );
                y += f64::from(caption.line_height_px()) + gap;
            }
        }
    }

    fn paint_form(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
    ) {
        let gap = f64::from(theme.tokens.spacing.s2);
        let caption = theme.text(TextRole::Caption);
        let line = f64::from(caption.line_height_px());
        let x = body.x0 + gap;
        let width = (body.width() - 2.0 * gap).max(0.0);
        let mut y = body.y0;
        let mut label = |text: &mut TextRenderer, scene: &mut Scene, words: &str, y: f64, ink| {
            let shown = text.elide_to_width_style(words, f32_of_f64(width), &caption);
            text.draw_style(
                scene,
                &shown,
                ink,
                transform.pre_translate((x, y).into()),
                None,
                &caption,
            );
        };
        if self.identities.is_empty() {
            label(
                text,
                scene,
                &t("identity-picker-empty-hint"),
                y,
                theme.tokens.colors.text_dim,
            );
            y += line + gap;
        }
        label(
            text,
            scene,
            &t("identity-picker-name"),
            y,
            theme.tokens.colors.text_dim,
        );
        y += line + gap / 2.0;
        let control = FieldFace::height(ControlSize::Regular, theme);
        let field = Rect::new(x, y, x + width, y + control);
        self.paint_name(scene, transform, text, theme, field);
        self.targets.push((Target::Name, field));
        y = field.y1 + gap;
        label(
            text,
            scene,
            &t("identity-picker-colour"),
            y,
            theme.tokens.colors.text_dim,
        );
        y += line + gap / 2.0;
        let chosen = self.chosen_hue(theme.tokens.colors.you);
        let step = SWATCH + gap;
        for (index, hue) in PresenceHue::ALL.iter().enumerate() {
            let left = x + step * f64::from(u8::try_from(index).unwrap_or(u8::MAX));
            if left + SWATCH > x + width {
                break;
            }
            let rect = Rect::new(left, y, left + SWATCH, y + SWATCH);
            let centre = rect.center();
            let fill = theme.tokens.colors.presence.hue(*hue);
            let selected = *hue == chosen;
            let radius = SWATCH / 2.0 - if selected { 4.0 } else { 2.0 };
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &Circle::new(centre, radius),
            );
            if selected {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    fill,
                    None,
                    &Circle::new(centre, SWATCH / 2.0 - 1.0),
                );
            }
            self.targets.push((Target::Colour(index), rect));
        }
        y += SWATCH + gap;
        let _ = self.paint_note(scene, transform, text, theme, Point::new(x, y), width);
    }

    fn paint_name(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        field: Rect,
    ) {
        let placeholder = t("identity-picker-name-placeholder");
        let face = FieldFace::new(field, &self.name)
            .placeholder(&placeholder)
            .text_role(TextRole::Body)
            .focused(self.open);
        if self.name.is_empty() {
            face.paint(scene, transform, text, theme);
            return;
        }
        face.paint_chrome(scene, transform, theme);
        let input = self.input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Body)
        });
        input.set_text(&self.name);
        input.set_selection(self.name_selection.anchor, self.name_selection.cursor);
        input.set_focus(self.open);
        input.apply_layout_external(face.text_rect(theme));
        let input_theme = theme
            .clone()
            .with_text_scale(theme.text_scale * theme.ui_scale);
        input.draw(
            scene,
            transform,
            text,
            &input_theme,
            &PresenceCtx::default(),
        );
    }

    /// The panel for assistive technology: a dialog named by the workspace,
    /// each identity a button that chooses it (its details a button that
    /// shows the fingerprint), the form's field and colours, the footer.
    #[must_use]
    pub fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let mut panel = AccessNode::new(Role::Dialog)
            .with_name(t_args(
                "identity-picker-dialog",
                &[("workspace", self.title().into())],
            ))
            .with_bounds(self.rect);
        if let Some(note) = &self.note {
            panel = panel.with_description(note.clone());
        }
        for (target, rect) in &self.targets {
            let node = match target {
                Target::Row(row) => {
                    let Some(identity) = self.identities.get(*row) else {
                        continue;
                    };
                    AccessNode::new(Role::Button)
                        .with_name(t_args(
                            "identity-picker-choose-named",
                            &[("name", identity.name.clone().into())],
                        ))
                        .with_description(used_in_line(identity))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.selected == Some(*row)),
                        )
                }
                Target::Details(row) => {
                    let mut node = AccessNode::new(Role::Button)
                        .with_name(t("identity-picker-details"))
                        .with_state(AccessState::new().expanded(self.details == Some(*row)));
                    if self.details == Some(*row)
                        && let Some(identity) = self.identities.get(*row)
                    {
                        node = node.with_description(identity.id.clone());
                    }
                    node
                }
                Target::Create => {
                    AccessNode::new(Role::Button).with_name(t("identity-picker-create"))
                }
                Target::Name => AccessNode::new(Role::TextInput)
                    .with_name(t("identity-picker-name"))
                    .with_value(self.name.clone()),
                Target::Colour(index) => {
                    let Some(hue) = PresenceHue::ALL.get(*index) else {
                        continue;
                    };
                    AccessNode::new(Role::RadioButton)
                        .with_name(hue.label().to_owned())
                        .with_state(AccessState::new().checked(self.colour == Some(*hue)))
                }
                Target::CreateConfirm => {
                    AccessNode::new(Role::Button).with_name(t("identity-picker-create-confirm"))
                }
                Target::Back => AccessNode::new(Role::Button).with_name(t("identity-picker-back")),
            };
            panel.children.push(
                node.with_bounds(*rect)
                    .with_action_target(format!("{TARGET_PREFIX}{}", self.slug(target))),
            );
        }
        Some(panel)
    }

    /// What a target does, as its assistive name: never its position.
    fn slug(&self, target: &Target) -> String {
        let id = |row: &usize| {
            self.identities
                .get(*row)
                .map(|identity| identity.id.clone())
                .unwrap_or_default()
        };
        match target {
            Target::Row(row) => format!("choose/{}", id(row)),
            Target::Details(row) => format!("details/{}", id(row)),
            Target::Create => "create".into(),
            Target::Name => "name".into(),
            Target::Colour(index) => format!(
                "colour/{}",
                PresenceHue::ALL.get(*index).map_or("", |hue| hue.id())
            ),
            Target::CreateConfirm => "create-confirm".into(),
            Target::Back => "back".into(),
        }
    }

    /// Carry out `action` on the target `target` names.
    pub fn perform_accessibility_action(
        &mut self,
        target: &str,
        action: &Action,
    ) -> Option<PickerOutcome> {
        let part = target.strip_prefix(TARGET_PREFIX)?;
        if !self.open {
            return None;
        }
        if part == "name" {
            return match action {
                Action::SetValue(value) => {
                    self.set_name(value.chars().filter(|c| !c.is_control()).collect());
                    Some(PickerOutcome::consumed())
                }
                Action::Focus | Action::Activate | Action::ScrollIntoView => {
                    Some(PickerOutcome::consumed())
                }
                _ => None,
            };
        }
        let row = |id: &str| {
            self.identities
                .iter()
                .position(|identity| identity.id == id)
        };
        let target = match part {
            "create" => Target::Create,
            "create-confirm" => Target::CreateConfirm,
            "back" => Target::Back,
            other => {
                if let Some(id) = other.strip_prefix("choose/") {
                    Target::Row(row(id)?)
                } else if let Some(id) = other.strip_prefix("details/") {
                    Target::Details(row(id)?)
                } else {
                    let hue = PresenceHue::from_id(other.strip_prefix("colour/")?)?;
                    Target::Colour(hue.slot())
                }
            }
        };
        match action {
            Action::Activate => Some(self.activate(&target)),
            Action::Focus | Action::ScrollIntoView => Some(PickerOutcome::consumed()),
            _ => None,
        }
    }

    /// The painted targets by name, for harnesses.
    #[must_use]
    pub fn test_rects(&self) -> Vec<(String, Rect)> {
        self.targets
            .iter()
            .map(|(target, rect)| (format!("identity-picker.{}", self.slug(target)), *rect))
            .collect()
    }
}

/// Whether `target` was published by this panel.
#[must_use]
pub fn is_accessibility_target(target: &str) -> bool {
    target.starts_with(TARGET_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(id: &str, name: &str, used_in: &[&str], here: bool) -> PickerIdentity {
        PickerIdentity {
            id: id.repeat(64 / id.len().max(1)),
            name: name.to_owned(),
            colour: [40, 120, 200],
            used_in: used_in.iter().map(|name| (*name).to_owned()).collect(),
            here,
        }
    }

    fn drawn(picker: &mut IdentityPicker) {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        picker.draw(
            Rect::new(0.0, 0.0, 1280.0, 800.0),
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
    }

    fn key(picker: &mut IdentityPicker, key: Key) -> PickerOutcome {
        picker.handle_key(&key, ModifiersState::empty())
    }

    fn named(picker: &mut IdentityPicker, target: &str) -> PickerOutcome {
        picker
            .perform_accessibility_action(&format!("{TARGET_PREFIX}{target}"), &Action::Activate)
            .expect("the panel publishes the target")
    }

    // USER REQUIREMENT (2026-10-19): "The identity chooser must never ask
    // for a typed fingerprint." "Its body is a list of the person's
    // identities as ListRows [...] Enter or click chooses." "The fingerprint
    // can appear only behind a "Details" disclosure on a row, never as the
    // input."
    #[test]
    fn the_person_chooses_an_identity_by_name_and_reads_its_fingerprint_only_in_details() {
        let mut picker = IdentityPicker::default();
        picker.open(
            "Studio",
            vec![
                identity("a", "Ada", &["Home", "Lab", "Shop"], false),
                identity("b", "Grace", &[], true),
            ],
        );
        drawn(&mut picker);
        assert!(!picker.is_creating(), "identities are listed first");
        let panel = picker.accessibility().expect("the panel is semantic");
        assert_eq!(panel.name.as_deref(), Some("Choose who you are in Studio"));
        let ada = picker.identities()[0].id.clone();
        assert!(
            panel
                .children
                .iter()
                .all(|node| node.role != Role::TextInput),
            "nothing on the list asks for typing"
        );
        let row = panel
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Use Ada here"))
            .expect("Ada's row chooses her");
        assert_eq!(
            row.description.as_deref(),
            Some("Used in Home and 2 other workspaces")
        );
        assert!(
            panel
                .children
                .iter()
                .all(|node| node.description.as_deref() != Some(ada.as_str())),
            "the fingerprint stays behind Details"
        );
        named(&mut picker, &format!("details/{ada}"));
        drawn(&mut picker);
        let panel = picker.accessibility().unwrap();
        assert!(
            panel
                .children
                .iter()
                .any(|node| node.description.as_deref() == Some(ada.as_str())),
            "Details shows the fingerprint"
        );

        assert_eq!(
            key(&mut picker, Key::Named(NamedKey::Enter)).request,
            Some(PickerRequest::Choose(ada.clone())),
            "Enter chooses the row the keyboard is on"
        );
        picker.failed("refused");
        let grace = picker.identities()[1].id.clone();
        assert_eq!(
            named(&mut picker, &format!("choose/{grace}")).request,
            Some(PickerRequest::Choose(grace))
        );
    }

    // USER REQUIREMENT (2026-10-19): "Footer: "Create an identity" opens an
    // inline form (name and colour, with a sensible default colour) and then
    // chooses it." "With no identities at all, the panel opens straight on
    // the create form."
    #[test]
    fn with_no_identity_the_panel_opens_on_creating_one_in_the_persons_colour() {
        let mut picker = IdentityPicker::default();
        picker.open("Studio", Vec::new());
        drawn(&mut picker);
        assert!(picker.is_creating(), "no identities: straight to the form");
        let panel = picker.accessibility().unwrap();
        assert!(
            panel
                .children
                .iter()
                .any(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Name")),
            "the form asks for a name"
        );
        assert_eq!(
            panel
                .children
                .iter()
                .filter(|node| node.role == Role::RadioButton)
                .count(),
            PresenceHue::ALL.len(),
            "and offers every colour"
        );
        assert!(
            panel
                .children
                .iter()
                .all(|node| node.name.as_deref() != Some("Back")),
            "there is nothing to go back to"
        );
        assert_eq!(
            named(&mut picker, "create-confirm").request,
            None,
            "a nameless identity is not created"
        );
        assert!(picker.commit_text("Ada Lovelace"));
        let you = Theme::dark().tokens.colors.you;
        let expected = rgb_of(Theme::dark().tokens.colors.presence.hue(you));
        assert_eq!(
            named(&mut picker, "create-confirm").request,
            Some(PickerRequest::Create {
                name: "Ada Lovelace".into(),
                colour: expected,
            }),
            "the person's own colour unless they choose another"
        );

        let mut picker = IdentityPicker::default();
        picker.open("Studio", vec![identity("a", "Ada", &[], false)]);
        drawn(&mut picker);
        named(&mut picker, "create");
        drawn(&mut picker);
        assert!(picker.is_creating());
        named(&mut picker, "colour/green");
        assert!(picker.commit_text("Grace"));
        let green = rgb_of(Theme::dark().tokens.colors.presence.hue(PresenceHue::Green));
        assert_eq!(
            named(&mut picker, "create-confirm").request,
            Some(PickerRequest::Create {
                name: "Grace".into(),
                colour: green,
            })
        );
        picker.failed("no keyring");
        named(&mut picker, "back");
        assert!(!picker.is_creating(), "Back returns to the list");
    }
}
