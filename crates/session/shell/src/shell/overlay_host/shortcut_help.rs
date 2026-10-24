#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(super) fn shortcut_help_panel_rect(&self) -> Rect {
        // Keep the desktop visible around the guide: it is a contextual
        // reference, not a replacement screen. The additional desktop height
        // gives the two sections room to breathe, while compact height still
        // contains every documented action.
        let area = self.overlay_area();
        let width = (area.width() - 64.0).clamp(0.0, 960.0);
        // The keyboard guide is exactly as tall as its content (header, body,
        // bottom margin) and scrolls only when the window is shorter; the
        // other modalities keep their fixed card.
        let intrinsic = if matches!(
            self.shortcut_help_modality(),
            ui::InputModality::Keyboard | ui::InputModality::Pointer
        ) {
            let note_lines = shortcut_note_lines(
                shortcut_action_summary(&self.shortcut_config).is_some(),
                self.desktop_controls.config().vim_mode,
            );
            shortcut_guide_height(
                width - 48.0,
                SHORTCUT_GUIDE_ROWS.len(),
                note_lines,
                self.shortcut_guide_rhythm,
            )
        } else {
            600.0
        };
        let height = (area.height() - 64.0).min(intrinsic).max(0.0);
        Rect::new(
            area.center().x - width / 2.0,
            area.center().y - height / 2.0,
            area.center().x + width / 2.0,
            area.center().y + height / 2.0,
        )
    }

    /// The Done button: right-aligned in the header, as wide as its label
    /// (measured by the last paint), never narrower than a pointer target.
    pub(super) fn shortcut_help_done_rect(&self) -> Rect {
        let panel = self.shortcut_help_panel_rect();
        let width = self.shortcut_help_done_width.max(56.0);
        Rect::new(
            panel.x1 - 24.0 - width,
            panel.y0 + 18.0,
            panel.x1 - 24.0,
            panel.y0 + 50.0,
        )
    }

    #[cfg(test)]
    pub(super) fn draw_shortcut_help_button(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.shortcut_help_button_is_suppressed() {
            return;
        }
        let style = theme.text(TextRole::Label).strong();
        let label = t("help-pill");
        let label = label.as_str();
        let (w, h) = text_renderer.measure_style(label, None, &style);
        // Label padding `s6` across / `s3` down, `s4` off the display
        // corner (the same pixels as before, now the theme's steps). The
        // pill is at least the §9.6 pointer floor plus `s1` tall (36 at
        // 100 %), and as wide as its measured label needs — a 140 px width
        // floor added nothing the label did not.
        let spacing = &theme.tokens.spacing;
        let (pad_x, pad_y, inset) = (
            f64::from(spacing.s6),
            f64::from(spacing.s3),
            f64::from(spacing.s4),
        );
        let min_h = f64::from(theme.tokens.metrics.min_pointer_target + spacing.s1);
        // The focused display's corner: the desktop's bottom-right corner
        // is on no display at all when the displays are stacked offset.
        let area = self.overlay_area();
        self.shortcut_help_drawn_rect = Rect::new(
            area.x1 - (f64::from(w) + pad_x) - inset,
            area.y1 - (f64::from(h) + pad_y).max(min_h) - inset,
            area.x1 - inset,
            area.y1 - inset,
        );
        let rect = self.shortcut_help_button_rect();
        let colors = &theme.tokens.colors;
        // A floating pill over the desktop is Panel material (HIG §9.1–9.2):
        // the panel ground with its derived edge, never a ramp slot
        // (`neutral[1]` until 2026-09-17, S1-27), and ink chosen against
        // that ground rather than assumed.
        let radius = rect.height() / 2.0;
        let surface = ui::chrome::ChromeRole::Panel.surface(theme);
        ui::paint::fill_round(scene, transform, rect, radius, surface);
        ui::paint::stroke_round(
            scene,
            transform,
            rect,
            radius,
            ui::chrome::shell::panel_border(theme),
            1.0,
        );
        text_renderer.draw_style(
            scene,
            label,
            colors.ink_on(
                surface,
                colors.text_primary,
                ui::tokens::colors::ON_FILL_MIN_CONTRAST,
            ),
            transform.pre_translate(
                (
                    rect.center().x - f64::from(w) / 2.0,
                    rect.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
            &style,
        );
    }

    /// The input devices and last-used modality the chrome adapts to.
    pub fn interaction_environment(&self) -> ui::InteractionEnvironment {
        self.interaction
    }

    /// Input language the help overlay is written in.
    pub fn shortcut_help_modality(&self) -> ui::InputModality {
        self.interaction.preferred_hint_modality()
    }

    /// The shortcut-guide affordance is persistent chrome, not modal
    /// content. Keep it behind every temporary shell-owned surface: drawing
    /// it later would let its label overlap a popover at small sizes or large
    /// text, and would make it appear actionable while that surface owns
    /// input.
    pub(super) fn shortcut_help_button_is_suppressed(&self) -> bool {
        let flags = self.overlay_flags();
        let has_app_content = self.chrome.as_ref().is_some_and(|chrome| {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .is_some_and(tilo_desktop_model::Workspace::has_windows)
        });
        self.onboarding.is_some()
            || has_app_content
            || flags.shortcut_help
            || flags.desktop_tour
            || flags.overview
            || flags.quick_settings
            || flags.ws_settings
            || flags.collaborators
            || flags.room_activity
            || flags.projection
            || flags.capture
            || self.palette_visible
            || self.palette_transition.is_visible(ui::time::now())
            || self.qs_motion.is_visible(ui::time::now())
            || self.layers.power_menu.is_open()
            || self.workspace_settings_overlay.member_sheet_is_open()
            || self.file_picker_overlay.widget().is_some()
            || self.peer_sync_overlay.visible()
            || self.directory_sheet.visible()
            || self.portal_dialogs.view().is_some()
    }

    #[allow(
        clippy::items_after_statements,
        clippy::too_many_lines,
        reason = "the shortcut guide is one cohesive modal rendering pass"
    )]
    pub(super) fn draw_shortcut_help(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        draw_modal_scrim_at(scene, transform, self.rect, 1.0, theme);
        let panel = self.shortcut_help_panel_rect();
        let colors = &theme.tokens.colors;
        // The guide is a Panel (HIG §9.1): `radii.panel`, the panel ground
        // from the semantic surface tier (never `neutral[0]`, §9.2), a
        // derived hairline edge, and an `e4` shadow along that outline.
        // Was an 18 px radius — off the 4/8/12/24 scale — on a ramp slot
        // with a sprite shadow until 2026-09-17 (S1-26).
        let role = ui::chrome::ChromeRole::Panel;
        let panel_radius = role.radius(theme, panel.height());
        let panel_shape = RoundedRect::from_rect(panel, panel_radius);
        ui::surface::shadow(
            scene,
            transform,
            panel,
            panel_radius,
            &theme.tokens.elevation.floating,
        );
        ui::paint::fill_shape(scene, transform, &panel_shape, role.surface(theme));
        ui::paint::stroke_shape(
            scene,
            transform,
            &panel_shape,
            ui::chrome::shell::panel_border(theme),
            1.0,
        );
        let title_style = theme.text(TextRole::Title);
        let title = t("guide-title");
        text_renderer.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate((panel.x0 + 24.0, panel.y0 + 20.0).into()),
            Some(f32_of_f64(
                (self.shortcut_help_done_rect().x0 - panel.x0 - 48.0).max(0.0),
            )),
            &title_style,
        );
        let (title_width, _) = text_renderer.measure_style(&title, None, &title_style);
        // The key that reopens the guide.
        let reopen = [ui::widget::KeyHint::new(["F1"], String::new())];
        let reopen = ui::widget::KeyHintsFace::new(&reopen);
        let reopen_x = panel.x0 + 34.0 + f64::from(title_width);
        let reopen_width = reopen.width(text_renderer, theme) + 1.0;
        reopen.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            Rect::new(
                reopen_x,
                panel.y0 + 22.0,
                reopen_x + reopen_width,
                panel.y0 + 46.0,
            ),
        );
        text_renderer.draw_style(
            scene,
            &t("guide-reopen-hint"),
            colors.text_dim,
            transform.pre_translate((panel.x0 + 24.0, panel.y0 + 54.0).into()),
            Some(f32_of_f64(panel.width() - 56.0)),
            &theme.text(TextRole::Caption),
        );

        let done_label = t("guide-done");
        self.shortcut_help_done_width = ui::widget::ButtonFace::new(Rect::ZERO, &done_label)
            .role(ui::widget::ButtonRole::Primary)
            .intrinsic_width(text_renderer, theme);
        let done = self.shortcut_help_done_rect();
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(done, &done_label)
                .role(ui::widget::ButtonRole::Primary)
                .focused(self.shortcut_guide_focus == Some(ShortcutGuideFocus::Done)),
        );

        use crate::compositor::controller_nav::{FaceRole, face_button};
        use ui::input::{
            ControllerBinding, ControllerButton, InputBinding, KeyLegend, KeyboardBinding,
            KeyboardModifier, TouchGesture,
        };
        use ui::widget::{
            InputDeviceGlyph, InputHint, KeyHintsFace, LegendConventions, SectionHeaderFace,
        };
        let platform = ui::PlatformConventions::default();
        let family = self
            .interaction
            .controller
            .unwrap_or(ui::ControllerFamily::Generic);
        let body = Rect::new(
            panel.x0 + 24.0,
            panel.y0 + 98.0,
            panel.x1 - 24.0,
            panel.y1 - 22.0,
        );
        self.shortcut_binding_rects.clear();
        self.shortcut_reset_rects.clear();

        let help_modality = self.shortcut_help_modality();
        if help_modality != ui::InputModality::Touch {
            let feedback = if let Some(action) = self.shortcut_rebind {
                Some(self.shortcut_rebind_message.clone().map_or_else(
                    || {
                        t_args(
                            "guide-press-new-shortcut",
                            &[("action", shortcut_action_label(action).into())],
                        )
                    },
                    |message| t_args("guide-message-esc-cancels", &[("message", message.into())]),
                ))
            } else {
                self.shortcut_rebind_message.clone()
            };
            if let Some(feedback) = feedback {
                text_renderer.draw_style(
                    scene,
                    &feedback,
                    colors.text_primary,
                    transform.pre_translate((body.x0, panel.y0 + 76.0).into()),
                    Some(f32_of_f64(body.width())),
                    &theme.text(TextRole::Caption),
                );
            }
        }

        match help_modality {
            ui::InputModality::Controller => {
                InputDeviceGlyph::paint_controller(
                    scene,
                    transform,
                    Rect::new(body.x0, body.y0, body.x0 + 82.0, body.y0 + 52.0),
                    family,
                    theme,
                );
                let guide = ControllerButton::Guide.label(family);
                text_renderer.draw_style(
                    scene,
                    &t_args("guide-controller-intro", &[("guide", guide.into())]),
                    colors.text_primary,
                    transform.pre_translate((body.x0 + 104.0, body.y0 + 8.0).into()),
                    Some(f32_of_f64(body.width() - 104.0)),
                    &theme.text(TextRole::Caption),
                );
                let one = |button| (vec![button], vec![button]);
                let chord = |first, second| {
                    (
                        vec![ControllerButton::Guide, first],
                        vec![ControllerButton::Guide, second],
                    )
                };
                type Choice = (Vec<ControllerButton>, Vec<ControllerButton>);
                let rows: [(&str, Choice); 13] = [
                    (
                        "guide-row-move-focus",
                        (
                            vec![ControllerButton::DpadLeft],
                            vec![ControllerButton::DpadRight],
                        ),
                    ),
                    (
                        "guide-row-select",
                        one(face_button(family, FaceRole::Confirm)),
                    ),
                    ("guide-row-back", one(face_button(family, FaceRole::Cancel))),
                    (
                        "guide-row-app-actions",
                        one(face_button(family, FaceRole::Third)),
                    ),
                    (
                        "guide-row-search",
                        one(face_button(family, FaceRole::Fourth)),
                    ),
                    (
                        "guide-row-tabs",
                        (
                            vec![ControllerButton::LeftBumper],
                            vec![ControllerButton::RightBumper],
                        ),
                    ),
                    (
                        "guide-row-workspaces",
                        (
                            vec![ControllerButton::LeftTrigger],
                            vec![ControllerButton::RightTrigger],
                        ),
                    ),
                    ("guide-overview", one(ControllerButton::Select)),
                    ("guide-row-this-guide", one(ControllerButton::Start)),
                    (
                        "guide-row-windows",
                        chord(ControllerButton::DpadLeft, ControllerButton::DpadRight),
                    ),
                    (
                        "guide-row-strips",
                        chord(ControllerButton::DpadUp, ControllerButton::DpadDown),
                    ),
                    (
                        "guide-row-move-window",
                        chord(ControllerButton::LeftBumper, ControllerButton::RightBumper),
                    ),
                    ("guide-row-quick-settings", one(ControllerButton::Guide)),
                ];
                let row_step = if panel.height() <= 536.0 { 48.0 } else { 54.0 };
                let per_column = rows.len().div_ceil(2);
                for (index, (label, (first, second))) in rows.iter().enumerate() {
                    let column = index.checked_div(per_column).unwrap_or(0);
                    let row = index.checked_rem(per_column).unwrap_or(index);
                    let x = body.x0 + f64_of_usize(column) * (body.width() / 2.0);
                    let y = body.y0 + 66.0 + f64_of_usize(row) * row_step;
                    text_renderer.draw_style(
                        scene,
                        &t(label),
                        colors.text_primary,
                        transform.pre_translate((x, y).into()),
                        None,
                        &theme.text(TextRole::Caption),
                    );
                    InputHint::paint_controller_choice_at(
                        &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                        Point::new(x, y + 20.0),
                        &ControllerBinding::new(first.iter().copied()),
                        &ControllerBinding::new(second.iter().copied()),
                        LegendConventions {
                            platform,
                            keyboard: self.interaction.keyboard_family,
                            controller: family,
                        },
                    );
                }
            }
            ui::InputModality::Touch => {
                text_renderer.draw_style(
                    scene,
                    &t("guide-touch-intro"),
                    colors.text_dim,
                    transform.pre_translate((body.x0, body.y0).into()),
                    None,
                    &theme.text(TextRole::Caption),
                );
                let gestures = [
                    (
                        "guide-touch-focus-next-window",
                        TouchGesture::SwipeLeft { fingers: 3 },
                    ),
                    (
                        "guide-touch-focus-previous-window",
                        TouchGesture::SwipeRight { fingers: 3 },
                    ),
                    (
                        "guide-touch-strip-above",
                        TouchGesture::SwipeUp { fingers: 3 },
                    ),
                    (
                        "guide-touch-strip-below",
                        TouchGesture::SwipeDown { fingers: 3 },
                    ),
                    (
                        "guide-touch-previous-workspace",
                        TouchGesture::SwipeUp { fingers: 4 },
                    ),
                    (
                        "guide-touch-next-workspace",
                        TouchGesture::SwipeDown { fingers: 4 },
                    ),
                    ("guide-touch-open-overview", TouchGesture::PinchIn),
                    ("guide-touch-close-overview", TouchGesture::PinchOut),
                ];
                let narrow = body.width() < 560.0;
                for (index, (label, gesture)) in gestures.iter().enumerate() {
                    let column = if narrow { 0 } else { index % 2 };
                    let row = if narrow { index } else { index / 2 };
                    let x = body.x0 + f64_of_usize(column) * (body.width() / 2.0);
                    let y = body.y0
                        + if narrow { 36.0 } else { 52.0 }
                        + f64_of_usize(row) * if narrow { 54.0 } else { 82.0 };
                    text_renderer.draw_style(
                        scene,
                        &t(label),
                        colors.text_primary,
                        transform.pre_translate((x, y).into()),
                        None,
                        &theme.text(TextRole::Caption),
                    );
                    InputHint::paint_at(
                        &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                        Point::new(x, y + 25.0),
                        &InputBinding::Touch(*gesture),
                        LegendConventions {
                            platform,
                            keyboard: self.interaction.keyboard_family,
                            controller: family,
                        },
                    );
                }
            }
            ui::InputModality::Keyboard | ui::InputModality::Pointer => {
                let shortcuts = &self.shortcut_config;
                let defaults = settings_store::hardware::ShortcutConfig::default();
                let binding = |action| {
                    let chord = shortcuts.bindings.get(&action);
                    let modifiers = chord
                        .into_iter()
                        .flat_map(|chord| chord.modifiers.iter())
                        .filter_map(|modifier| match modifier.as_str() {
                            "Super" => Some(KeyboardModifier::System),
                            "Ctrl" => Some(KeyboardModifier::Control),
                            "Alt" => Some(KeyboardModifier::Alt),
                            "Shift" => Some(KeyboardModifier::Shift),
                            _ => None,
                        });
                    let key = chord.map_or_else(
                        || KeyLegend::Character("—".into()),
                        |chord| KeyLegend::from_name(&chord.key),
                    );
                    KeyboardBinding::new(modifiers, key)
                };
                let rows = SHORTCUT_GUIDE_ROWS;
                let references = shortcut_reference_groups(shortcuts);
                let summary = shortcut_action_summary(shortcuts);
                let vim_mode = self.desktop_controls.config().vim_mode;
                let note_lines = shortcut_note_lines(summary.is_some(), vim_mode);
                // Everything below is laid out in content coordinates and
                // shifted by `scroll`; the body clips it. The panel is as
                // tall as this content, so scrolling only exists when the
                // window is shorter than the guide.
                let content_height = shortcut_guide_content_height(
                    body.width(),
                    rows.len(),
                    note_lines,
                    self.shortcut_guide_rhythm,
                );
                self.shortcut_guide_max_scroll = (content_height - body.height()).max(0.0);
                let scroll = self
                    .shortcut_guide_scroll
                    .clamp(0.0, self.shortcut_guide_max_scroll);
                self.shortcut_guide_scroll = scroll;
                let outer = transform;
                let transform = outer.pre_translate((0.0, -scroll).into());
                let visible = body.inflate(8.0, 0.0);
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    outer,
                    &visible,
                );
                let nav_columns = shortcut_navigation_columns(body.width());
                let rhythm = self.shortcut_guide_rhythm;
                let navigation_height = shortcut_navigation_height(rows.len(), nav_columns, rhythm);
                let navigation_surface =
                    Rect::new(body.x0, body.y0, body.x1, body.y0 + navigation_height);
                // The navigation chords sit on a recessed card in the panel.
                let navigation_ground = colors.surfaces.sidebar;
                ui::surface::card(
                    scene,
                    transform,
                    navigation_surface,
                    f64::from(theme.tokens.radii.detail),
                    navigation_ground,
                );
                let rows_top = shortcut_group_rows_top(rhythm);
                SectionHeaderFace::group(&t("guide-navigation-heading")).paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(
                        navigation_surface.x0 + 14.0,
                        navigation_surface.y0 + rhythm.inset,
                        navigation_surface.x1 - 14.0,
                        navigation_surface.y0 + rhythm.inset + rhythm.header,
                    ),
                    ui::widget::Interaction::default(),
                );
                for (index, (label, previous, next)) in rows.iter().enumerate() {
                    let column = index.checked_rem(nav_columns).unwrap_or_default();
                    let row = index.checked_div(nav_columns).unwrap_or_default();
                    let column_width = (navigation_surface.width()
                        - 28.0
                        - 8.0 * f64_of_usize(nav_columns.saturating_sub(1)))
                        / f64_of_usize(nav_columns);
                    let x =
                        navigation_surface.x0 + 14.0 + f64_of_usize(column) * (column_width + 8.0);
                    let y = navigation_surface.y0
                        + rows_top
                        + f64_of_usize(row) * rhythm.navigation_step;
                    text_renderer.draw_style(
                        scene,
                        &t(label),
                        colors.text_primary,
                        transform.pre_translate((x, y).into()),
                        None,
                        &theme.text(TextRole::Caption),
                    );
                    let actions = [*previous, *next];
                    let hit_rects = InputHint::paint_keyboard_choice_at(
                        &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                        Point::new(x, y + 18.0),
                        &binding(*previous),
                        &binding(*next),
                        LegendConventions {
                            platform,
                            keyboard: self.interaction.keyboard_family,
                            controller: family,
                        },
                    );
                    for (action, rect) in actions.into_iter().zip(hit_rects) {
                        let hit =
                            Rect::new(rect.x0 - 5.0, rect.y0 - 5.0, rect.x1 + 5.0, rect.y1 + 5.0);
                        // Pointer hit-testing runs in screen space.
                        let screen_hit =
                            Rect::new(hit.x0, hit.y0 - scroll, hit.x1, hit.y1 - scroll);
                        let active = self.shortcut_rebind == Some(action);
                        let focused =
                            self.shortcut_guide_focus == Some(ShortcutGuideFocus::Binding(action));
                        let hovered = self
                            .last_cursor
                            .is_some_and(|point| screen_hit.contains(point));
                        // The chord being rebound is selected; one under the
                        // pointer lifts; the keyboard's carries its ring.
                        if active || hovered {
                            ui::chrome::shell::paint_row_selection(
                                scene,
                                transform,
                                hit,
                                ui::chrome::SelectionState::new(active, hovered && !active, false),
                                navigation_ground,
                                theme,
                            );
                        }
                        if focused {
                            ui::widget::paint_focus_ring(
                                scene,
                                transform,
                                hit,
                                f64::from(theme.tokens.radii.control),
                                theme,
                            );
                        }
                        if screen_hit.intersect(body).area() > 0.0 {
                            self.shortcut_binding_rects.push((action, screen_hit));
                        }
                    }

                    let reset_side = ui::widget::ControlSize::Small.height(theme);
                    let caps_cy = y + 18.0 + InputHint::keycap_height(theme) / 2.0;
                    let reset = Rect::new(
                        hit_rects[1].x1 + 8.0,
                        caps_cy - reset_side / 2.0,
                        hit_rects[1].x1 + 8.0 + reset_side,
                        caps_cy + reset_side / 2.0,
                    );
                    let changed = actions.into_iter().any(|action| {
                        shortcuts.bindings.get(&action) != defaults.bindings.get(&action)
                    });
                    if changed {
                        let screen_reset =
                            Rect::new(reset.x0, reset.y0 - scroll, reset.x1, reset.y1 - scroll);
                        let hovered = self
                            .last_cursor
                            .is_some_and(|point| screen_reset.contains(point));
                        let focused = self.shortcut_guide_focus
                            == Some(ShortcutGuideFocus::Reset(*previous, *next));
                        let name =
                            t_args("keyboard-shortcut-reset", &[("shortcut", t(label).into())]);
                        ui::widget::Button::paint(
                            scene,
                            transform,
                            text_renderer,
                            theme,
                            &ui::widget::IconButton::face(reset, ui::icons::Icon::Undo2, &name)
                                .hovered(hovered)
                                .focused(focused),
                        );
                        if screen_reset.intersect(body).area() > 0.0 {
                            self.shortcut_reset_rects
                                .push((*previous, *next, screen_reset));
                        }
                    }
                }
                let essentials_y = navigation_surface.y1;
                SectionHeaderFace::group(&t("guide-essentials-heading")).paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    Rect::new(body.x0, essentials_y, body.x1, essentials_y + rhythm.header),
                    ui::widget::Interaction::default(),
                );
                let reference_columns = shortcut_reference_columns(body.width());
                let reference_step = shortcut_reference_step();
                let reference_top = essentials_y + rhythm.header;
                let reference_gap = 12.0;
                let reference_width = (body.width()
                    - reference_gap * f64_of_usize(reference_columns.saturating_sub(1)))
                    / f64_of_usize(reference_columns);
                let reference_height = shortcut_reference_group_height(rhythm);
                let group_titles = [
                    "guide-group-windows",
                    "guide-group-search",
                    "guide-group-capture",
                ];
                let chord_style = theme.text(TextRole::Caption);
                let label_gap =
                    f64::from(text_renderer.measure_style("Ag", None, &chord_style).1) + 4.0;
                let cap_h = KeyHintsFace::height(theme);
                let mut groups_bottom = reference_top;
                for (group_index, group_rows) in references.iter().enumerate() {
                    let column = group_index
                        .checked_rem(reference_columns)
                        .unwrap_or_default();
                    let band = group_index
                        .checked_div(reference_columns)
                        .unwrap_or_default();
                    let x = body.x0 + f64_of_usize(column) * (reference_width + reference_gap);
                    let y = reference_top + f64_of_usize(band) * (reference_height + reference_gap);
                    let group = Rect::new(x, y, x + reference_width, y + reference_height);
                    groups_bottom = groups_bottom.max(group.y1);
                    ui::surface::card(
                        scene,
                        transform,
                        group,
                        f64::from(theme.tokens.radii.detail),
                        qs::control_fill(theme, false),
                    );
                    let group_title = group_titles.get(group_index).copied().unwrap_or_default();
                    SectionHeaderFace::group(&t(group_title)).paint(
                        &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                        Rect::new(
                            group.x0 + 12.0,
                            group.y0 + rhythm.inset,
                            group.x1 - 12.0,
                            group.y0 + rhythm.inset + rhythm.header,
                        ),
                        ui::widget::Interaction::default(),
                    );
                    for (row_index, (label, chord)) in group_rows.iter().enumerate() {
                        let entry_y = group.y0
                            + shortcut_group_rows_top(rhythm)
                            + f64_of_usize(row_index) * reference_step;
                        text_renderer.draw_style(
                            scene,
                            label,
                            colors.text_primary,
                            transform.pre_translate((group.x0 + 12.0, entry_y).into()),
                            Some(f32_of_f64(group.width() - 24.0)),
                            &chord_style,
                        );
                        // Alternative chords are one hint, joined by "or".
                        let cap_y = entry_y + label_gap;
                        KeyHintsFace::new(&shortcut_chord_hints(chord)).paint(
                            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                            Rect::new(group.x0 + 12.0, cap_y, group.x1 - 12.0, cap_y + cap_h),
                        );
                    }
                }
                let pointer_y = groups_bottom + 4.0;
                ui::surface::separator(
                    scene,
                    transform,
                    body.x0,
                    body.x1,
                    pointer_y + 0.5,
                    0.0,
                    ui::chrome::ChromeRole::Panel.surface(theme),
                );
                let mut note_y = pointer_y + 14.0;
                let mut note = |text: &str, scene: &mut Scene| {
                    text_renderer.draw_style(
                        scene,
                        text,
                        colors.text_primary,
                        transform.pre_translate((body.x0, note_y).into()),
                        Some(f32_of_f64(body.width())),
                        &theme.text(TextRole::Caption),
                    );
                    note_y += 18.0;
                };
                note(&t("guide-pointer"), scene);
                note(&shortcuts::shortcut_strip_jump_note(shortcuts), scene);
                note(&shortcut_app_tab_note(), scene);
                if let Some(summary) = &summary {
                    note(summary, scene);
                }
                if vim_mode {
                    note(&t("guide-vim-mode"), scene);
                }
                scene.pop_layer();
                if self.shortcut_guide_max_scroll > 0.0 {
                    // A scrollbar thumb on the panel's right inner edge, so
                    // a clipped guide says it continues.
                    let track = Rect::new(panel.x1 - 10.0, body.y0, panel.x1 - 6.0, body.y1);
                    let thumb_h = (track.height() * body.height() / content_height).max(24.0);
                    let thumb_y = track.y0
                        + (track.height() - thumb_h) * scroll / self.shortcut_guide_max_scroll;
                    ui::widget::scrollbar::paint_thumb(
                        scene,
                        outer,
                        Rect::new(track.x0, thumb_y, track.x1, thumb_y + thumb_h),
                        theme,
                        false,
                    );
                }
            }
        }
    }
}
