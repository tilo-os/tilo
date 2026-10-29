//! Top overlays: the search card, portal dialogs and the sheets.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// The overlays that sit above dropdowns and sheets: the command
    /// palette, the peer-sync card, the file-picker overlay and the OSD.
    /// Last of four passes split out of `draw`.
    pub(super) fn draw_top_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_top_overlays_below_switcher(scene, transform, text_renderer, theme, presence);
        let desktop = transform.transform_rect_bbox(self.rect);
        let outputs = self.layer_outputs();
        for (kind, id) in [
            ("shell.dictation", LayerId::Dictation),
            ("shell.window-switcher", LayerId::WindowSwitcher),
        ] {
            overlay_part(scene, kind, desktop, |scene| {
                self.layers.draw(
                    id,
                    &mut LayerPaint {
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        presence,
                        geometry: self.layer_geometry(&outputs),
                        thumbnails: &mut self.window_previews,
                    },
                );
                None
            });
        }
        // The last pass: every preview on hand has now been painted.
        self.window_previews.mark_painted();
    }

    pub(super) fn draw_top_overlays_below_switcher(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.palette_was_visible = self.palette_visible;
        self.palette_transition.show(
            self.palette_visible,
            ui::motion::Appear::SPEC,
            theme.motion(),
            self.input_source,
            ui::time::now(),
        );
        let desktop = transform.transform_rect_bbox(self.rect);
        self.draw_search_and_desk(scene, transform, text_renderer, theme, presence);

        // The tab menu: above the windows and the strip chrome it was
        // opened from, below the sheets that follow (a portal answer or a
        // token prompt is modal; a menu is not).
        let outputs = self.layer_outputs();
        overlay_part(scene, "shell.tab-menu", desktop, |scene| {
            self.layers.draw(
                LayerId::TabContextMenu,
                &mut LayerPaint {
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    geometry: self.layer_geometry(&outputs),
                    thumbnails: &mut self.window_previews,
                },
            );
            Some(
                self.layers
                    .tab_menu
                    .painted_bounds(theme)
                    .into_iter()
                    .map(|rect| transform.transform_rect_bbox(rect))
                    .collect(),
            )
        });

        // Owns the keyboard while up, so it must sit above the palette
        // that opened it.
        let directory_outputs = self.layer_outputs();
        overlay_part(scene, "shell.directory", desktop, |scene| {
            use super::layers::ShellLayer as _;
            self.layers.directory.draw(&mut LayerPaint {
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                geometry: self.layer_geometry(&directory_outputs),
                thumbnails: &mut self.window_previews,
            });
            None
        });

        overlay_part(scene, "shell.peer-sync", desktop, |scene| {
            self.draw_peer_sync(scene, transform, text_renderer, theme, presence);
            None
        });

        overlay_part(scene, "shell.app-uninstall", desktop, |scene| {
            self.draw_app_sheet(scene, transform, text_renderer, theme, presence);
            None
        });

        let outputs = self.layer_outputs();
        overlay_part(scene, "shell.shortcut-help", desktop, |scene| {
            use super::layers::ShellLayer as _;
            self.layers.shortcut_guide.draw(&mut LayerPaint {
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                geometry: self.layer_geometry(&outputs),
                thumbnails: &mut self.window_previews,
            });
            None
        });

        // Above every other overlay, including the strip switcher: the
        // system sheet for the asking window is drawn by the compositor rather than
        // the requesting app so it can't be drawn over, read, or dismissed
        // by that app.
        let place = self.file_picker_place();
        overlay_part(scene, "shell.file-picker", desktop, |scene| {
            self.file_picker_overlay
                .draw(place, scene, transform, text_renderer, theme, presence);
            None
        });

        // The contextual tour paints after this pass; it draws the sheet
        // itself, above the tour, so the sheet is never covered.
        overlay_part(scene, "shell.portal-sheet", desktop, |scene| {
            if self.onboarding.is_none() {
                self.draw_portal_dialog_sheet(scene, transform, text_renderer, theme);
            }
            None
        });

        // A held pad button works over modals too, like the keys below.
        overlay_part(scene, "shell.pad-map", desktop, |scene| {
            self.draw_pad_map(scene, transform, text_renderer, theme);
            None
        });

        // Above everything, including modals — a physical key works
        // while a modal is up, so its readout must too.
        overlay_part(scene, "shell.osd", desktop, |scene| {
            self.draw_osd(scene, transform, text_renderer, theme);
            None
        });

        // A passing acknowledgement answers the action just taken, which
        // may have been inside a modal, so it shows over everything too.
        overlay_part(scene, "shell.acknowledgement", desktop, |scene| {
            self.draw_acknowledgement(scene, transform, text_renderer, theme);
            None
        });

        // A window that just filled the display says how to leave it over
        // itself, and over every other overlay: it is the only way out the
        // person can see.
        overlay_part(scene, "shell.fullscreen-hint", desktop, |scene| {
            self.draw_fullscreen_hint(scene, transform, text_renderer, theme);
            None
        });
    }

    /// The launcher's card while search is shown, arriving or leaving, and
    /// the bounds of everything it painted in scene coordinates: the card,
    /// its edge and its `e4` shadow. Nothing without a launcher.
    pub(super) fn draw_palette_card(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Vec<Rect>> {
        let card = self.search_card();
        // Summoned from no control, search appears in place, from its own
        // centre (`ui::motion::Appear`).
        let pose = ui::motion::Appear::pose(
            self.palette_transition.progress(ui::motion::frame_now()),
            card.center(),
            self.motion.get(),
        );
        let in_flight = pose.opacity < 1.0;
        let launcher = self.launcher.as_mut()?;
        if in_flight {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                BlendMode::default(),
                pose.opacity,
                transform,
                &self.rect,
            );
        }
        let card_xform = pose.applied_to(transform);
        // The launcher paints its card, its shadow and its chips itself.
        launcher
            .app
            .draw(scene, card_xform, text_renderer, theme, presence);
        if in_flight {
            scene.pop_layer();
        }
        let shadow = ui::widget::floating_surface::card_shadow(theme);
        let edge = card.inflate(2.0, 2.0);
        let painted =
            ui::surface::shadow_bounds(card, &shadow).map_or(edge, |shadow| shadow.union(edge));
        Some(vec![card_xform.transform_rect_bbox(painted)])
    }

    /// Draw an OS-owned review sheet. The requesting application controls
    /// neither its surface nor its hit regions.
    #[allow(
        clippy::too_many_lines,
        reason = "all portal sheet variants share one chrome renderer so untrusted app data cannot select a different visual surface"
    )]
    pub(super) fn draw_portal_dialog_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // A review sheet is a security surface: the app, the files and the
        // devices it names are read letter by letter.
        text_renderer.security_surface(|text_renderer| {
            let Some(view) = self.portal_dialogs.view() else {
                return;
            };
            let area = self.overlay_area();
            let inset = self.portal_dialogs.keyboard_inset(area);
            let interaction = self.interaction;
            if let Some(prompt) = self.portal_dialogs.prompt_mut() {
                // A PIN asked while touch is in use opens with its keypad.
                ui::widget::Widget::set_interaction_environment(prompt, interaction);
                prompt.set_keyboard_inset(inset);
                // A password is asked with the one prompt every part of Tilo
                // uses, sealed as the compositor's own.
                prompt.place(area, text_renderer, theme);
                prompt.paint(scene, transform, text_renderer, theme);
                if let Some(modal) = &prompt.layout().modal {
                    crate::shell::trusted_ui::paint_seal(
                        scene,
                        transform,
                        modal.card,
                        theme,
                        if modal.card == modal.scrim {
                            0.0
                        } else {
                            ui::widget::floating_surface::radius(theme)
                        },
                    );
                }
                return;
            }
            // A draft sheet's detail wraps: measured before the sheet is
            // laid out, so its rows start below it.
            if let Some((detail, width)) = self.portal_dialogs.draft_detail_wrap(area) {
                let (_, height) = text_renderer.measure_style(
                    &detail,
                    Some(sheet_text_width(width)),
                    &theme.text(TextRole::Body),
                );
                self.portal_dialogs
                    .set_draft_detail_height(f64::from(height));
            }
            let Some(layout) = self.portal_dialogs.layout(area) else {
                return;
            };
            draw_modal_scrim_at(scene, transform, self.rect, 1.0, theme);
            // A review sheet is a Panel (HIG §9.1): `radii.panel` on the
            // floating card's ground, with its inner hairline and the
            // floating elevation, from the one painter every floating
            // surface uses. It hand-painted the same ground under a bare
            // shadow, so the sheet was the one Panel in the shell with no
            // edge along its outline. Opaque, not the blurred material: this
            // is a security surface, and a control on the window behind must
            // not read through the text it asks the person to trust.
            let sheet_radius = ui::chrome::ChromeRole::Panel.radius(theme, layout.sheet.height());
            ui::widget::floating_surface::paint_card(scene, transform, layout.sheet, theme);
            crate::shell::trusted_ui::paint_seal(
                scene,
                transform,
                layout.sheet,
                theme,
                sheet_radius,
            );
            text_renderer.draw_style(
                scene,
                &view.title(),
                theme.tokens.colors.text_primary,
                transform.pre_translate((layout.sheet.x0 + 24.0, layout.sheet.y0 + 22.0).into()),
                None,
                &theme.text(TextRole::Title),
            );
            text_renderer.draw_style(
                scene,
                &view.app_name(),
                theme.tokens.colors.text_dim,
                transform.pre_translate((layout.sheet.x0 + 24.0, layout.sheet.y0 + 50.0).into()),
                None,
                &theme.text(TextRole::Body),
            );
            text_renderer.draw_style(
                scene,
                &self.portal_dialogs.displayed_detail(),
                theme.tokens.colors.text_dim,
                transform.pre_translate((layout.sheet.x0 + 24.0, layout.sheet.y0 + 74.0).into()),
                layout.detail_width.map(sheet_text_width),
                &theme.text(TextRole::Body),
            );
            for (index, (choice, rect)) in view.choices().iter().zip(&layout.choices).enumerate() {
                let selected = index == self.portal_dialogs.selected();
                draw_filter_chip(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *rect,
                    choice,
                    &theme.text(TextRole::Body),
                    ui::chrome::SelectionState::active(selected),
                );
            }
            // USB is intentionally a real multi-select sheet, rather than a
            // generic all-or-nothing permission prompt.  A row click or Space
            // toggles only that device; Enter commits the resulting subset.
            let usb_start = self.portal_dialogs.visible_selection_start();
            for (visible_index, (device, rect)) in view
                .usb_devices()
                .iter()
                .skip(usb_start)
                .zip(&layout.choices)
                .enumerate()
            {
                let index = usb_start.saturating_add(visible_index);
                let access_key = if device.writable {
                    "portal-usb-row-read-write"
                } else {
                    "portal-usb-row-read-only"
                };
                let focused = index == self.portal_dialogs.selected();
                draw_check_row(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *rect,
                    &CheckRow {
                        checked: device.selected,
                        pose: self.faces.check(
                            ("usb", view.handle(), index),
                            device.selected,
                            ui::widget::CheckGlyph::Check,
                            theme.motion(),
                        ),
                        label: &device.label,
                        detail: Some(&crate::shell::i18n::t(access_key)),
                        focused,
                    },
                );
            }
            if let Some(editor) = view.launcher_editor() {
                // A shortcut step's answer is a real text field, painted below.
                if let Some(name_rect) = layout.launcher_name
                    && !matches!(
                        view,
                        crate::shell::portal_dialog::DialogView::ShortcutStep { .. }
                    )
                {
                    let focused = self.portal_dialogs.launcher_name_focused();
                    draw_filter_chip(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        name_rect,
                        self.portal_dialogs.launcher_name().unwrap_or(editor.name),
                        &theme.text(TextRole::Body),
                        ui::chrome::SelectionState::active(focused),
                    );
                }
                for (index, (label, rect)) in editor
                    .icon_choices
                    .iter()
                    .zip(&layout.launcher_icons)
                    .enumerate()
                {
                    let selected = self.portal_dialogs.launcher_icon() == Some(index);
                    draw_filter_chip(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        *rect,
                        label,
                        &theme.text(TextRole::Body),
                        ui::chrome::SelectionState::active(selected),
                    );
                }
            }
            // A confirmation's draft: each field labelled above its control,
            // a yes-or-no field as its own check row. Its text fields are
            // painted with the sheet's others, below.
            for face in self.portal_dialogs.draft_faces(&layout) {
                let Some(placed) = layout.draft_rows.iter().find(|row| row.index == face.index)
                else {
                    continue;
                };
                if let Some(on) = face.flag {
                    draw_check_row(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        placed.control,
                        &CheckRow {
                            checked: on,
                            pose: self.faces.check(
                                ("draft", view.handle(), face.index),
                                on,
                                ui::widget::CheckGlyph::Check,
                                theme.motion(),
                            ),
                            label: &face.label,
                            detail: None,
                            focused: face.focused,
                        },
                    );
                } else {
                    text_renderer.draw_style(
                        scene,
                        &face.label,
                        theme.tokens.colors.text_dim,
                        transform.pre_translate((placed.label.x0, placed.label.y0).into()),
                        Some(sheet_text_width(placed.label.width())),
                        &theme.text(TextRole::Caption),
                    );
                }
            }
            // The sheet's text fields: an authentication answer, a shortcut
            // step's, one row per network secret labelled by its
            // placeholder, or a draft's fields. Secret ones stay hidden until
            // their own show/hide control is used.
            let presence = PresenceCtx::default();
            for field in self.portal_dialogs.place_fields(&layout) {
                field.draw(scene, transform, text_renderer, theme, &presence);
            }
            if let (Some(rect), Some((remember, focused)), Some(remember_label)) = (
                layout.remember,
                self.portal_dialogs.remember_display(),
                self.portal_dialogs.remember_label(),
            ) {
                draw_check_row(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    &CheckRow {
                        checked: remember,
                        pose: self.faces.check(
                            ("remember", view.handle()),
                            remember,
                            ui::widget::CheckGlyph::Check,
                            theme.motion(),
                        ),
                        label: &remember_label,
                        detail: None,
                        focused,
                    },
                );
            }
            if let Some(error) = self.portal_dialogs.entry_error() {
                text_renderer.draw_style(
                    scene,
                    error,
                    theme.tokens.colors.semantic.warning,
                    transform
                        .pre_translate((layout.sheet.x0 + 24.0, layout.cancel.y0 - 22.0).into()),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
            for (index, (row, rect)) in view.shortcut_rows().iter().zip(&layout.choices).enumerate()
            {
                let accelerator = row.accelerator.as_deref().map_or_else(
                    || crate::shell::i18n::t("portal-shortcut-not-set"),
                    crate::portal::global_shortcuts::display_accelerator,
                );
                let label = if row.description.is_empty() {
                    format!("{}  —  {accelerator}", row.id)
                } else {
                    format!("{}  —  {accelerator}", row.description)
                };
                let selected = index == self.portal_dialogs.selected();
                draw_filter_chip(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *rect,
                    &label,
                    &theme.text(TextRole::Body),
                    ui::chrome::SelectionState::active(selected),
                );
                if let Some(status) = &row.status {
                    text_renderer.draw_style(
                        scene,
                        status,
                        theme.tokens.colors.semantic.warning,
                        transform.pre_translate((rect.x0 + 10.0, rect.y1 - 10.0).into()),
                        None,
                        &theme.text(TextRole::Caption),
                    );
                }
            }
            // A sheet whose choices are its answers: one real button per
            // choice, the suggested one primary, the other secondary, and a
            // close button for "not now".
            if let crate::shell::portal_dialog::DialogView::DriveConsent {
                choices,
                preselect,
                cancel_label,
                ..
            } = &view
            {
                for (index, (label, rect)) in choices.iter().zip(&layout.answers).enumerate() {
                    Button::paint(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        &ButtonFace::new(*rect, label)
                            .role(if index == *preselect {
                                ButtonRole::Primary
                            } else {
                                ButtonRole::Secondary
                            })
                            .focused(index == self.portal_dialogs.selected())
                            .hovered(self.last_cursor.is_some_and(|p| rect.contains(p))),
                    );
                }
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::icon_only(layout.cancel, ui::icons::Icon::X, cancel_label)
                        .hovered(self.last_cursor.is_some_and(|p| layout.cancel.contains(p))),
                );
                return;
            }
            // The sheet's two buttons are the shared `Button` (never a chip
            // painter wearing button colours): Cancel neutral, the commit
            // primary, both at the Control radius. Was `surfaces.base` /
            // `accent[5]` pills at a literal 14 px until 2026-09-17 (S1-26).
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(layout.cancel, &view.cancel_label())
                    .hovered(self.last_cursor.is_some_and(|p| layout.cancel.contains(p))),
            );
            if let Some(back) = layout.review_back {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(back, &t("guide-row-back"))
                        .hovered(self.last_cursor.is_some_and(|p| back.contains(p))),
                );
            }
            let confirm_label = if self.portal_dialogs.reviewing_more() {
                t("shortcut-step-continue")
            } else {
                view.confirm_label()
            };
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(layout.confirm, &confirm_label)
                    .role(ButtonRole::Primary)
                    .hovered(self.last_cursor.is_some_and(|p| layout.confirm.contains(p))),
            );
        });
    }
}

/// A width text on a review sheet wraps at, as the text renderer takes it.
fn sheet_text_width(width: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a sheet is at most a few hundred logical pixels wide"
    )]
    {
        width.max(0.0) as f32
    }
}
