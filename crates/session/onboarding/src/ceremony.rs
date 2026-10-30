//! Setup's surface: the trusted backdrop, the card the page is composed in,
//! the slide from one step to the next and the file chooser over it.
//!
//! The page is composed again whenever it is laid out after the shell
//! changed it, at the size and in the theme it is laid out in; a short
//! display, large text and high contrast each get the room they need. The
//! card is the only thing a pointer or key reaches while the chooser is
//! closed; Escape follows the page's Back while the ceremony may still go
//! back, and skips the tour on a tour page.

use std::collections::HashMap;
use std::sync::Arc;

use ui::ColorCompat as _;
use ui::LocalState;
use ui::framework::Dispatcher;
use ui::taffy::{self, NodeId, TaffyTree};
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Circle, Point, Rect};
use ui::vello::peniko::{BlendMode, Fill};
use ui::widget::{
    AppModel, Button, Checkbox, FilePicker, FilePickerMode, ModelPath, TaffyTreeExt, Widget,
};
use ui::winit::event::{ElementState, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use crate::effect::OnboardingEffect;
use crate::page::{ChooserPurpose, Page, Screen, TourPage};
use crate::seat::{OnboardingSeat, Secrets, WordMistakes};
use crate::{Onboarding, t};

const PANEL_MAX_WIDTH: f32 = 760.0;
// Longer translated/large-text pages scroll inside this limit. Short pages
// retain their intrinsic height so setup never becomes a mostly empty modal.
const PANEL_MAX_HEIGHT: f32 = 640.0;
/// Large text and high contrast need additional vertical room before a dense
/// first-use page falls back to scrolling. The viewport wrapper still caps
/// this to its own height minus the safety gutters.
const ACCESSIBLE_PANEL_HEIGHT: f32 = 672.0;
/// Restore, protection and backup contain dense decisions and controls. Give
/// them enough preferred height to avoid needless scrolling on a wide laptop;
/// flex shrink still preserves the safety gutter on a compact display.
const DENSE_PANEL_PREFERRED_HEIGHT: f32 = 560.0;
/// The introduction's choice gets a lower cap than the other stages; it
/// still shrinks to its contents.
const INTRO_PANEL_HEIGHT: f32 = 520.0;
/// Completion is a hand-off surface, not a compact alert. Give its title,
/// confirmation, next-step explanation, and backup guidance a stable canvas
/// on ordinary laptop displays; the inner viewport still scrolls when large
/// text or a short window cannot fit that content.
const READY_PANEL_PREFERRED_HEIGHT: f32 = 552.0;
/// The formats step is a full page like the hand-off: a title, a line and one
/// large Install button; its results add a banner and a progress bar. One
/// height for all of them, so choosing does not make the card jump.
const CODECS_PANEL_PREFERRED_HEIGHT: f32 = READY_PANEL_PREFERRED_HEIGHT;
const VIEWPORT_GUTTER: f32 = 24.0;
/// A 40 px action plus the footer's 17 px bottom inset. The gap above the
/// footer belongs to the parent column and must not be reserved twice; doing
/// so made short pages acquire a pointless scrollbar and hid useful recovery
/// copy on laptop-height displays.
pub(crate) const FOOTER_HEIGHT: f32 = 57.0;
/// A labelled checkbox has a 44 px minimum target. Decision pages pin that
/// acknowledgement above the ordinary action row so large text and compact
/// displays never hide the gate inside scrolling explanatory copy.
pub(crate) const ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT: f32 = 72.0;
/// `s3` top pad, eyebrow row, gap, padded 8 px progress track, and gap below
/// the sticky header. The pad keeps the eyebrow off the panel's top edge
/// (HIG §7: nothing touches the container it sits in).
pub(crate) const PROGRESS_HEADER_HEIGHT: f32 = 70.0;

/// Setup's surface.
pub(crate) struct Ceremony {
    seat: Option<OnboardingSeat>,
    /// The revision of the seat's page shown.
    seen: u64,
    pub(crate) page: Page,
    pub(crate) secrets: Secrets,
    pub(crate) mistakes: Option<WordMistakes>,
    pub(crate) dispatch: Dispatcher<Onboarding>,
    /// The backup file to restore from, as typed here.
    pub(crate) path: LocalState<String>,
    acknowledged: LocalState<bool>,
    skip_acknowledged: LocalState<bool>,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    /// A step arriving: forward from the trailing side, back from the
    /// leading side (`ui::motion::Drill`).
    transition: ui::motion::Presence,
    transition_direction: f64,
    reduce_motion: bool,
    motion: ui::tokens::EffectiveMotion,
    chooser: Option<(ChooserPurpose, FilePicker)>,
    waker: Option<std::task::Waker>,
    structure_dirty: bool,
    focus_reset: bool,
}

struct Wake(ui::WakeHandle);

impl std::task::Wake for Wake {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }
}

impl Ceremony {
    pub(crate) fn new(
        seat: Option<OnboardingSeat>,
        page: Page,
        dispatch: Dispatcher<Onboarding>,
        wake: Option<ui::WakeHandle>,
    ) -> Self {
        let secrets = seat
            .as_ref()
            .map(OnboardingSeat::secrets)
            .unwrap_or_default();
        let mistakes = seat.as_ref().and_then(OnboardingSeat::mistakes);
        let seen = seat.as_ref().map_or(0, OnboardingSeat::revision);
        let mut ceremony = Self {
            seat,
            seen,
            path: LocalState::new(page.connect_path.clone()),
            acknowledged: LocalState::new(page.acknowledged),
            skip_acknowledged: LocalState::new(page.backup_skip_acknowledged),
            page,
            secrets,
            mistakes,
            dispatch,
            composed: None,
            rect: Rect::ZERO,
            transition: ui::motion::Presence::opened(),
            transition_direction: 1.0,
            reduce_motion: false,
            motion: ui::tokens::EffectiveMotion::default(),
            chooser: None,
            waker: wake.map(|wake| std::task::Waker::from(Arc::new(Wake(wake)))),
            structure_dirty: true,
            focus_reset: false,
        };
        ceremony.follow_chooser();
        ceremony
    }

    /// Take the page the shell shows now, if it changed.
    fn follow_seat(&mut self) {
        let Some(seat) = self.seat.clone() else {
            return;
        };
        if seat.revision() == self.seen {
            if seat.take_settle() {
                self.transition = ui::motion::Presence::opened();
            }
            return;
        }
        self.seen = seat.revision();
        let page = seat.page();
        let before = (&self.page.screen, self.page.tour_page);
        let moved = page.screen != *before.0
            || page.tour_page != before.1
            || page.identity_ready_page != self.page.identity_ready_page
            || page.codec_page != self.page.codec_page;
        if moved {
            self.transition_direction = if page.identity_ready_page != self.page.identity_ready_page
                || page.codec_page != self.page.codec_page
            {
                1.0
            } else if view_order(page.screen, page.tour_page)
                < view_order(self.page.screen, self.page.tour_page)
            {
                -1.0
            } else {
                1.0
            };
            self.transition = step_arriving(self.motion);
            self.focus_reset = true;
        }
        if page.connect_path != self.page.connect_path
            && self.path.with(|path| *path != page.connect_path)
        {
            self.path.set(page.connect_path.clone());
        }
        if self.acknowledged.get() != page.acknowledged {
            self.acknowledged.set(page.acknowledged);
        }
        if self.skip_acknowledged.get() != page.backup_skip_acknowledged {
            self.skip_acknowledged.set(page.backup_skip_acknowledged);
        }
        if seat.take_settle() {
            self.transition = ui::motion::Presence::opened();
        }
        self.page = page;
        self.follow_chooser();
        self.structure_dirty = true;
    }

    /// Open the chooser the page asks for, or close it once it no longer does.
    fn follow_chooser(&mut self) {
        let wanted = self.page.chooser.clone();
        match (&self.chooser, wanted) {
            (Some((open, _)), Some(chooser)) if *open == chooser.purpose => {}
            (_, Some(chooser)) => {
                let picker = match chooser.purpose {
                    ChooserPurpose::Restore => FilePicker::new(
                        t("onboarding-picker-import-title"),
                        &chooser.initial,
                        chooser.roots,
                    )
                    .with_filters(vec![
                        ui::widget::file_picker::FileFilter::new(
                            t("onboarding-picker-import-filter"),
                            &["tilo-identity", "tilo-identity-backup", "json"],
                        ),
                        ui::widget::file_picker::FileFilter::all(),
                    ]),
                    ChooserPurpose::Backup => FilePicker::new(
                        t("onboarding-picker-backup-title"),
                        &chooser.initial,
                        chooser.roots,
                    )
                    .with_mode(FilePickerMode::Save)
                    .with_filters(vec![ui::widget::file_picker::FileFilter::new(
                        t("onboarding-picker-backup-filter"),
                        &["tilo-identity"],
                    )])
                    .with_filename("tilo-identity-backup.tilo-identity"),
                };
                self.chooser = Some((chooser.purpose, picker.with_waker(self.waker.clone())));
                self.structure_dirty = true;
                self.focus_reset = true;
            }
            (Some(_), None) => {
                self.chooser = None;
                self.structure_dirty = true;
                self.focus_reset = true;
            }
            (None, None) => {}
        }
    }

    /// What the chooser answered: the file, or that it was closed.
    fn follow_chooser_answer(&mut self) {
        let Some((_, picker)) = self.chooser.as_mut() else {
            return;
        };
        if picker.poll_lookups() {
            self.structure_dirty = true;
        }
        let Some(outcome) = picker.take_outcome() else {
            return;
        };
        let effect = match outcome {
            ui::widget::FilePickerOutcome::Selected(paths) => {
                paths
                    .into_iter()
                    .next()
                    .map_or(OnboardingEffect::CloseChooser, |path| {
                        OnboardingEffect::Pick {
                            path: path.to_string_lossy().into_owned(),
                        }
                    })
            }
            ui::widget::FilePickerOutcome::Cancelled => OnboardingEffect::CloseChooser,
        };
        if let OnboardingEffect::Pick { path } = &effect
            && self.chooser.as_ref().map(|(purpose, _)| *purpose) == Some(ChooserPurpose::Restore)
        {
            self.path.set(path.clone());
        }
        // Closed here at once; the shell confirms with the next page.
        self.chooser = None;
        self.structure_dirty = true;
        self.focus_reset = true;
        self.dispatch.effect(&effect);
    }

    /// A button whose press asks `effect`.
    pub(crate) fn button(&self, id: &str, label: &str, effect: OnboardingEffect) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id, label).on_effect(move || dispatch.effect(&effect))
    }

    pub(crate) fn back_button(&self) -> Box<dyn Widget> {
        Box::new(
            self.button(
                "onboarding.back.button",
                &t("onboarding-back"),
                OnboardingEffect::Back,
            )
            .with_icon(ui::icons::Icon::ArrowLeft),
        )
    }

    /// The page's acknowledgement: ticking it asks the shell to record it.
    pub(crate) fn acknowledgement(&self, id: &str, label: String, _checked: bool) -> Checkbox {
        let state = if id.contains("skip") {
            &self.skip_acknowledged
        } else {
            &self.acknowledged
        };
        let dispatch = self.dispatch.clone();
        Checkbox::from_state(state)
            .with_id(id)
            .with_label(label)
            .on_view(id, move |checked| {
                dispatch.effect(&OnboardingEffect::Acknowledge { checked });
            })
    }

    pub(crate) fn path_input(&self) -> ui::widget::TextInput {
        let dispatch = self.dispatch.clone();
        ui::widget::TextInput::from_state(&self.path).on_change(move |value| {
            dispatch.effect(&OnboardingEffect::Enter {
                field: "backup-file".to_owned(),
                value: value.to_owned(),
            });
        })
    }

    fn draw_backdrop(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        if self.page.tour_page.is_some() {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[9].with_alpha_factor(
                    if theme.name.eq_ignore_ascii_case("high contrast") {
                        0.72
                    } else {
                        0.48
                    },
                ),
                None,
                &self.rect,
            );
            // Keep the real desktop legible while the tour is modal. Do not
            // paint a focus target from the covered desktop behind the card:
            // a full-width ring reads as a second, interactive surface and
            // competes with the tour's own keyboard focus.
            return;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if theme.name.eq_ignore_ascii_case("high contrast") {
            return;
        }
        // Quiet, theme-derived depth: two broad accent blooms make the
        // trusted setup surface distinct without becoming illustration.
        let bloom = colors.text_primary.with_alpha_factor(0.10);
        let radius = self.rect.width().max(self.rect.height()) * 0.34;
        scene.fill(
            Fill::NonZero,
            transform,
            bloom,
            None,
            &Circle::new(Point::new(self.rect.x0, self.rect.y0), radius),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.intent.you.with_alpha_factor(0.07),
            None,
            &Circle::new(Point::new(self.rect.x1, self.rect.y1), radius * 0.82),
        );
    }

    /// The arriving step's pose: it comes in from the side it lives on.
    fn transition_pose(&mut self, theme: &Theme) -> ui::motion::Pose {
        self.reduce_motion = theme.reduce_motion;
        // Under reduced motion a step is simply there: no drill, no fade.
        let arrived = if self.reduce_motion {
            1.0
        } else {
            self.transition.progress(ui::motion::frame_now())
        };
        ui::motion::Drill::pose(
            self.transition_direction * (1.0 - arrived),
            self.rect.width(),
            theme.motion(),
        )
    }

    fn draw_composed(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.composed.is_none() {
            return;
        }
        let pose = self.transition_pose(theme);
        let Some(composed) = self.composed.as_mut() else {
            return;
        };
        let panel = composed.get_rect();
        let on_panel = ui::chrome::shell::on_panel(theme);
        if panel.area() > 0.0 {
            // The card floats over the backdrop and casts the Panel shadow
            // every other Panel casts; on the high-contrast palette its edge
            // does the separating.
            let _ = ui::chrome::shell::paint_panel(scene, transform, panel, theme, 1.0);
        }
        let mut local = Scene::new();
        composed.draw(
            &mut local,
            Affine::IDENTITY,
            text_renderer,
            &on_panel,
            presence,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            transform,
            &self.rect,
        );
        scene.append(&local, Some(pose.applied_to(transform)));
        scene.pop_layer();
    }

    fn draw_chooser(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let Some((_, picker)) = self.chooser.as_mut() else {
            return;
        };
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.scrim.with_alpha_factor(0.56),
            None,
            &self.rect,
        );
        picker.set_rect(chooser_rect(self.rect));
        picker.draw(scene, transform, text_renderer, theme, presence);
    }

    fn escape_effect(&self) -> Option<OnboardingEffect> {
        if self.page.migration.is_some() || self.page.prepared.is_some() {
            return None;
        }
        if self.page.tour_page.is_some() {
            return Some(OnboardingEffect::Skip);
        }
        // Escape follows the visible Back while the identity ceremony is
        // still reversible; on Nearby it also ends a running pairing. Intro
        // and Done stay: setup has either no identity yet or still needs its
        // explicit hand-off into the desktop.
        (self.page.screen.is_existing_branch() || self.page.screen == Screen::NoRecovery)
            .then_some(OnboardingEffect::Back)
    }
}

impl Widget for Ceremony {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_seat();
        self.follow_chooser_answer();
        if let Some(composed) = &mut self.composed {
            composed.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.follow_seat();
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.motion();
        if self.chooser.is_some() {
            // The chooser places itself over the whole surface.
            let style = taffy::Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Default::default()
            };
            return taffy.widget_node(style, &[], node_map, self);
        }
        let accessibility_expanded =
            theme.text_scale > 1.0 || theme.name.eq_ignore_ascii_case("high contrast");
        let page = &self.page;
        let identity_page = page.tour_page.is_none() && page.migration.is_none();
        let panel_height = if accessibility_expanded {
            ACCESSIBLE_PANEL_HEIGHT
        } else if identity_page && page.screen == Screen::Intro {
            // The three ways to bring an existing identity (and recovery's
            // explanation above them) need the ordinary limit.
            INTRO_PANEL_HEIGHT
        } else {
            PANEL_MAX_HEIGHT
        };
        let ready_identity_page = identity_page
            && page.screen == Screen::Done
            && page.identity_ready_page
            && !page.codec_page;
        // The introduction's pinned cards take a fixed share of the panel, so
        // it keeps one stable height instead of measuring its explanation.
        let intro_page = identity_page && page.screen == Screen::Intro;
        let codecs_page = ready_identity_page && page.codec_page;
        let dense_identity_page = identity_page
            && (accessibility_expanded
                || matches!(
                    page.screen,
                    Screen::Nearby | Screen::Restore | Screen::RecoveryKey | Screen::NoRecovery
                )
                // Recovery explains what happened above its three ways back.
                || (page.recovery_only && page.screen == Screen::Existing)
                || (page.screen == Screen::Done
                    && (!page.identity_ready_page || page.codec_page)));
        // The scroll/footer budget uses the same preferred height as the
        // card itself, so the sticky action never lands on its scrolling
        // sibling.
        let composition_height = if ready_identity_page {
            panel_height
        } else if accessibility_expanded {
            ACCESSIBLE_PANEL_HEIGHT
        } else if dense_identity_page {
            DENSE_PANEL_PREFERRED_HEIGHT
        } else if codecs_page {
            CODECS_PANEL_PREFERRED_HEIGHT
        } else {
            panel_height
        };
        let mut composed = self.build_composition(theme, composition_height);
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // A readable measure on large displays, shrinking to a phone-like
        // one; the body scrolls when its copy is taller than the panel. The
        // width is definite so a page is measured at the width it is laid
        // out at.
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::length(PANEL_MAX_WIDTH);
            // Pages with little content own only the space they need.
            cs.size.height = if ready_identity_page {
                taffy::Dimension::auto()
            } else if accessibility_expanded {
                taffy::Dimension::length(ACCESSIBLE_PANEL_HEIGHT)
            } else if dense_identity_page {
                taffy::Dimension::length(DENSE_PANEL_PREFERRED_HEIGHT)
            } else if codecs_page {
                taffy::Dimension::length(CODECS_PANEL_PREFERRED_HEIGHT)
            } else if intro_page {
                taffy::Dimension::length(INTRO_PANEL_HEIGHT)
            } else {
                taffy::Dimension::auto()
            };
            // The automatic minimum is the content's min-height, which would
            // keep the scroll viewport from shrinking on a short display.
            cs.min_size.height = taffy::Dimension::length(0.0);
            cs.max_size.width = taffy::Dimension::percent(1.0);
            cs.max_size.height = taffy::Dimension::length(panel_height);
            cs.flex_basis = taffy::Dimension::auto();
            cs.flex_grow = 0.0;
            cs.flex_shrink = 1.0;
            let _ = taffy.set_style(child, cs);
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
            // The gutter is part of the viewport-sized surface, keeping the
            // panel inside narrow displays.
            box_sizing: taffy::BoxSizing::BorderBox,
            flex_direction: taffy::FlexDirection::Column,
            justify_content: Some(taffy::JustifyContent::Center),
            align_items: Some(taffy::AlignItems::Center),
            size: taffy::Size::from_percent(1.0, 1.0),
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                right: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                top: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                bottom: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some((_, picker)) = self.chooser.as_mut() {
            picker.set_rect(chooser_rect(self.rect));
            return;
        }
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            composed.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
            if composed.needs_animation_frame() {
                ui::widget::note_layout_moved();
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
        self.draw_backdrop(scene, transform, theme);
        if self.chooser.is_none() {
            self.draw_composed(scene, transform, text_renderer, theme, presence);
        }
        self.draw_chooser(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some((_, picker)) = self.chooser.as_mut() {
            picker.set_rect(chooser_rect(self.rect));
            let mut picker_taffy = TaffyTree::<()>::new();
            if let Ok(picker_node) = picker_taffy.new_leaf(taffy::Style::default()) {
                picker.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        taffy: &picker_taffy,
                        ..ctx.reborrow(ctx.path, picker_node)
                    },
                );
            }
            *ctx.handled = true;
            self.follow_chooser_answer();
            return;
        }
        let escape = matches!(
            event,
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && matches!(event.logical_key, Key::Named(NamedKey::Escape))
        );
        if escape && let Some(effect) = self.escape_effect() {
            self.dispatch.effect(&effect);
            *ctx.handled = true;
            return;
        }
        if let Some(composed) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some((_, picker)) = self.chooser.as_ref() {
            return vec![picker];
        }
        self.composed
            .as_ref()
            .map(std::convert::AsRef::as_ref)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some((_, picker)) = self.chooser.as_mut() {
            return vec![picker];
        }
        self.composed
            .as_mut()
            .map(|composed| composed.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "Onboarding"
    }

    fn test_id(&self) -> Option<&str> {
        Some("onboarding")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|composed| composed.take_structure_dirty());
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        (!self.reduce_motion && self.transition.is_moving(ui::motion::frame_now()))
            || self
                .chooser
                .as_ref()
                .is_some_and(|(_, picker)| picker.needs_animation_frame())
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_animation_frame())
    }
}

fn screen_order(screen: Screen) -> u8 {
    match screen {
        Screen::Intro => 0,
        Screen::Existing | Screen::NoRecovery => 1,
        Screen::Nearby | Screen::Restore | Screen::RecoveryKey => 2,
        Screen::Done => 3,
    }
}

fn view_order(screen: Screen, tour: Option<TourPage>) -> u8 {
    match tour {
        Some(TourPage::Navigate) => 4,
        Some(TourPage::Organise) => 5,
        Some(TourPage::Collaborate) => 6,
        Some(TourPage::Start) => 7,
        None => screen_order(screen),
    }
}

fn chooser_rect(viewport: Rect) -> Rect {
    let width = (viewport.width() * 0.78)
        .clamp(420.0, 900.0)
        .min(viewport.width());
    let height = (viewport.height() * 0.78)
        .clamp(320.0, 640.0)
        .min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - width) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - height) / 2.0;
    Rect::new(x0, y0, x0 + width, y0 + height)
}

/// A step arriving now, on the drill.
fn step_arriving(motion: ui::tokens::EffectiveMotion) -> ui::motion::Presence {
    let mut arriving = ui::motion::Presence::closed();
    arriving.show(
        true,
        ui::motion::Drill::SPEC,
        motion,
        ui::tokens::MotionSource::Pointer,
        ui::time::now(),
    );
    arriving
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tour_backdrop_only_scrims_the_covered_desktop() {
        for theme in [Theme::dark(), Theme::high_contrast()] {
            let page = Page {
                screen: Screen::Done,
                identity_ready_page: true,
                tour_page: Some(TourPage::Navigate),
                ..Page::default()
            };
            let dispatch = ui::framework::testing::dispatcher::<Onboarding>(crate::APP_ID);
            let mut ceremony = Ceremony::new(None, page, dispatch, None);
            ceremony.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let mut scene = Scene::new();
            ceremony.draw_backdrop(&mut scene, Affine::IDENTITY, &theme);
            assert_eq!(
                scene.encoding().n_paths,
                1,
                "a modal tour backdrop must not add a detached desktop focus target"
            );
        }
    }
}
