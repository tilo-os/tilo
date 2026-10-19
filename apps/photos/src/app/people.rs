//! The People surface: its cards and controls, how they are laid out and
//! painted, and what choosing one does.

use crate::actions::PhotosEffect;
use crate::model::PhotoLibrary;
use crate::strings::{tr, tr_args, tr_count, tr_with};
use std::rc::Rc;
use ui::ArgValue;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::MediaCardFace;
use ui::widget::{Button, ButtonRole, ScrollMetrics, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::{
    LibraryDestination, PeopleControl, PeopleIntent, PeopleLayout, PeopleSelection, PhotosApp,
    bound,
};

impl PeopleControl {
    /// A person or a suggestion is opened by its card, not by a button.
    pub(super) fn is_card(&self) -> bool {
        matches!(
            self.intent,
            PeopleIntent::OpenApproved(_) | PeopleIntent::OpenSuggestion(_)
        )
    }
}

impl PhotosApp {
    /// Device-local People controller for a platform-specific face pipeline
    /// or the shared People management surface. It intentionally exposes no
    /// document reference: local clusters and their evidence never sync.
    pub fn people_controller(&self) -> &crate::people::PeopleController {
        &self.people_controller
    }

    /// Mutable local People controller. Callers use this for explicit opt-in,
    /// review, hiding, merging, and deletion of private clusters.
    pub fn people_controller_mut(&mut self) -> &mut crate::people::PeopleController {
        &mut self.people_controller
    }

    /// Commit a reviewed cluster's compact, user-approved membership through
    /// the library's verb. The local cluster stays local; the
    /// action contains only the selected display name and stable photo ids.
    pub fn approve_people_cluster(&mut self, cluster_id: &str) -> bool {
        let Some(membership) = self.people_controller.approve_membership(cluster_id) else {
            self.notice = Some(tr("photos-people-confirm-first"));
            return false;
        };
        self.commit(PhotoLibrary::SET_PERSON_MEMBERSHIP.with(membership));
        self.notice =
            Some("Approved person labels were added to the shared photo library".to_owned());
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "rebuilds every retained People control from one cluster snapshot so labels and routes cannot drift"
    )]
    pub(super) fn refresh_people_controls(&mut self) {
        let clusters: Vec<_> = self
            .people_controller
            .all_clusters()
            .map(|cluster| {
                (
                    cluster.id.clone(),
                    cluster.display_name.clone(),
                    cluster.photos.len(),
                    cluster.hidden,
                    cluster.rejected,
                )
            })
            .collect();
        let signature = format!(
            "{:?}|{}|{}|{}|{}|{}|{:?}|{:?}",
            self.people_selection,
            self.people_merge_mode,
            self.people_show_hidden,
            self.people_delete_confirm,
            self.active_people_scans.is_empty(),
            self.people_controller.privacy().recognition_enabled,
            self.people_approved
                .iter()
                .map(|person| (&person.label, person.count()))
                .collect::<Vec<_>>(),
            clusters
        );
        if signature == self.people_control_signature {
            return;
        }
        self.people_control_signature = signature;
        self.people_controls.clear();
        let actions = self.actions.clone();
        let recognition_enabled = self.people_controller.privacy().recognition_enabled;
        let mut push = |intent: PeopleIntent, label: String, primary: bool, danger: bool| {
            let button = Button::new(
                &format!("photos-people-control-{}", self.people_controls.len()),
                &label,
            )
            .with_accessible_name(&label);
            let queued = intent.clone();
            let pending = Rc::clone(&self.people_pending);
            let button = if matches!(
                &intent,
                PeopleIntent::OpenApproved(_)
                    | PeopleIntent::OpenSuggestion(_)
                    | PeopleIntent::BrowseApproved
                    | PeopleIntent::BeginMerge
            ) {
                button.on_view(move || *pending.borrow_mut() = Some(queued.clone()))
            } else {
                button.on_effect(move || *pending.borrow_mut() = Some(queued.clone()))
            };
            let mut button = bound(button, &intent.effect(), actions.as_ref());
            if primary {
                button = button.role(ButtonRole::Primary);
            } else if danger
                || matches!(
                    &intent,
                    PeopleIntent::DeleteLocalData
                        | PeopleIntent::ConfirmDeleteLocalData
                        | PeopleIntent::RejectSuggestion
                )
            {
                button = button.role(ButtonRole::Destructive);
            }
            if matches!(intent, PeopleIntent::Scan) && !recognition_enabled {
                button = button.with_enabled(false);
            }
            // The width is the button's own measure, read back from the
            // layout pass that places it.
            self.people_controls.push(PeopleControl {
                intent,
                label,
                bounds: Rect::ZERO,
                button,
                width: 0.0,
            });
        };

        if self.people_selection.is_none() {
            let recognition_on = self.people_controller.privacy().recognition_enabled;
            push(
                PeopleIntent::ToggleRecognition,
                if recognition_on {
                    tr("photos-people-recognition-off")
                } else {
                    tr("photos-people-recognition-on")
                },
                false,
                false,
            );
            if self.active_people_scans.is_empty() {
                push(PeopleIntent::Scan, tr("photos-people-scan"), true, false);
            } else {
                push(
                    PeopleIntent::CancelScan,
                    tr("photos-people-pause-scan"),
                    false,
                    false,
                );
            }
            push(
                if self.people_delete_confirm {
                    PeopleIntent::ConfirmDeleteLocalData
                } else {
                    PeopleIntent::DeleteLocalData
                },
                if self.people_delete_confirm {
                    tr("photos-people-confirm-delete-data")
                } else {
                    tr("photos-people-delete-data")
                },
                false,
                self.people_delete_confirm,
            );
            for person in &self.people_approved {
                push(
                    PeopleIntent::OpenApproved(person.label.clone()),
                    tr_args(
                        "photos-people-card-name",
                        &[
                            ("name", ArgValue::Text(person.label.clone())),
                            ("count", ArgValue::Number(person.count() as f64)),
                        ],
                    ),
                    false,
                    false,
                );
            }
            let visible_clusters: Vec<_> = self
                .people_controller
                .all_clusters()
                .filter(|cluster| self.people_show_hidden || (!cluster.hidden && !cluster.rejected))
                .map(|cluster| {
                    (
                        cluster.id.clone(),
                        cluster.display_name.clone(),
                        cluster.photos.len(),
                        cluster.hidden || cluster.rejected,
                    )
                })
                .collect();
            for (id, name, count, hidden) in visible_clusters {
                push(
                    PeopleIntent::OpenSuggestion(id),
                    tr_args(
                        if hidden {
                            "photos-people-card-hidden"
                        } else {
                            "photos-people-card-review"
                        },
                        &[
                            (
                                "name",
                                ArgValue::Text(
                                    name.unwrap_or_else(|| tr("photos-people-suggested")),
                                ),
                            ),
                            ("count", ArgValue::Number(count as f64)),
                        ],
                    ),
                    false,
                    false,
                );
            }
        } else {
            match self.people_selection.clone() {
                Some(PeopleSelection::Approved(name)) => {
                    if self.people_merge_mode {
                        for person in &self.people_approved {
                            if person.label != name {
                                push(
                                    PeopleIntent::MergeApprovedInto(person.label.clone()),
                                    tr_with("photos-people-merge-into", "name", &person.label),
                                    false,
                                    false,
                                );
                            }
                        }
                    } else {
                        push(
                            PeopleIntent::BrowseApproved,
                            tr("photos-people-browse"),
                            true,
                            false,
                        );
                        push(
                            PeopleIntent::SaveName,
                            tr("photos-people-save-name"),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::BeginMerge,
                            tr("photos-people-merge"),
                            false,
                            false,
                        );
                    }
                }
                Some(PeopleSelection::Suggestion(id)) => {
                    let hidden = self
                        .people_controller
                        .cluster(&id)
                        .is_some_and(|cluster| cluster.hidden || cluster.rejected);
                    if self.people_merge_mode {
                        let candidates: Vec<_> = self
                            .people_controller
                            .all_clusters()
                            .filter(|cluster| cluster.id != id && !cluster.rejected)
                            .map(|cluster| {
                                (
                                    cluster.id.clone(),
                                    cluster
                                        .display_name
                                        .clone()
                                        .unwrap_or_else(|| tr("photos-people-suggested")),
                                )
                            })
                            .collect();
                        for (candidate, name) in candidates {
                            push(
                                PeopleIntent::MergeSuggestionWith(candidate),
                                tr_with("photos-people-combine-with", "name", &name),
                                false,
                                false,
                            );
                        }
                    } else if hidden {
                        push(
                            PeopleIntent::RestoreSuggestion,
                            tr("photos-people-restore"),
                            true,
                            false,
                        );
                    } else {
                        push(
                            PeopleIntent::ApproveSuggestion,
                            tr("photos-people-approve"),
                            true,
                            false,
                        );
                        push(
                            PeopleIntent::HideSuggestion,
                            tr("photos-people-decide-later"),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::RejectSuggestion,
                            tr("photos-people-dismiss"),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::BeginMerge,
                            tr("photos-people-combine"),
                            false,
                            false,
                        );
                    }
                }
                None => {}
            }
        }
    }

    pub(super) fn select_people_item(&mut self, selection: PeopleSelection) {
        let initial = match &selection {
            PeopleSelection::Approved(name) => name.clone(),
            PeopleSelection::Suggestion(id) => self
                .people_controller
                .cluster(id)
                .and_then(|cluster| cluster.display_name.clone())
                .unwrap_or_default(),
        };
        self.people_selection = Some(selection);
        // Into a person's detail: a drill in (`ui::motion::Drill`).
        self.people_detail_transition.show(
            true,
            ui::motion::Drill::SPEC,
            ui::tokens::EffectiveMotion::default(),
            ui::tokens::MotionSource::Pointer,
            ui::time::now(),
        );
        self.people_merge_mode = false;
        self.people_delete_confirm = false;
        self.people_name_input.set_text(&initial);
        self.people_name_input.set_focus(false);
        self.people_control_signature.clear();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match routes every People intent to its local or shared effect"
    )]
    pub(super) fn handle_people_intent(&mut self, intent: PeopleIntent) {
        match intent {
            PeopleIntent::Back => {
                if self.people_selection.is_some() {
                    self.people_selection = None;
                    self.people_merge_mode = false;
                    self.people_detail_transition.show(
                        false,
                        ui::motion::Drill::SPEC,
                        ui::tokens::EffectiveMotion::default(),
                        ui::tokens::MotionSource::Pointer,
                        ui::time::now(),
                    );
                } else {
                    self.library_destination = LibraryDestination::Timeline;
                    self.people_mode = false;
                }
            }
            PeopleIntent::ToggleRecognition => {
                let enabled = !self.people_controller.privacy().recognition_enabled;
                self.people_controller.set_recognition_enabled(enabled);
                if enabled {
                    self.people_controller
                        .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                }
                self.notice = Some(self.people_controller.capability().summary());
            }
            PeopleIntent::Scan => {
                let invocation = tilo_actions::InvocationId::new();
                if let Err(error) = self.schedule_people_scan_batch(invocation, &self.presented()) {
                    self.notice = Some(error);
                } else {
                    self.notice = Some(tr("photos-people-scanning-notice"));
                }
            }
            PeopleIntent::CancelScan => {
                for invocation in self.active_people_scans.drain() {
                    self.active_analysis.remove(&invocation);
                    self.cancelled_analysis.insert(invocation);
                    self.pending_people_scans.remove(&invocation);
                }
                "cancelled".clone_into(&mut self.people_scan_progress.status);
                self.people_scan_progress.save();
                self.notice = Some(tr("photos-people-scan-paused"));
            }
            PeopleIntent::OpenApproved(name) => {
                self.select_people_item(PeopleSelection::Approved(name));
            }
            PeopleIntent::OpenSuggestion(id) => {
                self.select_people_item(PeopleSelection::Suggestion(id));
            }
            PeopleIntent::BrowseApproved => {
                if let Some(PeopleSelection::Approved(name)) = &self.people_selection {
                    self.active_person = Some(name.clone());
                    self.library_destination = LibraryDestination::Timeline;
                    self.people_mode = false;
                    self.timeline_scroll_y = 0.0;
                }
            }
            PeopleIntent::SaveName => {
                if let Some(PeopleSelection::Approved(from)) = self.people_selection.clone() {
                    let to = self.people_name_input.value().trim().to_owned();
                    if to.is_empty() {
                        self.notice = Some(tr("photos-people-name-first"));
                    } else {
                        self.commit(PhotoLibrary::RENAME_PERSON.with((from, to.clone())));
                        self.people_selection = Some(PeopleSelection::Approved(to));
                        self.notice = Some(tr("photos-people-renamed"));
                    }
                }
            }
            PeopleIntent::BeginMerge => self.people_merge_mode = true,
            PeopleIntent::MergeApprovedInto(to) => {
                if let Some(PeopleSelection::Approved(from)) = self.people_selection.clone() {
                    self.commit(PhotoLibrary::RENAME_PERSON.with((from, to)));
                    self.people_selection = None;
                    self.people_merge_mode = false;
                    self.notice = Some(tr("photos-people-merged"));
                }
            }
            PeopleIntent::ApproveSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let name = self.people_name_input.value().trim().to_owned();
                    if matches!(
                        self.people_controller.confirm_suggestion(&id, &name),
                        crate::people::PeopleChange::Unchanged
                    ) || !self.approve_people_cluster(&id)
                    {
                        self.notice = Some(tr("photos-people-name-before-approving"));
                    } else {
                        let _ = self.people_controller.set_hidden(&id, true);
                        self.people_selection = None;
                    }
                }
            }
            PeopleIntent::RejectSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.reject_suggestion(&id);
                    self.people_selection = None;
                    self.notice = Some(tr("photos-people-dismissed"));
                }
            }
            PeopleIntent::HideSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.set_hidden(&id, true);
                    self.people_selection = None;
                    self.notice = Some(tr("photos-people-hidden"));
                }
            }
            PeopleIntent::RestoreSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.restore_suggestion(&id);
                    self.people_selection = None;
                    self.notice = Some(tr("photos-people-restored"));
                }
            }
            PeopleIntent::MergeSuggestionWith(source) => {
                if let Some(PeopleSelection::Suggestion(destination)) =
                    self.people_selection.clone()
                {
                    let _ = self.people_controller.merge_clusters(&destination, &source);
                    self.people_merge_mode = false;
                    self.notice = Some(tr("photos-people-combined"));
                }
            }
            PeopleIntent::Page(forward) => {
                let step = self.people_layout.lanes.height();
                let max = (self.people_layout.lanes_content - step).max(0.0);
                self.people_scroll =
                    (self.people_scroll + if forward { step } else { -step }).clamp(0.0, max);
            }
            PeopleIntent::ToggleHidden => {
                self.people_show_hidden = !self.people_show_hidden;
                self.people_scroll = 0.0;
            }
            PeopleIntent::DeleteLocalData => {
                self.people_delete_confirm = true;
                self.notice = Some(tr("photos-people-confirm-delete-notice"));
            }
            PeopleIntent::ConfirmDeleteLocalData => {
                self.people_controller.delete_all_local_biometric_data();
                self.people_delete_confirm = false;
                self.people_selection = None;
                self.notice = Some(tr("photos-people-deleted-data"));
            }
        }
        self.people_control_signature.clear();
    }

    /// Pose one real local suggestion for the deterministic visual gallery.
    /// The named cluster must already exist; this never invents recognition
    /// output or runs on a live launch.
    pub fn show_people_suggestion_for_shot(&mut self, cluster_id: &str) {
        self.show_people();
        if self.people_controller.cluster(cluster_id).is_some() {
            self.select_people_item(PeopleSelection::Suggestion(cluster_id.to_owned()));
            // Acceptance shots exercise the settled visual state. Live entry
            // still starts the shared transition in `select_people_item`.
            self.people_detail_transition = ui::motion::Presence::opened();
        }
    }

    /// The side and gap of the People surface's cards at `width`.
    fn people_grid(theme: &Theme, width: f64) -> (usize, f64, f64) {
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        let extent = ui::widget::MediaSize::Small.extent(theme);
        let columns = ui::widget::card_columns(width, extent, gap).max(1);
        (
            columns,
            ui::widget::card_column_width(width, columns, gap),
            gap,
        )
    }

    /// Place the People surface: the recognition section and its controls,
    /// then the people you named and the suggestions to review, which scroll
    /// under the section; the photographs, the name field and the decisions
    /// in a person's detail. Controls are placed with their measured widths,
    /// so paint and hit-testing read the same rects.
    #[expect(
        clippy::too_many_lines,
        reason = "the overview and the detail are laid out in one pass so hit targets and paint agree"
    )]
    pub(super) fn layout_people_controls(&mut self, theme: &Theme) {
        let frame = self.content_frame(theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let (related, sibling, group) = (
            f64::from(rhythm.related),
            f64::from(rhythm.sibling),
            f64::from(rhythm.group),
        );
        let heading = Self::line_of(theme, ui::TextRole::Heading);
        let body = Self::line_of(theme, ui::TextRole::Body);
        let caption = Self::line_of(theme, ui::TextRole::Caption);
        let control_h = ui::widget::ControlSize::Regular.height(theme);
        let mut layout = PeopleLayout::default();
        self.people_name_bounds = Rect::ZERO;
        for control in &mut self.people_controls {
            control.bounds = Rect::ZERO;
        }

        let mut y = frame.y0;
        if self.people_selection.is_none() {
            layout.recognition_heading = Some(Point::new(frame.x0, y));
            y += heading + related;
            layout.recognition_text = Rect::new(frame.x0, y, frame.x1, y + body);
            y += body + sibling;
            // The recognition controls lead the row; deleting the face data
            // stands apart at its trailing end, or under them where the row
            // has no room for it.
            let mut x = frame.x0;
            let mut row_y = y;
            for control in &mut self.people_controls {
                match control.intent {
                    PeopleIntent::ToggleRecognition
                    | PeopleIntent::Scan
                    | PeopleIntent::CancelScan => {
                        control.bounds = Rect::new(x, row_y, x + control.width, row_y + control_h);
                        x = control.bounds.x1 + related;
                    }
                    PeopleIntent::DeleteLocalData | PeopleIntent::ConfirmDeleteLocalData => {
                        let fits = x + group + control.width <= frame.x1;
                        if !fits {
                            row_y += control_h + related;
                        }
                        let x0 = if fits {
                            frame.x1 - control.width
                        } else {
                            frame.x0
                        };
                        control.bounds =
                            Rect::new(x0, row_y, x0 + control.width, row_y + control_h);
                    }
                    _ => {}
                }
            }
            y = row_y + control_h + group;

            // People and suggestions side by side where two lanes of cards
            // fit, one above the other where they do not; both scroll in the
            // rest of the column, laid out from its top and moved by the
            // scroll offset afterwards.
            let lanes_top = y;
            let viewport = Rect::new(frame.x0, lanes_top, frame.x1, self.canvas_rect.y1);
            let extent = ui::widget::MediaSize::Small.extent(theme);
            let side_by_side = frame.width() >= 2.0 * extent + group;
            let lanes = if side_by_side {
                let mid = frame.x0 + (frame.width() - group) / 2.0;
                [(frame.x0, mid), (mid + group, frame.x1)]
            } else {
                [(frame.x0, frame.x1), (frame.x0, frame.x1)]
            };
            let label_h = related + body + 2.0 * caption;
            let mut y = 0.0;
            let mut lanes_bottom = 0.0_f64;
            let mut placed: Vec<(usize, Rect)> = Vec::new();
            let mut headings = Vec::with_capacity(2);
            let mut empties = Vec::with_capacity(2);
            for (is_member, (x0, x1)) in [
                PeopleIntent::OpenApproved(String::new()),
                PeopleIntent::OpenSuggestion(String::new()),
            ]
            .into_iter()
            .zip(lanes)
            {
                if side_by_side {
                    y = 0.0;
                }
                let (columns, side, gap) = Self::people_grid(theme, x1 - x0);
                let card_h = side + label_h;
                headings.push(Point::new(x0, y));
                y += heading + related;
                let mut index = 0usize;
                for (position, control) in self.people_controls.iter().enumerate() {
                    if std::mem::discriminant(&control.intent) != std::mem::discriminant(&is_member)
                    {
                        continue;
                    }
                    let column = index % columns;
                    let row = index / columns;
                    let x = x0 + column as f64 * (side + gap);
                    let top = y + row as f64 * (card_h + group);
                    placed.push((position, Rect::new(x, top, x + side, top + card_h)));
                    index += 1;
                }
                if index == 0 {
                    empties.push(Some(Rect::new(x0, y, x1, y + 2.0 * body)));
                    y += 2.0 * body + group;
                } else {
                    empties.push(None);
                    y += index.div_ceil(columns) as f64 * (card_h + group);
                }
                lanes_bottom = lanes_bottom.max(y);
            }
            // The last row's gap becomes the page's bottom margin.
            let content_h = lanes_bottom - group + f64::from(rhythm.section);
            self.people_scroll = self
                .people_scroll
                .clamp(0.0, (content_h - viewport.height()).max(0.0));
            let shift = Vec2::new(0.0, lanes_top - self.people_scroll);
            for (position, rect) in placed {
                if let Some(control) = self.people_controls.get_mut(position) {
                    control.bounds = rect + shift;
                }
            }
            let mut headings = headings.into_iter().map(|at| at + shift);
            layout.people_heading = headings.next();
            layout.suggestions_heading = headings.next();
            let mut empties = empties
                .into_iter()
                .map(|rect| rect.map(|rect| rect + shift));
            layout.people_empty = empties.next().flatten();
            layout.suggestions_empty = empties.next().flatten();
            layout.lanes = viewport;
            layout.lanes_content = content_h;
        } else {
            let gap = f64::from(rhythm.related);
            let side = ((frame.width() - 3.0 * gap) / 4.0)
                .min(ui::widget::MediaSize::Small.extent(theme))
                .max(1.0);
            layout.detail_photos = (0..4)
                .map(|index| {
                    let x = frame.x0 + f64::from(index) * (side + gap);
                    Rect::new(x, y, x + side, y + side)
                })
                .filter(|rect| rect.x1 <= frame.x1 + 0.5)
                .collect();
            y += side + group;
            // A named person's name is the header's title; a suggestion asks
            // who it is, and merging says what is being merged.
            let titled = self.people_merge_mode
                || matches!(self.people_selection, Some(PeopleSelection::Suggestion(_)));
            if titled {
                layout.detail_title = Some(Point::new(frame.x0, y));
                y += Self::line_of(theme, ui::TextRole::Title) + f64::from(rhythm.tight);
            }
            layout.detail_text = Rect::new(frame.x0, y, frame.x1, y + 2.0 * body);
            y += 2.0 * body + group;
            let hidden = matches!(
                &self.people_selection,
                Some(PeopleSelection::Suggestion(id))
                    if self
                        .people_controller
                        .cluster(id)
                        .is_none_or(|cluster| cluster.hidden || cluster.rejected)
            );
            if !self.people_merge_mode && !hidden {
                layout.name_label = Some(Point::new(frame.x0, y));
                y += Self::line_of(theme, ui::TextRole::Label) + f64::from(rhythm.tight);
                let field_w = frame
                    .width()
                    .min(f64::from(theme.tokens.metrics.panel_width_list));
                self.people_name_bounds = Rect::new(frame.x0, y, frame.x0 + field_w, y + control_h);
                y += control_h + sibling;
            }
            let mut x = frame.x0;
            for control in &mut self.people_controls {
                if self.people_merge_mode {
                    control.bounds =
                        Rect::new(frame.x0, y, frame.x0 + control.width, y + control_h);
                    y += control_h + related;
                } else {
                    if x > frame.x0 && x + control.width > frame.x1 {
                        x = frame.x0;
                        y += control_h + related;
                    }
                    control.bounds = Rect::new(x, y, x + control.width, y + control_h);
                    x = control.bounds.x1 + related;
                }
            }
        }
        self.people_layout = layout;
    }

    /// The People surface: who you named and what this device suggests, as
    /// media cards; one person's photographs and decisions in the detail.
    /// It never paints a made-up suggestion when no face model is installed.
    pub(super) fn draw_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        let layout = self.people_layout.clone();
        let frame = self.content_frame(theme);
        // What does not fit stays inside the content column.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.canvas_rect,
        );
        self.paint_people(scene, transform, text_renderer, theme, &layout, frame);
        scene.pop_layer();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the People overview and detail paint from one shared layout"
    )]
    fn paint_people(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &PeopleLayout,
        frame: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);

        if let Some(selection) = self.people_selection.clone() {
            let (title, summary, photo_ids, private, hidden) = match &selection {
                PeopleSelection::Approved(name) => {
                    let ids = self
                        .people_approved
                        .iter()
                        .find(|person| &person.label == name)
                        .map_or_else(Vec::new, |person| person.photo_ids.clone());
                    (
                        name.clone(),
                        tr_count("photos-people-approved-summary", ids.len()),
                        ids,
                        false,
                        false,
                    )
                }
                PeopleSelection::Suggestion(id) => self.people_controller.cluster(id).map_or_else(
                    || {
                        (
                            tr("photos-people-unavailable-title"),
                            tr("photos-people-unavailable-body"),
                            Vec::new(),
                            true,
                            true,
                        )
                    },
                    |cluster| {
                        (
                            cluster
                                .display_name
                                .clone()
                                .unwrap_or_else(|| tr("photos-people-who")),
                            tr_count("photos-people-suggestion-summary", cluster.photos.len()),
                            cluster.visible_photo_ids(),
                            true,
                            cluster.hidden || cluster.rejected,
                        )
                    },
                ),
            };
            let reveal = if theme.reduce_motion {
                1.0_f32
            } else {
                ui::motion::unit_opacity(
                    self.people_detail_transition
                        .progress(ui::motion::frame_now()),
                )
            };
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                reveal,
                transform,
                &self.canvas_rect,
            );
            for (rect, photo_id) in layout.detail_photos.iter().zip(photo_ids.iter()) {
                let art = self.thumbnail_for(photo_id).map_or(
                    ui::widget::MediaArt::Placeholder,
                    ui::widget::MediaArt::Image,
                );
                MediaCardFace::new(art)
                    .aspect(ui::widget::MediaAspect::Square)
                    .paint(&mut ctx, *rect);
            }
            if let Some(at) = layout.detail_title {
                let title = if self.people_merge_mode {
                    if private {
                        tr("photos-people-combine-title")
                    } else {
                        tr("photos-people-merge-title")
                    }
                } else {
                    title
                };
                let width = (frame.width()) as f32;
                let style = theme.text(ui::TextRole::Title);
                let shown = ctx.text.elide_to_width_style(&title, width, &style);
                ctx.text(&shown, &style, theme.text_primary, at, None);
                if private && !self.people_merge_mode {
                    let label = tr("photos-people-only-here");
                    let title_w = f64::from(ctx.text.measure_style(&shown, None, &style).0);
                    let badge_w = ui::widget::BadgeFace::intrinsic_width(&label, ctx.text, theme);
                    let badge_h = ui::widget::BadgeFace::height(theme);
                    let line = Self::line_of(theme, ui::TextRole::Title);
                    let x = at.x + title_w + f64::from(theme.tokens.spacing.rhythm.sibling);
                    let y = at.y + (line - badge_h) / 2.0;
                    if x + badge_w <= frame.x1 {
                        ui::widget::BadgeFace::new(
                            Rect::new(x, y, x + badge_w, y + badge_h),
                            &label,
                        )
                        .paint(ctx.scene, transform, ctx.text, theme);
                    }
                }
            }
            let text = if self.people_merge_mode {
                tr("photos-people-merge-body")
            } else if hidden {
                tr("photos-people-hidden-body")
            } else {
                summary
            };
            ctx.text_role(
                &text,
                ui::TextRole::Body,
                theme.text_dim,
                Point::new(layout.detail_text.x0, layout.detail_text.y0),
                Some(layout.detail_text.width() as f32),
            );
            if let Some(at) = layout.name_label {
                ctx.text_role(
                    &tr("photos-people-name"),
                    ui::TextRole::Label,
                    theme.text_dim,
                    at,
                    None,
                );
            }
            ctx.scene.pop_layer();
            return;
        }

        if let Some(at) = layout.recognition_heading {
            ctx.text_role(
                &tr("photos-people-recognition"),
                ui::TextRole::Heading,
                theme.text_primary,
                at,
                None,
            );
        }
        let scan_state = match self.people_scan_progress.status.as_str() {
            "running" => tr("photos-people-state-running"),
            "error" => self
                .people_scan_progress
                .last_error
                .clone()
                .unwrap_or_else(|| tr("photos-people-state-error")),
            "cancelled" => tr("photos-people-state-paused"),
            _ => match self.people_controller.capability() {
                crate::people::FaceCapability::DisabledByOwner => tr("photos-people-state-off"),
                crate::people::FaceCapability::NoVerifiedFaceModel => {
                    tr("photos-people-state-no-model")
                }
                crate::people::FaceCapability::Ready { .. } => tr("photos-people-state-ready"),
            },
        };
        let text_rect = layout.recognition_text;
        let shown = ctx.text.elide_to_width_style(
            &scan_state,
            text_rect.width() as f32,
            &theme.text(ui::TextRole::Body),
        );
        ctx.text_role(
            &shown,
            ui::TextRole::Body,
            theme.text_dim,
            Point::new(text_rect.x0, text_rect.y0),
            None,
        );

        // The cards scroll under the recognition section.
        let lanes = layout.lanes;
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &Rect::new(self.canvas_rect.x0, lanes.y0, self.canvas_rect.x1, lanes.y1),
        );
        for (at, key) in [
            (layout.people_heading, "photos-people"),
            (layout.suggestions_heading, "photos-people-suggestions"),
        ] {
            if let Some(at) = at {
                ctx.text_role(
                    &tr(key),
                    ui::TextRole::Heading,
                    theme.text_primary,
                    at,
                    None,
                );
            }
        }
        for (empty, key) in [
            (layout.people_empty, "photos-people-empty"),
            (layout.suggestions_empty, "photos-people-suggestions-empty"),
        ] {
            if let Some(empty) = empty {
                ctx.text_role(
                    &tr(key),
                    ui::TextRole::Body,
                    theme.text_dim,
                    Point::new(empty.x0, empty.y0),
                    Some(empty.width() as f32),
                );
            }
        }
        let only_here = tr("photos-people-only-here");
        for control in &self.people_controls {
            let (name, count_line, private_line, photo_ids) = match &control.intent {
                PeopleIntent::OpenApproved(name) => {
                    let person = self
                        .people_approved
                        .iter()
                        .find(|person| &person.label == name);
                    (
                        name.clone(),
                        tr_count(
                            "photos-count-photos",
                            person.map_or(0, crate::people::SharedPerson::count),
                        ),
                        None,
                        person.map_or_else(Vec::new, |person| person.photo_ids.clone()),
                    )
                }
                PeopleIntent::OpenSuggestion(id) => {
                    let Some(cluster) = self.people_controller.cluster(id) else {
                        continue;
                    };
                    (
                        cluster
                            .display_name
                            .clone()
                            .unwrap_or_else(|| tr("photos-people-suggested")),
                        tr_count("photos-count-photos", cluster.photos.len()),
                        Some(only_here.as_str()),
                        cluster.visible_photo_ids(),
                    )
                }
                _ => continue,
            };
            if control.bounds.y1 < lanes.y0 || control.bounds.y0 > lanes.y1 {
                continue;
            }
            let art = photo_ids
                .first()
                .and_then(|id| self.thumbnail_for(id))
                .map_or(
                    ui::widget::MediaArt::Placeholder,
                    ui::widget::MediaArt::Image,
                );
            let mut face = MediaCardFace::new(art)
                .aspect(ui::widget::MediaAspect::Square)
                .title(&name)
                .meta(&count_line)
                .interaction(ui::widget::Interaction {
                    hovered: control.bounds.contains(self.pointer) && lanes.contains(self.pointer),
                    focused: control.button.is_focused(),
                    ..Default::default()
                });
            if let Some(line) = private_line {
                face = face.meta(line);
            }
            face.paint(&mut ctx, control.bounds);
        }
        ctx.scene.pop_layer();
        let metrics = ScrollMetrics::new(layout.lanes_content, lanes.height(), self.people_scroll);
        if metrics.overflows() {
            ui::widget::scrollbar::Scrollbar::vertical().draw(
                ctx.scene,
                transform,
                Self::scroll_track(lanes, theme),
                metrics,
                theme,
                false,
            );
        }
    }
}

impl PeopleIntent {
    fn effect(&self) -> PhotosEffect {
        match self.clone() {
            Self::Back => PhotosEffect::PeopleBack,
            Self::ToggleRecognition => PhotosEffect::TogglePeopleRecognition,
            Self::Scan => PhotosEffect::AnalyzePeopleLibrary,
            Self::CancelScan => PhotosEffect::CancelPeopleScan,
            Self::OpenApproved(person) => PhotosEffect::OpenApprovedPerson { person },
            Self::OpenSuggestion(cluster) => PhotosEffect::OpenSuggestion { cluster },
            Self::BrowseApproved => PhotosEffect::BrowseApprovedPeople,
            Self::SaveName => PhotosEffect::SavePersonName,
            Self::BeginMerge => PhotosEffect::BeginPeopleMerge,
            Self::MergeApprovedInto(person) => PhotosEffect::MergeApprovedInto { person },
            Self::ApproveSuggestion => PhotosEffect::ApproveSuggestion,
            Self::RejectSuggestion => PhotosEffect::RejectSuggestion,
            Self::HideSuggestion => PhotosEffect::HideSuggestion,
            Self::RestoreSuggestion => PhotosEffect::RestoreSuggestion,
            Self::MergeSuggestionWith(person) => PhotosEffect::MergeSuggestionWith { person },
            Self::ToggleHidden => PhotosEffect::ToggleHiddenSuggestions,
            Self::Page(forward) => PhotosEffect::PeoplePage { forward },
            Self::DeleteLocalData => PhotosEffect::DeleteLocalPeopleData,
            Self::ConfirmDeleteLocalData => PhotosEffect::ConfirmDeleteLocalPeopleData,
        }
    }
}
