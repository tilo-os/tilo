//! Importing photographs from a folder or a contents chooser: the import
//! run, its status line and its failure details.

use crate::actions::PhotosEffect;
use crate::model::PhotoLibrary;
use crate::strings::{tr, tr_args, tr_count, tr_with};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use ui::ArgValue;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::ListRowFace;
use ui::widget::{Button, ButtonFace, ModalAction, ModalSpec, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::{
    DIRECTORY_FAILURE_DETAIL_LIMIT, DIRECTORY_IMPORT_PER_FRAME, DirectoryImport,
    DirectoryImportEvent, DirectoryImportMode, DirectoryImportReceipt, DirectoryStatusHit,
    PhotosApp, bound,
};

impl PhotosApp {
    /// Pose a running folder import for hermetic offscreen visual coverage.
    /// The held sender prevents the ordinary sync loop from treating this
    /// deterministic fixture as a crashed worker.
    pub fn show_directory_import_progress_for_shot(&mut self) {
        self.show_library();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.directory_import_receipt = None;
        self.directory_import_details_open = false;
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            invocation: None,
            mode: DirectoryImportMode::Import,
            discovered: 184,
            imported: 112,
            refreshed: 0,
            unchanged: 7,
            skipped: 2,
            sequence: 184,
            pending: VecDeque::new(),
            failures: vec![
                crate::import::ImportFailure {
                    source: "Scan of album page 3.tif".into(),
                    message: tr("photos-import-unsafe"),
                    retryable: true,
                },
                crate::import::ImportFailure {
                    source: "Old phone backup".into(),
                    message: tr("photos-import-link"),
                    retryable: false,
                },
            ],
            worker_done: false,
            cancelled: false,
            limit_reached: false,
            produced_resources: Vec::new(),
            _shot_sender: Some(sender),
        });
    }

    /// Pose a completed partial import with a deliberately bounded issue
    /// receipt. It exists only for offscreen regression images.
    pub fn show_directory_import_errors_for_shot(&mut self) {
        self.show_library();
        self.directory_import = None;
        let failures = [
            ("IMG_2041.JPG", true),
            ("IMG_2047.JPG", true),
            ("DSC01183.ARW", false),
            ("IMG_2103.JPG", true),
            ("Beach panorama.tif", false),
            ("IMG_2131.JPG", true),
            ("DSC01207.ARW", false),
            ("Scan of album page 3.tif", true),
            ("IMG_2188.JPG", true),
        ]
        .into_iter()
        .map(|(name, retryable)| crate::import::ImportFailure {
            source: name.into(),
            message: if retryable {
                tr("photos-import-unreadable")
            } else {
                tr("photos-import-unsafe")
            },
            retryable,
        })
        .collect();
        self.directory_import_receipt = Some(DirectoryImportReceipt {
            mode: DirectoryImportMode::Import,
            discovered: 268,
            imported: 241,
            refreshed: 0,
            unchanged: 0,
            failures,
            failure_count: 27,
            cancelled: false,
            limit_reached: false,
        });
        self.directory_import_details_open = true;
        self.directory_import_details_scroll.set_offset(Vec2::ZERO);
    }

    pub(super) fn begin_directory_import(
        &mut self,
        path: std::path::PathBuf,
        invocation: Option<tilo_actions::InvocationId>,
        mode: DirectoryImportMode,
        retain_exif_location: bool,
    ) {
        if self.directory_import.is_some() {
            self.notice = Some(tr("photos-import-already-running"));
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let content = self.content.clone();
        let spawn = ui::rt::spawn_thread("photos-directory-import", move || {
            let result = crate::import::index_directory(
                &path,
                &content,
                crate::import::ImportOptions {
                    retain_exif_location,
                    ..crate::import::ImportOptions::default()
                },
                &worker_cancel,
                |progress| {
                    let _ = sender.send(DirectoryImportEvent::Progress(progress));
                },
            );
            let _ = sender.send(DirectoryImportEvent::Finished(result));
        });
        if spawn.is_err() {
            self.notice = Some(tr("photos-import-could-not-start"));
            if let Some(invocation) = invocation {
                self.settle(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.directory_worker_unavailable".into(),
                        message: tr("photos-import-could-not-start"),
                        details: None,
                        retryable: true,
                    },
                });
            }
            return;
        }
        self.directory_import_receipt = None;
        self.directory_import_details_open = false;
        self.directory_import_details_scroll.set_offset(Vec2::ZERO);
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel,
            invocation,
            mode,
            discovered: 0,
            imported: 0,
            refreshed: 0,
            unchanged: 0,
            skipped: 0,
            sequence: 0,
            pending: VecDeque::new(),
            failures: Vec::new(),
            worker_done: false,
            cancelled: false,
            limit_reached: false,
            produced_resources: Vec::new(),
            _shot_sender: None,
        });
        self.notice = Some(match mode {
            DirectoryImportMode::Import => tr("photos-import-preparing"),
            DirectoryImportMode::Reindex => tr("photos-reindex-preparing"),
        });
    }

    fn stop_directory_import(&mut self) -> bool {
        let Some(job) = self.directory_import.as_mut() else {
            return false;
        };
        job.cancel.store(true, Ordering::Release);
        job.cancelled = true;
        job.pending.clear();
        if let Some(invocation) = job.invocation.take() {
            self.settle(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(tr("photos-import-stopped-notice")),
            });
        }
        self.resource_invocation = None;
        self.notice = Some(tr("photos-import-stopping"));
        true
    }

    pub(super) fn activate_directory_status(&mut self, hit: DirectoryStatusHit) -> bool {
        match hit {
            DirectoryStatusHit::Stop => self.stop_directory_import(),
            DirectoryStatusHit::Details => {
                let has_failures = self
                    .directory_import
                    .as_ref()
                    .is_some_and(|job| !job.failures.is_empty())
                    || self
                        .directory_import_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.failure_count > 0);
                if has_failures {
                    self.directory_import_details_open = true;
                    self.directory_import_details_scroll.set_offset(Vec2::ZERO);
                }
                has_failures
            }
            DirectoryStatusHit::Dismiss => {
                if self.directory_import.is_some() || self.directory_import_receipt.is_none() {
                    return false;
                }
                self.directory_import_receipt = None;
                self.directory_import_details_open = false;
                self.directory_import_details_layout = None;
                true
            }
        }
    }

    pub(super) fn close_directory_details(&mut self) -> bool {
        if !self.directory_import_details_open {
            return false;
        }
        self.directory_import_details_open = false;
        self.directory_import_details_layout = None;
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "applies every directory import event kind and the final summary from one worker drain"
    )]
    pub(super) fn drain_directory_import(&mut self) {
        if self.directory_import.is_none() {
            return;
        }
        loop {
            let event = self
                .directory_import
                .as_ref()
                .map(|job| job.receiver.try_recv());
            match event {
                None => return,
                Some(Ok(DirectoryImportEvent::Progress(progress))) => {
                    let Some(job) = self.directory_import.as_mut() else {
                        return;
                    };
                    job.discovered = progress.discovered;
                    job.skipped = progress.skipped;
                    job.sequence = job.sequence.saturating_add(1);
                    let (invocation, sequence) = (job.invocation, job.sequence);
                    if let Some(invocation) = invocation {
                        self.settle(tilo_actions::InvocationUpdate::Progress {
                            invocation,
                            sequence,
                            phase: "indexing".into(),
                            completed: Some(progress.indexed as u64),
                            total: None,
                            message: Some(tr_args(
                                "photos-import-found-progress",
                                &[
                                    ("found", ArgValue::Number(progress.discovered as f64)),
                                    ("issues", ArgValue::Number(progress.skipped as f64)),
                                ],
                            )),
                        });
                    }
                }
                Some(Ok(DirectoryImportEvent::Finished(Ok(report)))) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        job.discovered = report.discovered;
                        job.skipped = report.failures.len();
                        let cancellation_was_requested = job.cancelled;
                        job.cancelled |= report.cancelled;
                        job.limit_reached = report.limit_reached;
                        job.failures = report.failures;
                        if !cancellation_was_requested {
                            job.pending = report.photos.into();
                        }
                        job.worker_done = true;
                    }
                    break;
                }
                Some(Ok(DirectoryImportEvent::Finished(Err(message)))) => {
                    let Some(mut job) = self.directory_import.take() else {
                        return;
                    };
                    self.notice = Some(message.clone());
                    if let Some(invocation) = job.invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_index_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                    return;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => break,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    let Some(mut job) = self.directory_import.take() else {
                        return;
                    };
                    let message = tr("photos-import-worker-stopped");
                    self.notice = Some(message.clone());
                    if let Some(invocation) = job.invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_worker_stopped".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                    return;
                }
            }
        }

        for _ in 0..DIRECTORY_IMPORT_PER_FRAME {
            let indexed = self
                .directory_import
                .as_mut()
                .and_then(|job| job.pending.pop_front());
            let Some(indexed) = indexed else { break };
            let existing = self.presented().photo(&indexed.photo.id).cloned();
            let mode = self
                .directory_import
                .as_ref()
                .map_or(DirectoryImportMode::Import, |job| job.mode);
            let (action, changed) = match (mode, existing.as_ref()) {
                (DirectoryImportMode::Reindex, Some(current)) => {
                    let capture_changes =
                        current.captured_at.is_none() && indexed.photo.captured_at.is_some();
                    let location_changes = current.location.as_ref().is_none_or(|location| {
                        location.provenance.source == "exif"
                            && current.location != indexed.photo.location
                    });
                    (
                        PhotoLibrary::REFRESH_IMPORTED_METADATA.with((
                            indexed.photo.item(),
                            indexed.photo.captured_at.clone(),
                            indexed.photo.location.clone(),
                        )),
                        capture_changes || location_changes,
                    )
                }
                (_, Some(_)) => (crate::resource::import(&indexed.photo), false),
                (_, None) => (crate::resource::import(&indexed.photo), true),
            };
            if !changed {
                if let Some(job) = self.directory_import.as_mut() {
                    job.unchanged = job.unchanged.saturating_add(1);
                }
                continue;
            }
            match self.submit(action) {
                Ok(()) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        if existing.is_some() {
                            job.refreshed = job.refreshed.saturating_add(1);
                        } else {
                            job.imported = job.imported.saturating_add(1);
                        }
                        job.produced_resources.push(indexed.photo.id);
                    }
                }
                Err(error) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        job.skipped = job.skipped.saturating_add(1);
                        job.failures.push(crate::import::ImportFailure {
                            source: indexed.source,
                            message: error,
                            retryable: false,
                        });
                    }
                }
            }
        }

        if let Some(mut job) = self
            .directory_import
            .take_if(|job| job.worker_done && job.pending.is_empty())
        {
            let message = tr_args(
                if job.mode == DirectoryImportMode::Import {
                    "photos-import-finished-notice"
                } else {
                    "photos-reindex-finished-notice"
                },
                &[
                    ("imported", ArgValue::Number(job.imported as f64)),
                    ("issues", ArgValue::Number(job.failures.len() as f64)),
                ],
            );
            self.notice = Some(message);
            let failure_count = job.failures.len();
            self.directory_import_receipt = Some(DirectoryImportReceipt {
                mode: job.mode,
                discovered: job.discovered,
                imported: job.imported,
                refreshed: job.refreshed,
                unchanged: job.unchanged,
                failures: job
                    .failures
                    .iter()
                    .take(DIRECTORY_FAILURE_DETAIL_LIMIT)
                    .cloned()
                    .collect(),
                failure_count,
                cancelled: job.cancelled,
                limit_reached: job.limit_reached,
            });
            if let Some(invocation) = job.invocation.take() {
                let failures: Vec<_> = job
                    .failures
                    .iter()
                    .take(50)
                    .map(|failure| serde_json::json!({
                        "name": failure.source.file_name().and_then(|name| name.to_str()).map_or_else(|| tr("photos-import-unnamed"), str::to_owned),
                        "message": failure.message,
                        "retryable": failure.retryable,
                    }))
                    .collect();
                self.settle(if job.cancelled {
                    tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(tr("photos-import-cancelled-notice")),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!({
                                "operation": if job.mode == DirectoryImportMode::Import {
                                    "import"
                                } else {
                                    "reindex"
                                },
                                "discovered": job.discovered,
                                "imported": job.imported,
                                "refreshed": job.refreshed,
                                "unchanged": job.unchanged,
                                "failed": job.failures.len(),
                                "limit_reached": job.limit_reached,
                                "failures": failures,
                            })),
                            resources: job.produced_resources,
                        },
                    }
                });
            }
            self.resource_invocation = None;
        }
    }

    /// Collect a chosen file from the portal, if one has arrived.
    #[expect(
        clippy::too_many_lines,
        reason = "drains every portal picker channel with its outcome handling side by side"
    )]
    pub(super) fn drain_picker(&mut self) {
        self.drain_contents_picker();
        if let Some(rx) = self.directory_picker.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(path)) => {
                    self.directory_picker = None;
                    let invocation = self.resource_invocation.take();
                    self.begin_directory_import(
                        path,
                        invocation,
                        DirectoryImportMode::Import,
                        self.directory_picker_retain_exif_location,
                    );
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => {
                    self.directory_picker = None;
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some(tr("photos-import-pick-cancelled")),
                        });
                    }
                }
                Ok(ui::portal::PortalOutcome::Failed(message)) => {
                    self.directory_picker = None;
                    self.notice = Some(tr_with("photos-import-pick-failed", "reason", &message));
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_portal_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.directory_picker = None;
                }
            }
        }
        let Some(rx) = self.picker.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.picker = None;
                self.import_content = None;
                if self.picker_stale {
                    // The chooser already ran. Settle its real selection outcome
                    // without importing into content changed since dispatch.
                    let message = tr_with(
                        "photos-import-stale-named",
                        "name",
                        &path.file_name().map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        ),
                    );
                    self.notice = Some(message.clone());
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.stale_selection".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                    return;
                }
                let path = path.to_string_lossy().into_owned();
                self.open_path(&path);
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(if self.notice.is_none() {
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    } else {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.open_failed".into(),
                                message: tr("photos-open-failed"),
                                details: None,
                                retryable: false,
                            },
                        }
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.picker = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(tr("photos-open-cancelled")),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.picker = None;
                self.notice = Some(tr("photos-picker-failed"));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_disconnected".into(),
                            message: tr("photos-picker-failed"),
                            details: None,
                            retryable: true,
                        },
                    });
                }
            }
        }
    }

    /// The title and the one-line summary of the folder import in flight or
    /// just finished, if any.
    pub(super) fn directory_import_summary(&self) -> Option<(String, String, usize, bool)> {
        let count = |n: usize| ArgValue::Number(n as f64);
        if let Some(job) = self.directory_import.as_ref() {
            let title = if job.mode == DirectoryImportMode::Import {
                tr("photos-import-running")
            } else {
                tr("photos-reindex-running")
            };
            let summary = tr_args(
                "photos-import-progress",
                &[
                    ("imported", count(job.imported)),
                    ("found", count(job.discovered)),
                    ("issues", count(job.failures.len())),
                ],
            );
            return Some((title, summary, job.failures.len(), true));
        }
        let receipt = self.directory_import_receipt.as_ref()?;
        let title = match (receipt.cancelled, receipt.mode) {
            (true, _) => tr("photos-import-stopped"),
            (false, DirectoryImportMode::Reindex) => tr("photos-reindex-complete"),
            (false, DirectoryImportMode::Import) => tr("photos-import-complete"),
        };
        // The facts that happened, as sentences; a count of zero says nothing.
        let mut sentences = vec![tr_args(
            "photos-import-result",
            &[
                ("imported", count(receipt.imported)),
                ("found", count(receipt.discovered)),
            ],
        )];
        if receipt.refreshed + receipt.unchanged > 0 {
            sentences.push(tr_args(
                "photos-import-result-kept",
                &[
                    ("refreshed", count(receipt.refreshed)),
                    ("unchanged", count(receipt.unchanged)),
                ],
            ));
        }
        if receipt.failure_count > 0 {
            sentences.push(tr_count(
                "photos-import-result-issues",
                receipt.failure_count,
            ));
        }
        if receipt.limit_reached {
            sentences.push(tr("photos-import-limit"));
        }
        Some((title, sentences.join(" "), receipt.failure_count, false))
    }

    /// The folder import's progress or result, on the floating material at
    /// the bottom-trailing corner of the content, with its actions. While its
    /// issues are open in their dialog, the dialog stands for it.
    pub(super) fn draw_directory_import_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some((title, summary, issue_count, active)) = self
            .directory_import_summary()
            .filter(|_| !self.directory_import_details_open)
        else {
            for (_, button) in &mut self.directory_import_buttons {
                button.set_enabled(false);
                button.apply_layout_external(Rect::ZERO);
            }
            return;
        };
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s4);
        let gap = f64::from(spacing.s2);
        let outer = f64::from(spacing.s4);
        let width = (self.canvas_rect.width() - 2.0 * outer)
            .min(f64::from(theme.tokens.metrics.panel_width_list) * 1.2);
        let inner_w = width - 2.0 * pad;
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, &ink);
        let title_style = ink.text(ui::TextRole::Body).strong();
        let title_h = f64::from(
            ctx.text
                .measure_style(&title, Some(inner_w as f32), &title_style)
                .1,
        );
        let summary_h = ctx
            .measure_role(&summary, ui::TextRole::Caption, Some(inner_w as f32))
            .height;
        let progress_h = if active {
            f64::from(theme.tokens.radii.detail) * 2.0 + gap
        } else {
            0.0
        };
        let button_h = ui::widget::ControlSize::Regular.height(theme);
        let height =
            pad + title_h + f64::from(spacing.s1) + summary_h + progress_h + gap + button_h + pad;
        let panel = Rect::new(
            self.canvas_rect.x1 - outer - width,
            self.canvas_rect.y1 - outer - height,
            self.canvas_rect.x1 - outer,
            self.canvas_rect.y1 - outer,
        );
        ui::widget::floating_surface::paint(ctx.scene, transform, panel, theme);
        let mut y = panel.y0 + pad;
        ctx.text(
            &title,
            &title_style,
            ink.text_primary,
            Point::new(panel.x0 + pad, y),
            Some(inner_w as f32),
        );
        y += title_h + f64::from(spacing.s1);
        ctx.text_role(
            &summary,
            ui::TextRole::Caption,
            ink.text_dim,
            Point::new(panel.x0 + pad, y),
            Some(inner_w as f32),
        );
        y += summary_h + gap;
        if active {
            let bar = Rect::new(
                panel.x0 + pad,
                y,
                panel.x1 - pad,
                y + f64::from(theme.tokens.radii.detail) * 2.0,
            );
            self.directory_import_progress.apply_layout_external(bar);
            self.directory_import_progress.draw(
                ctx.scene,
                transform,
                ctx.text,
                &ink,
                &PresenceCtx::empty(),
            );
        }
        let visible = |hit| match hit {
            DirectoryStatusHit::Stop => active,
            DirectoryStatusHit::Details => issue_count > 0,
            DirectoryStatusHit::Dismiss => !active,
        };
        let mut x = panel.x1 - pad;
        let y0 = panel.y1 - pad - button_h;
        for (hit, button) in self.directory_import_buttons.iter_mut().rev() {
            if visible(*hit) {
                let label = directory_status_label(*hit);
                let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, &ink);
                let rect = Rect::new(x - width, y0, x, y0 + button_h);
                x = rect.x0 - gap;
                button.set_enabled(true);
                button.apply_layout_external(rect);
                button.draw(ctx.scene, transform, ctx.text, &ink, &PresenceCtx::empty());
            } else {
                button.set_enabled(false);
                button.apply_layout_external(Rect::ZERO);
            }
        }
    }

    /// The folder import's issues in a modal sheet over the window, the list
    /// scrolling in the sheet's body. Painted in the top layer.
    #[expect(
        clippy::too_many_lines,
        reason = "the sheet measures, lays out and paints its rows and actions in one pass"
    )]
    pub(super) fn draw_directory_import_details(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.directory_import_details_open {
            self.directory_import_details_layout = None;
            return;
        }
        let (failures, failure_count) = if let Some(job) = self.directory_import.as_ref() {
            (job.failures.clone(), job.failures.len())
        } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
            (receipt.failures.clone(), receipt.failure_count)
        } else {
            self.directory_import_details_open = false;
            self.directory_import_details_layout = None;
            return;
        };
        if failures.is_empty() {
            self.directory_import_details_open = false;
            self.directory_import_details_layout = None;
            return;
        }
        let retained = failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT);
        let window = presence.viewport.get().unwrap_or(self.rect);
        let max_width = f64::from(theme.tokens.metrics.panel_width_sheet);
        let body_w = ui::widget::modal_dialog::body_width(window, max_width, theme);
        let rows: Vec<(String, String, String)> = failures
            .iter()
            .take(retained)
            .map(|failure| {
                (
                    failure
                        .source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map_or_else(|| tr("photos-import-unnamed"), str::to_owned),
                    failure.message.clone(),
                    if failure.retryable {
                        tr("photos-import-can-retry")
                    } else {
                        tr("photos-import-needs-change")
                    },
                )
            })
            .collect();
        // When there were more issues than were kept, how many in all.
        let count = |n: usize| ArgValue::Number(n as f64);
        let overview = (failure_count > retained).then(|| {
            tr_args(
                "photos-import-issues-total",
                &[("kept", count(retained)), ("count", count(failure_count))],
            )
        });
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let overview_h = overview.as_deref().map_or(0.0, |overview| {
            PaintCtx::new(scene, transform, text_renderer, theme)
                .measure_role(overview, ui::TextRole::Caption, Some(body_w as f32))
                .height
                + gap
        });
        let faces: Vec<ListRowFace<'_>> = rows
            .iter()
            .enumerate()
            .map(|(index, (name, message, meta))| {
                let face = ListRowFace::new(name).secondary(message).meta(meta);
                if index + 1 < rows.len() {
                    face.separator(false)
                } else {
                    face
                }
            })
            .collect();
        let list_h: f64 = faces
            .iter()
            .map(|face| face.height(text_renderer, theme))
            .sum();
        let body_h = overview_h + list_h;
        let (close, sheet_title) = (tr("photos-close"), tr("photos-import-issues-title"));
        let actions = [ModalAction::primary(&close)];
        let base_spec = ModalSpec::new(&sheet_title)
            .actions(&actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(true);
        let layout = ui::widget::modal_dialog::layout(window, &base_spec, text_renderer, theme);
        let hovered = |index: usize| {
            layout
                .actions
                .get(index)
                .is_some_and(|rect| rect.contains(self.pointer))
        };
        let actions = [ModalAction::primary(&close).hovered(hovered(0))];
        let spec = ModalSpec::new(&sheet_title)
            .actions(&actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(true)
            .close_hovered(layout.close.is_some_and(|rect| rect.contains(self.pointer)));
        ui::widget::modal_dialog::paint_backdrop(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        let scroll = &mut self.directory_import_details_scroll;
        scroll.set_viewport(layout.body, theme);
        scroll.set_content_size(layout.body.width(), body_h);
        let width = scroll.content_rect().width();
        let visible = scroll.visible_content();
        scroll.paint(scene, transform, &sheet, |scene, content| {
            let mut ctx = PaintCtx::new(scene, content, text_renderer, &sheet);
            if let Some(overview) = overview.as_deref() {
                ctx.text_role(
                    overview,
                    ui::TextRole::Caption,
                    sheet.text_dim,
                    Point::ZERO,
                    Some(width as f32),
                );
            }
            let mut y = overview_h;
            for face in &faces {
                let h = face.height(ctx.text, &sheet);
                let row = Rect::new(0.0, y, width, y + h);
                if row.y1 >= visible.y0 && row.y0 <= visible.y1 {
                    face.paint(&mut ctx, row);
                }
                y += h;
            }
        });
        ui::widget::modal_dialog::paint_actions(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        self.directory_import_details_layout = Some(layout);
    }
}

/// What a folder import card's button says.
fn directory_status_label(hit: DirectoryStatusHit) -> String {
    tr(match hit {
        DirectoryStatusHit::Stop => "photos-import-stop",
        DirectoryStatusHit::Details => "photos-import-details",
        DirectoryStatusHit::Dismiss => "photos-import-dismiss",
    })
}

pub(super) fn build_directory_import_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(DirectoryStatusHit, Button)> {
    [
        (DirectoryStatusHit::Stop, "photos-directory-stop"),
        (DirectoryStatusHit::Details, "photos-directory-details"),
        (DirectoryStatusHit::Dismiss, "photos-directory-dismiss"),
    ]
    .into_iter()
    .map(|(hit, id)| {
        let label = directory_status_label(hit);
        let button = Button::new(id, &label)
            .tooltip(&label)
            .with_accessible_name(&label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

impl DirectoryStatusHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::Stop => PhotosEffect::StopImport,
            Self::Details => PhotosEffect::ShowImportDetails,
            Self::Dismiss => PhotosEffect::DismissImport,
        }
    }
}

impl PhotosApp {
    /// Import the photographs a contents chooser delivered.
    fn drain_contents_picker(&mut self) {
        let Some(rx) = self.contents_picker.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::ContentsOutcome::Failed(tr("photos-picker-failed"))
            }
        };
        self.contents_picker = None;
        let stale = std::mem::take(&mut self.picker_stale);
        self.import_content = None;
        let invocation = self.resource_invocation.take();
        let result = match outcome {
            ui::portal::ContentsOutcome::Picked(files) if stale => {
                let message = tr_count("photos-import-stale-count", files.len());
                self.notice = Some(message.clone());
                Err(("photos.stale_selection", message))
            }
            ui::portal::ContentsOutcome::Picked(files) => {
                for file in files {
                    self.open_contents(&file.name, &file.bytes);
                }
                if self.notice.is_none() {
                    Ok(())
                } else {
                    Err(("photos.open_failed", tr("photos-open-failed")))
                }
            }
            ui::portal::ContentsOutcome::Cancelled => {
                if let Some(invocation) = invocation {
                    self.settle(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(tr("photos-open-cancelled")),
                    });
                }
                return;
            }
            ui::portal::ContentsOutcome::Failed(message) => {
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                Err(("photos.portal_failed", message))
            }
        };
        if let Some(invocation) = invocation {
            self.settle(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err((code, message)) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: code.into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
    }
}
