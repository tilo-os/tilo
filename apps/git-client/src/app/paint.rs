//! What the client draws: the window's screens, the list column and its rows, the banner, the diff, the panels and the first-run screen.

use super::*;

impl GitClientApp {
    /// Paint the whole window for this frame, record its targets, and show
    /// where each overflowing region stands.
    pub(super) fn paint_window(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        self.paint_screens(ctx, presence);
        self.paint_scroll_bars(ctx);
    }

    /// Paint the screens and the list column.
    pub(super) fn paint_screens(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        self.targets.clear();
        self.semantic.clear();
        self.scroll_regions.clear();
        if self.awaiting_repo {
            self.paint_open(ctx, presence);
            return;
        }
        let theme = ctx.theme;
        let window = self.rect;
        let has_list = self.screen.has_list();
        if has_list {
            self.drawer.tick(theme);
            self.drawer.resolve(window);
            // A staged drawer opens once the window has its narrow layout.
            if self.staged == Staged::OpenDrawer && self.drawer.is_overlay() {
                self.drawer.snap(true);
                self.staged = Staged::Nothing;
            }
        }
        let overlay = has_list && self.drawer.is_overlay();
        let column = if has_list {
            self.drawer.content_rect()
        } else {
            window
        };
        let body = Rect::new(column.x0, window.y0, column.x1, window.y1);
        let body = self.paint_banner(ctx, body);
        match self.screen {
            Screen::Changes => self.paint_diff(ctx, body),
            Screen::History => self.paint_detail(ctx, body),
            Screen::Panels => self.paint_panels(ctx, presence, body),
        }

        if !has_list {
            return;
        }
        let panel = self.drawer.sidebar_rect();
        if overlay {
            if self.drawer.reveal() <= 0.001 || panel.width() <= 0.0 {
                return;
            }
            // Everything under the drawer is inert while it is out.
            self.targets.clear();
            self.semantic.clear();
            self.targets.push(Target {
                rect: window,
                hit: Hit::Scrim,
            });
            self.scroll_regions.clear();
            let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);
            let mut lctx = PaintCtx::new(&mut *ctx.scene, ctx.transform, &mut *ctx.text, &local);
            self.paint_list_column(&mut lctx, presence, panel, panel);
        } else if panel.width() > 0.5 {
            let local = self.drawer.paint_panel(ctx.scene, ctx.transform, theme);
            // The list keeps its width while the column opens or closes: it
            // slides under the column's edge instead of reflowing.
            let full = Rect::new(
                panel.x1 - self.drawer.resolved_docked_width(),
                panel.y0,
                panel.x1,
                panel.y1,
            );
            let mut lctx = PaintCtx::new(&mut *ctx.scene, ctx.transform, &mut *ctx.text, &local);
            self.paint_list_column(&mut lctx, presence, panel, full);
        } else {
            self.commit_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// The list column: the Changes or History list. `full` is the column
    /// at its full width; `clip` is what of it is showing.
    pub(super) fn paint_list_column(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        clip: Rect,
        full: Rect,
    ) {
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        match self.screen {
            Screen::Changes => self.paint_changes_list(ctx, presence, full, clip),
            Screen::History => self.paint_history_list(ctx, full, clip),
            Screen::Panels => {}
        }
        ctx.scene.pop_layer();
    }

    /// The Changes list in `list`, of which `clip` is showing.
    pub(super) fn paint_changes_list(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        list: Rect,
        clip: Rect,
    ) {
        let pointer = self.pointer.filter(|p| clip.contains(*p));
        let out = changes::paint(
            ctx,
            list,
            &changes::ChangesInput {
                model: &self.model,
                selected: self.selected_change,
                scroll: self.changes_scroll,
                message: self.commit_input.value(),
                pointer,
            },
        );
        for (rect, hit) in out.targets {
            let rect = rect.intersect(clip);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            // Row actions show under the pointer; their semantic
            // twins are recorded for every row below.
            if matches!(hit, ChangesHit::ToggleStage(..)) {
                self.targets.push(Target {
                    rect,
                    hit: Hit::Change(hit),
                });
                continue;
            }
            if let ChangesHit::SelectFile(area, index) = hit {
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Change(ChangesHit::ToggleStage(area, index)),
                });
            }
            self.push_target(rect, Hit::Change(hit));
        }
        let viewport =
            Rect::new(list.x0, list.y0, list.x1, list.y0 + out.viewport_height).intersect(clip);
        self.scroll_regions.push(ScrollRegion {
            rect: viewport,
            scroller: Scroller::ChangesList,
            max_y: out.content_height - out.viewport_height,
            max_x: 0.0,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
        self.changes_scroll = self
            .changes_scroll
            .clamp(0.0, (out.content_height - out.viewport_height).max(0.0));
        if !out.commit_enabled {
            let rect = out.commit_button.intersect(clip);
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Change(ChangesHit::Commit),
                });
            }
        }
        self.commit_input.apply_layout_external(out.commit_input);
        self.commit_input
            .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
    }

    /// The commit list in `list`, of which `clip` is showing.
    pub(super) fn paint_history_list(&mut self, ctx: &mut PaintCtx<'_>, list: Rect, clip: Rect) {
        let pointer = self.pointer.filter(|p| clip.contains(*p));
        let out = history::paint_list(
            ctx,
            list,
            &history::ListInput {
                model: &self.model,
                layout: &self.graph_layout,
                selected: self.selected_commit,
                scroll: self.history_scroll,
                pointer,
            },
        );
        for (index, rect) in out.rows {
            self.push_target(rect.intersect(clip), Hit::Commit(index));
        }
        self.scroll_regions.push(ScrollRegion {
            rect: list.intersect(clip),
            scroller: Scroller::HistoryList,
            max_y: out.content_height - list.height(),
            max_x: 0.0,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }

    /// An error or a running transfer as a banner at the top of the content;
    /// returns the content area under it.
    pub(super) fn paint_banner(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) -> Rect {
        use ui::widget::banner::{BannerContent, BannerKind};
        let (kind, title, dismissible) = if let Some(job) = &self.remote_job {
            (BannerKind::Info, job.label.clone(), false)
        } else if let Some(error) = &self.last_error {
            (BannerKind::Error, error.clone(), true)
        } else if let Some(notice) = &self.notice {
            (BannerKind::Success, notice.clone(), true)
        } else {
            return body;
        };
        let theme = ctx.theme;
        let margin = f64::from(theme.tokens.spacing.s3);
        let rect_w = (body.width() - 2.0 * margin).max(1.0);
        let content = BannerContent::new(kind, &title).dismissible(dismissible);
        let h = Banner::height(&content, rect_w, ctx.text, theme);
        let rect = Rect::new(
            body.x0 + margin,
            body.y0 + margin,
            body.x1 - margin,
            body.y0 + margin + h,
        );
        let hover = self.pointer.unwrap_or(Point::new(f64::MIN, f64::MIN));
        let hit = Banner::paint(ctx, rect, &content, hover);
        self.semantic.push(Target {
            rect,
            hit: Hit::DismissError,
        });
        if let Some(dismiss) = hit.dismiss {
            self.targets.push(Target {
                rect: dismiss,
                hit: Hit::DismissError,
            });
        }
        Rect::new(body.x0, rect.y1, body.x1, body.y1.max(rect.y1))
    }

    /// The selected file's diff, or one sentence when there is none.
    pub(super) fn paint_diff(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        let body = if let Some(file) = self.selected_conflict() {
            let (below, hits) = self.paint_conflict_actions(ctx, body, &file, self.pointer);
            for (rect, hit) in hits {
                self.push_target(rect, hit);
            }
            below
        } else {
            body
        };
        let key = self.current_diff_key();
        let Some(diff) = key.as_ref().and_then(|k| self.diffs.get(k)) else {
            let title = t(if self.model.change_count() == 0 {
                "git-tree-clean"
            } else {
                "git-select-file"
            });
            EmptyState::paint(
                ctx,
                body,
                &EmptyStateContent::new(&title),
                Point::new(f64::MIN, f64::MIN),
            );
            return;
        };
        let staged = self.selected_is_staged();
        let out = diffview::paint(
            ctx,
            body,
            &diffview::DiffInput {
                diff,
                scroll: self.diff_scroll,
                hscroll: self.diff_hscroll,
                settings: self.settings,
                staged,
                can_discard: true,
                // A conflicted file's decision is the band above its diff:
                // staging the markers would settle it as neither side.
                actions: self.selected_conflict().is_none(),
                pointer: self.pointer,
            },
        );
        for (rect, hit) in out.targets {
            self.push_target(rect, Hit::Diff(hit));
        }
        let max_y = out.content_height - out.viewport.height();
        self.diff_scroll = self.diff_scroll.clamp(0.0, max_y.max(0.0));
        self.diff_hscroll = self.diff_hscroll.clamp(0.0, out.max_hscroll);
        self.scroll_regions.push(ScrollRegion {
            rect: out.viewport,
            scroller: Scroller::Diff,
            max_y,
            max_x: out.max_hscroll,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }

    /// The branches page.
    pub(super) fn paint_panels(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        body: Rect,
    ) {
        let out = panels::paint(
            ctx,
            body,
            &panels::PanelsInput {
                model: &self.model,
                scroll: self.panels_scroll,
                pointer: self.pointer,
                can_create: self.repo_path.is_some(),
                branch_name: self.branch_input.value(),
                selected: self.branch_selection,
                worktree_fields: [
                    self.worktree_branch_input.value(),
                    self.worktree_path_input.value(),
                    self.worktree_start_input.value(),
                ],
            },
        );
        for (rect, hit) in out.targets {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.targets.push(Target {
                    rect,
                    hit: Hit::Panel(hit),
                });
            }
        }
        for (rect, hit) in out.semantic {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                self.semantic.push(Target {
                    rect,
                    hit: Hit::Panel(hit),
                });
            }
        }
        self.branch_input.apply_layout_external(out.branch_field);
        if out.branch_field.width() > 0.0 {
            self.branch_input
                .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
        }
        for (input, rect) in [
            &mut self.worktree_branch_input,
            &mut self.worktree_path_input,
            &mut self.worktree_start_input,
        ]
        .into_iter()
        .zip(out.worktree_fields)
        {
            input.apply_layout_external(rect);
            if rect.width() > 0.0 {
                input.draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
            }
        }
        let max_y = out.content_height - body.height();
        self.panels_scroll = self.panels_scroll.clamp(0.0, max_y.max(0.0));
        self.scroll_regions.push(ScrollRegion {
            rect: body,
            scroller: Scroller::Panels,
            max_y,
            max_x: 0.0,
            ground: ctx.theme.tokens.colors.surfaces.content,
        });
    }

    /// The open screen's title and what to do, from `at` down; returns
    /// where the path row starts.
    pub(super) fn paint_open_intro(ctx: &mut PaintCtx<'_>, at: Point, width: f32) -> f64 {
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let mut y = at.y;
        let title = t("git-open-title");
        let title_h = ctx
            .measure_role(&title, TextRole::Title, Some(width))
            .height;
        ctx.text_role(
            &title,
            TextRole::Title,
            theme.text_primary,
            Point::new(at.x, y),
            Some(width),
        );
        y += title_h + f64::from(s.s2);
        let hint = t("git-open-hint");
        let hint_h = ctx.measure_role(&hint, TextRole::Body, Some(width)).height;
        ctx.text_role(
            &hint,
            TextRole::Body,
            theme.text_dim,
            Point::new(at.x, y),
            Some(width),
        );
        y + hint_h + f64::from(s.s6)
    }

    /// The "open a repository" screen: what to do, the path field and the
    /// Open button, and the reason the last attempt failed.
    pub(super) fn paint_open(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        use ui::widget::banner::{BannerContent, BannerKind};
        let theme = ctx.theme;
        let s = &theme.tokens.spacing;
        let body = self.rect;
        let lane = ui::content_layout::FocusedContentLane::new()
            .max_width(f64::from(theme.tokens.metrics.panel_width_sheet))
            .gutter(f64::from(s.s6))
            .layout(body);
        let width = ui::num::f32_of_f64(lane.width());
        let y = Self::paint_open_intro(
            ctx,
            Point::new(lane.x0, body.y0 + body.height() / 3.0),
            width,
        );

        let h = ButtonFace::height(ControlSize::Regular, theme);
        let label = t("git-open");
        let button_w = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, theme);
        // Beside the path, the system's file chooser: most people pick the
        // folder rather than type where it is.
        let choose_label = t("git-choose");
        let choose_face =
            || ButtonFace::new(Rect::ZERO, &choose_label).icon(ui::icons::Icon::FolderOpen);
        let choose_w = choose_face().intrinsic_width(ctx.text, theme);
        let controls = OpenControls::at(
            lane,
            y,
            h,
            f64::from(s.s2),
            choose_w,
            button_w,
            f64::from(theme.tokens.metrics.content_min_width),
        );
        let (field, choose, button) = (controls.field, controls.choose, controls.open);
        self.open_input.apply_layout_external(field);
        self.open_input
            .draw(ctx.scene, ctx.transform, ctx.text, theme, presence);
        self.semantic.push(Target {
            rect: field,
            hit: Hit::OpenPath,
        });
        self.targets.push(Target {
            rect: field,
            hit: Hit::OpenPath,
        });
        ui::widget::Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(choose, &choose_label)
                .icon(ui::icons::Icon::FolderOpen)
                .hovered(self.pointer.is_some_and(|p| choose.contains(p))),
        );
        self.semantic.push(Target {
            rect: choose,
            hit: Hit::ChooseRepository,
        });
        self.targets.push(Target {
            rect: choose,
            hit: Hit::ChooseRepository,
        });
        let enabled = !self.open_input.value().trim().is_empty();
        ui::widget::Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(button, &label)
                .role(ui::widget::ButtonRole::Primary)
                .hovered(enabled && self.pointer.is_some_and(|p| button.contains(p)))
                .disabled(!enabled),
        );
        self.semantic.push(Target {
            rect: button,
            hit: Hit::OpenRepository,
        });
        if enabled {
            self.targets.push(Target {
                rect: button,
                hit: Hit::OpenRepository,
            });
        }

        if let Some(error) = &self.last_error {
            let top = button.y1.max(choose.y1) + f64::from(s.s4);
            let content = BannerContent::new(BannerKind::Error, error);
            let h = Banner::height(&content, lane.width(), ctx.text, theme);
            let banner = Rect::new(lane.x0, top, lane.x1, top + h);
            Banner::paint(ctx, banner, &content, Point::new(f64::MIN, f64::MIN));
        }
    }

    /// Switch the active screen.
    pub(super) fn switch_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.set_focus(None);
        self.confirm = None;
        // The branches page opens on its first local branch, as History
        // opens on its first commit: otherwise the first arrow lands on the
        // second branch and Enter checks that one out.
        if screen == Screen::Panels && self.branch_selection.is_none() {
            let local = self.model.branches.iter().filter(|b| !b.is_remote).count();
            if local > 0 {
                self.branch_selection = Some(0);
            }
        }
    }
}
