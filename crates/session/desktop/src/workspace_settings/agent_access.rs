//! What one agent may do in the workspace, app by app: Allow, Ask first or
//! Deny for reading, changes and outside effects.
//!
//! Each answer is an ordinary room grant ([`agent_answers`]); without one the
//! agent's admission decides, and the view says so. Actions whose contract
//! asks for confirmation (sending, publishing, deleting for good) still ask
//! first under an app-wide Allow; only "Always allow this" on the action
//! itself lifts that, and those exceptions are listed here to take back.

use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{ActionKind, Answer, Subject, decide};
use tilo_desktop_model::{UserId, Workspace};
use ui::paint::PaintCtx;
use ui::widget::{Button, ButtonFace, ButtonRole, ChipStyle, SegmentedFace};
use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use super::{
    AppDirectory, DISABLED_ALPHA, PANE_PAD_TOP, PANE_PAD_X, PolicyEdit, REVOKE_HEIGHT, ROW_GAP,
    app_display_name, at, draw_pill, draw_row_card, elide, grant_subjects, short_id,
};
use crate::i18n::{t, t_args};
use ui::TextRole;

/// The three kinds of action, in column order.
pub const KINDS: [ActionKind; 3] = [ActionKind::Read, ActionKind::Undoable, ActionKind::Outside];

/// The three answers, in segment order.
pub const ANSWERS: [Answer; 3] = [Answer::Allow, Answer::Ask, Answer::Deny];

/// One answer the agent has for one kind of action at one scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnswerCell {
    pub kind: ActionKind,
    pub answer: Answer,
    /// The person set it here; otherwise it follows the agent's admission
    /// or a broader answer.
    pub explicit: bool,
}

/// One application's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentAccessRow {
    pub scope: AnswerScope,
    pub label: String,
    pub cells: [AnswerCell; 3],
}

impl AgentAccessRow {
    /// Whether any of its answers was set here.
    #[must_use]
    pub fn customised(&self) -> bool {
        self.cells.iter().any(|cell| cell.explicit)
    }
}

/// An action the person always allows the agent, without asking first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentException {
    pub scope: AnswerScope,
    pub kind: ActionKind,
    pub label: String,
}

/// The answer `agent` holds for `kind` at `scope`, decided over the room's
/// grants naming it alone: what the person tells this agent, before their
/// own role bounds it.
#[must_use]
pub fn answer_at(ws: &Workspace, agent: &str, scope: &AnswerScope, kind: ActionKind) -> AnswerCell {
    let subject = [Subject::agent(agent)];
    let cx = tilo_desktop_model::context_at(tilo_clock::unix_now());
    let decision = decide(
        ws.policy.grants.iter(),
        &subject,
        &scope.node(&ws.policy.room),
        kind,
        &cx,
    );
    let id = agent_answers::grant_id(agent, scope, kind);
    AnswerCell {
        kind,
        answer: decision.answer,
        explicit: decision.grant().is_some_and(|grant| grant.id == id),
    }
}

/// The apps an agent's answers are offered for: every app the workspace
/// shows, holds capabilities for, or that the agent already has answers
/// for, by display name.
#[must_use]
pub fn agent_access_rows(
    ws: &Workspace,
    agent: &str,
    apps: &dyn AppDirectory,
) -> Vec<AgentAccessRow> {
    let subject = Subject::agent(agent);
    let mut ids: Vec<String> = grant_subjects(ws)
        .into_iter()
        .filter(|candidate| candidate.kind == super::SubjectKind::App)
        .map(|candidate| candidate.id)
        .collect();
    ids.extend(
        ws.policy
            .grants
            .iter()
            .filter(|grant| grant.subject == subject && agent_answers::is_answer(grant))
            .filter_map(|grant| match AnswerScope::of_node(&grant.node)? {
                AnswerScope::App { app }
                | AnswerScope::Kind { app, .. }
                | AnswerScope::Action { app, .. } => Some(app),
            }),
    );
    ids.sort();
    ids.dedup();
    let mut rows: Vec<AgentAccessRow> = ids
        .into_iter()
        .map(|app| {
            let scope = AnswerScope::app(&app);
            AgentAccessRow {
                cells: KINDS.map(|kind| answer_at(ws, agent, &scope, kind)),
                label: app_display_name(apps, &app),
                scope,
            }
        })
        .collect();
    rows.sort_by_key(|row| row.label.to_lowercase());
    rows
}

/// The actions the person always allows `agent`.
#[must_use]
pub fn agent_exceptions(
    ws: &Workspace,
    agent: &str,
    apps: &dyn AppDirectory,
) -> Vec<AgentException> {
    let subject = Subject::agent(agent);
    let mut out: Vec<AgentException> = ws
        .policy
        .grants
        .iter()
        .filter(|grant| {
            grant.subject == subject
                && agent_answers::is_answer(grant)
                && grant.answer == Answer::Allow
        })
        .filter_map(|grant| {
            let scope = AnswerScope::of_node(&grant.node)?;
            let AnswerScope::Action { app, action, .. } = &scope else {
                return None;
            };
            let label = format!("{} · {}", app_display_name(apps, app), humanize(action));
            let kind = grant.kinds.iter().next()?;
            Some(AgentException { scope, kind, label })
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Set `agent`'s answers, each for a kind at a scope (`None` returns it to
/// what the admission and broader answers say), as `actor`, in one
/// governed edit.
pub fn set_agent_answers(
    ws: &mut Workspace,
    actor: &UserId,
    agent: &str,
    answers: &[(AnswerScope, ActionKind, Option<Answer>)],
) -> PolicyEdit {
    let room = ws.policy.room.clone();
    let by = Subject::Person(actor.clone());
    let changes: Vec<_> = answers
        .iter()
        .flat_map(|(scope, kind, answer)| {
            agent_answers::set(
                &room,
                agent,
                (scope, *kind),
                *answer,
                (by.clone(), &ws.policy.grants),
            )
        })
        .collect();
    if changes.is_empty() {
        return PolicyEdit::default();
    }
    ws.govern_all_at(actor, changes, tilo_clock::unix_now())
        .unwrap_or_default()
}

/// `send_message` as `Send message`.
fn humanize(id: &str) -> String {
    let spaced = id.replace(['_', '-', '.'], " ");
    let mut chars = spaced.trim().chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Something in the agent access view a person can press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessTarget {
    Back,
    /// Open (or close) one app's answers.
    Expand(AnswerScope),
    Answer(AnswerScope, ActionKind, Answer),
    /// Return every answer of the scope to the admission's.
    Reset(AnswerScope),
    /// Stop always allowing an action.
    RemoveException(AnswerScope, ActionKind),
}

/// One keyboard stop in the agent access view. An Allow / Ask first /
/// Deny control is one stop whose answer the arrow keys change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessFocus {
    Back,
    Expand(AnswerScope),
    Answers(AnswerScope, ActionKind),
    Reset(AnswerScope),
    RemoveException(AnswerScope, ActionKind),
}

/// The view's geometry: what was painted where.
#[derive(Clone, Debug, Default)]
pub struct AgentAccessLayout {
    pub targets: Vec<(AccessTarget, Rect)>,
    /// The keyboard stops in reading order, where they were painted.
    pub stops: Vec<(AccessFocus, Rect)>,
    /// The region the app list scrolls in, under the pinned header.
    pub viewport: Rect,
    /// The whole list's height, scrolled or not.
    pub content_height: f64,
    /// The answer each editable control showed, for the arrow keys.
    pub answers: Vec<(AnswerScope, ActionKind, Answer)>,
}

impl AgentAccessLayout {
    /// The control at `point`; the last painted wins, so a control inside
    /// a row beats the row itself. A control scrolled out of the list's
    /// region answers nothing there.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<AccessTarget> {
        self.targets
            .iter()
            .rev()
            .filter(|(target, _)| {
                matches!(target, AccessTarget::Back) || self.viewport.contains(point)
            })
            .find(|(_, rect)| rect.contains(point))
            .map(|(target, _)| target.clone())
    }

    /// How far the list scrolls at most.
    #[must_use]
    pub fn max_scroll(&self) -> f64 {
        (self.content_height - self.viewport.height()).max(0.0)
    }

    /// The answer the control for `kind` at `scope` showed.
    #[must_use]
    pub fn answer_shown(&self, scope: &AnswerScope, kind: ActionKind) -> Option<Answer> {
        self.answers
            .iter()
            .find_map(|(s, k, answer)| (s == scope && *k == kind).then_some(*answer))
    }

    /// Where `focus` was painted.
    #[must_use]
    pub fn stop_rect(&self, focus: &AccessFocus) -> Option<Rect> {
        self.stops
            .iter()
            .find_map(|(stop, rect)| (stop == focus).then_some(*rect))
    }
}

/// Which agent the view shows, which of its apps is open, how far its list
/// is scrolled and which control has the keyboard.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentAccessView<'a> {
    pub agent: &'a str,
    pub open: Option<&'a AnswerScope>,
    pub editable: bool,
    pub scroll: f64,
    pub focus: Option<&'a AccessFocus>,
}

const HEADER_HEIGHT: f64 = 44.0;
const NOTE_GAP: f64 = 10.0;
const ACCESS_ROW_HEIGHT: f64 = 52.0;
const KIND_LINE_HEIGHT: f64 = 44.0;
const SEGMENT_WIDTH: f64 = 112.0;

fn answer_word(answer: Answer) -> String {
    t(match answer {
        Answer::Allow => "workspace-settings-answer-allow",
        Answer::Ask => "workspace-settings-answer-ask",
        Answer::Deny => "workspace-settings-answer-deny",
    })
}

fn kind_word(kind: ActionKind) -> String {
    t(match kind {
        ActionKind::Read => "workspace-settings-agent-access-read",
        ActionKind::Undoable => "workspace-settings-agent-access-change",
        ActionKind::Outside => "workspace-settings-agent-access-outside",
    })
}

/// A row's answers in one line: `Reading: Allow · Changes: Allow · …`.
fn summary(row: &AgentAccessRow) -> String {
    row.cells
        .iter()
        .map(|cell| {
            t_args(
                "workspace-settings-agent-access-summary-part",
                &[
                    ("kind", kind_word(cell.kind).into()),
                    ("answer", answer_word(cell.answer).into()),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Paint the header: back, the agent's name and its AGENT mark.
fn draw_header(
    ctx: &mut PaintCtx<'_>,
    (left, right, y): (f64, f64, f64),
    (name, focused): (&str, bool),
    layout: &mut AgentAccessLayout,
) {
    let theme = ctx.theme;
    let middle = y + HEADER_HEIGHT / 2.0;
    let back_label = t("workspace-settings-agent-access-back");
    let face = |rect| {
        ButtonFace::new(rect, &back_label)
            .role(ButtonRole::Plain)
            .icon(ui::icons::Icon::ChevronLeft)
    };
    let width = face(Rect::ZERO).intrinsic_width(ctx.text, theme);
    let back = Rect::new(
        left,
        middle - REVOKE_HEIGHT / 2.0,
        left + width,
        middle + REVOKE_HEIGHT / 2.0,
    );
    Button::paint(
        ctx.scene,
        ctx.transform,
        ctx.text,
        theme,
        &face(back).focused(focused),
    );
    layout.targets.push((AccessTarget::Back, back));
    layout.stops.push((AccessFocus::Back, back));
    let badge = draw_pill(
        ctx.scene,
        ctx.transform,
        Point::new(right, y + HEADER_HEIGHT / 2.0),
        &t("workspace-settings-badge-agent"),
        ChipStyle::Neutral,
        ctx.text,
        theme,
    );
    let title_style = theme.tokens.typography.style(TextRole::Body);
    let title_x = back.x1 + 12.0;
    let title = elide(name, &title_style, badge.x0 - 12.0 - title_x, ctx.text);
    let title_h = f64::from(title_style.line_height_px());
    ctx.text.draw_style(
        ctx.scene,
        &title,
        theme.tokens.colors.text_primary,
        at(ctx.transform, title_x, y + (HEADER_HEIGHT - title_h) / 2.0),
        None,
        &title_style,
    );
}

/// Paint one app: collapsed, its answers in a line; open, a labelled
/// Allow / Ask first / Deny control for each kind. Returns its height.
fn draw_app(
    ctx: &mut PaintCtx<'_>,
    (left, right, y): (f64, f64, f64),
    row: &AgentAccessRow,
    view: &AgentAccessView<'_>,
    layout: &mut AgentAccessLayout,
) -> f64 {
    let open = view.open == Some(&row.scope);
    let height = if open {
        ACCESS_ROW_HEIGHT + KIND_LINE_HEIGHT * 3.0 + 8.0
    } else {
        ACCESS_ROW_HEIGHT
    };
    draw_row_card(
        ctx.scene,
        ctx.transform,
        Rect::new(left, y, right, y + height),
        ctx.theme,
    );
    let head = Rect::new(left, y, right, y + ACCESS_ROW_HEIGHT);
    let text_x = draw_app_head(ctx, head, row, (open, view.editable), view.focus, layout);
    if open {
        let segments_x0 = right - 12.0 - SEGMENT_WIDTH * 3.0;
        for (index, cell) in row.cells.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "three kinds")]
            let line_y = head.y1 + KIND_LINE_HEIGHT * index as f64;
            draw_kind_line(
                ctx,
                (text_x, segments_x0, line_y + KIND_LINE_HEIGHT / 2.0),
                (&row.scope, cell),
                (view.editable, view.focus),
                layout,
            );
        }
    }
    height
}

/// Record `stop` as a keyboard stop at `rect`, ringed when it has focus:
/// for a surface that is not itself a shared control.
fn push_stop(
    ctx: &mut PaintCtx<'_>,
    (stop, rect): (AccessFocus, Rect),
    focus: Option<&AccessFocus>,
    layout: &mut AgentAccessLayout,
) {
    if focus == Some(&stop) {
        ui::widget::paint_focus_ring(
            ctx.scene,
            ctx.transform,
            rect,
            f64::from(ctx.theme.tokens.radii.control),
            ctx.theme,
        );
    }
    layout.stops.push((stop, rect));
}

/// Paint an app's head: chevron, name, its answers or where they come
/// from, and Reset or Custom. Returns where its text starts.
fn draw_app_head(
    ctx: &mut PaintCtx<'_>,
    head: Rect,
    row: &AgentAccessRow,
    (open, editable): (bool, bool),
    focus: Option<&AccessFocus>,
    layout: &mut AgentAccessLayout,
) -> f64 {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let caption = theme.tokens.typography.style(TextRole::Caption);
    let line_h = f64::from(caption.line_height_px());
    layout
        .targets
        .push((AccessTarget::Expand(row.scope.clone()), head));
    push_stop(
        ctx,
        (AccessFocus::Expand(row.scope.clone()), head),
        focus,
        layout,
    );
    let chevron = Rect::new(
        head.x0 + 12.0,
        head.center().y - 8.0,
        head.x0 + 28.0,
        head.center().y + 8.0,
    );
    ui::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        chevron,
        if open {
            ui::icons::Icon::ChevronDown
        } else {
            ui::icons::Icon::ChevronRight
        },
        colors.text_dim,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    let text_x = chevron.x1 + 10.0;
    let mut text_max = head.x1 - 12.0 - text_x;
    if row.customised() && open {
        let label = t("workspace-settings-agent-access-reset");
        let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, theme);
        let reset = Rect::new(
            head.x1 - 12.0 - width,
            head.center().y - REVOKE_HEIGHT / 2.0,
            head.x1 - 12.0,
            head.center().y + REVOKE_HEIGHT / 2.0,
        );
        let stop = AccessFocus::Reset(row.scope.clone());
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(reset, &label)
                .disabled(!editable)
                .focused(editable && focus == Some(&stop)),
        );
        if editable {
            layout
                .targets
                .push((AccessTarget::Reset(row.scope.clone()), reset));
            layout.stops.push((stop, reset));
        }
        text_max = reset.x0 - 12.0 - text_x;
    } else if row.customised() {
        let chip = draw_pill(
            ctx.scene,
            ctx.transform,
            Point::new(head.x1 - 12.0, head.center().y),
            &t("workspace-settings-agent-access-custom"),
            ChipStyle::Tinted(ui::widget::Tone::Info),
            ctx.text,
            theme,
        );
        text_max = chip.x0 - 12.0 - text_x;
    }
    let top = head.y0 + (ACCESS_ROW_HEIGHT - (line_h * 2.0 + 2.0)) / 2.0;
    let label = elide(&row.label, &caption, text_max, ctx.text);
    ctx.text.draw_style(
        ctx.scene,
        &label,
        colors.text_primary,
        at(ctx.transform, text_x, top),
        None,
        &caption,
    );
    let detail = match (open, row.customised()) {
        (true, true) => t("workspace-settings-agent-access-custom-detail"),
        (true, false) => t("workspace-settings-agent-access-inherited"),
        (false, _) => summary(row),
    };
    let detail = elide(&detail, &caption, text_max, ctx.text);
    ctx.text.draw_style(
        ctx.scene,
        &detail,
        colors.text_dim,
        at(ctx.transform, text_x, top + line_h + 2.0),
        None,
        &caption,
    );
    text_x
}

/// Paint one kind's line: its name, whether its answer was chosen here,
/// and the Allow / Ask first / Deny control centred on `centre`.
fn draw_kind_line(
    ctx: &mut PaintCtx<'_>,
    (text_x, segments_x0, centre): (f64, f64, f64),
    (scope, cell): (&AnswerScope, &AnswerCell),
    (editable, focus): (bool, Option<&AccessFocus>),
    layout: &mut AgentAccessLayout,
) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let caption = theme.tokens.typography.style(TextRole::Caption);
    let line_h = f64::from(caption.line_height_px());
    let name = elide(
        &kind_word(cell.kind),
        &caption,
        segments_x0 - 12.0 - text_x,
        ctx.text,
    );
    let (name_w, _) = ctx.text.measure_style(&name, None, &caption);
    ctx.text.draw_style(
        ctx.scene,
        &name,
        colors.text_primary,
        at(ctx.transform, text_x, centre - line_h / 2.0),
        None,
        &caption,
    );
    let origin_x = text_x + f64::from(name_w) + 8.0;
    if origin_x < segments_x0 - 24.0 {
        let origin = t(if cell.explicit {
            "workspace-settings-agent-access-set-here"
        } else {
            "workspace-settings-agent-access-default"
        });
        let origin = elide(&origin, &caption, segments_x0 - 12.0 - origin_x, ctx.text);
        ctx.text.draw_style(
            ctx.scene,
            &origin,
            colors.text_dim,
            at(ctx.transform, origin_x, centre - line_h / 2.0),
            None,
            &caption,
        );
    }
    let rects: [Rect; 3] = std::array::from_fn(|segment| {
        #[allow(clippy::cast_precision_loss, reason = "three segments")]
        let x0 = segments_x0 + SEGMENT_WIDTH * segment as f64;
        Rect::new(
            x0,
            centre - REVOKE_HEIGHT / 2.0,
            x0 + SEGMENT_WIDTH,
            centre + REVOKE_HEIGHT / 2.0,
        )
    });
    let selected = ANSWERS
        .iter()
        .position(|answer| *answer == cell.answer)
        .unwrap_or(2);
    let labels = ANSWERS.map(answer_word);
    let label_refs = labels.each_ref().map(String::as_str);
    if !editable {
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            DISABLED_ALPHA,
            ctx.transform,
            &Rect::new(rects[0].x0, rects[0].y0, rects[2].x1, rects[2].y1),
        );
    }
    let stop = AccessFocus::Answers(scope.clone(), cell.kind);
    ui::widget::ToggleGroup::paint(
        ctx.scene,
        ctx.transform,
        ctx.text,
        theme,
        &SegmentedFace::new(&rects, &label_refs, selected)
            .focused(editable && focus == Some(&stop)),
    );
    if editable {
        for (segment, answer) in rects.iter().zip(ANSWERS) {
            layout.targets.push((
                AccessTarget::Answer(scope.clone(), cell.kind, answer),
                *segment,
            ));
        }
        layout.stops.push((
            stop,
            Rect::new(rects[0].x0, rects[0].y0, rects[2].x1, rects[2].y1),
        ));
        layout.answers.push((scope.clone(), cell.kind, cell.answer));
    } else {
        ctx.scene.pop_layer();
    }
}

/// Paint the actions always allowed without asking first, from `y`.
fn draw_exceptions(
    ctx: &mut PaintCtx<'_>,
    (left, right, mut y): (f64, f64, f64),
    exceptions: &[AgentException],
    (editable, focus): (bool, Option<&AccessFocus>),
    layout: &mut AgentAccessLayout,
) -> f64 {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let caption = theme.tokens.typography.style(TextRole::Caption);
    let line_h = f64::from(caption.line_height_px());
    if exceptions.is_empty() {
        return y;
    }
    ctx.text.draw_style(
        ctx.scene,
        &t_args(
            "workspace-settings-agent-access-always",
            &[("count", exceptions.len().into())],
        ),
        colors.text_dim,
        at(ctx.transform, left, y),
        None,
        &caption,
    );
    y += line_h + 8.0;
    for exception in exceptions {
        let rect = Rect::new(left, y, right, y + ACCESS_ROW_HEIGHT);
        draw_row_card(ctx.scene, ctx.transform, rect, theme);
        let stop = t("workspace-settings-agent-access-stop");
        let width = ButtonFace::new(Rect::ZERO, &stop)
            .role(ButtonRole::Destructive)
            .intrinsic_width(ctx.text, theme);
        let remove = Rect::new(
            right - 12.0 - width,
            rect.center().y - REVOKE_HEIGHT / 2.0,
            right - 12.0,
            rect.center().y + REVOKE_HEIGHT / 2.0,
        );
        let label = elide(
            &exception.label,
            &caption,
            remove.x0 - 24.0 - left,
            ctx.text,
        );
        ctx.text.draw_style(
            ctx.scene,
            &label,
            colors.text_primary,
            at(ctx.transform, left + 12.0, rect.center().y - line_h / 2.0),
            None,
            &caption,
        );
        let key = AccessFocus::RemoveException(exception.scope.clone(), exception.kind);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(remove, &stop)
                .role(ButtonRole::Destructive)
                .disabled(!editable)
                .focused(editable && focus == Some(&key)),
        );
        if editable {
            layout.targets.push((
                AccessTarget::RemoveException(exception.scope.clone(), exception.kind),
                remove,
            ));
            layout.stops.push((key, remove));
        }
        y += ACCESS_ROW_HEIGHT + ROW_GAP;
    }
    y
}

/// Paint `view`'s agent access into `pane`, returning where its controls are.
pub fn draw_agent_access(
    ctx: &mut PaintCtx<'_>,
    pane: Rect,
    (ws, apps): (&Workspace, &dyn AppDirectory),
    view: &AgentAccessView<'_>,
) -> AgentAccessLayout {
    let mut layout = AgentAccessLayout::default();
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let caption = theme.tokens.typography.style(TextRole::Caption);
    let line_h = f64::from(caption.line_height_px());
    let left = pane.x0 + PANE_PAD_X;
    let right = pane.x1 - PANE_PAD_X;
    let bottom = pane.y1 - super::PANE_PAD_BOTTOM;
    let mut y = pane.y0 + PANE_PAD_TOP;
    let name = ws
        .collaborators
        .iter()
        .find(|row| row.id == view.agent)
        .map_or_else(|| short_id(view.agent), |row| row.name.clone());
    draw_header(
        ctx,
        (left, right, y),
        (&name, view.focus == Some(&AccessFocus::Back)),
        &mut layout,
    );
    y += HEADER_HEIGHT + NOTE_GAP;

    #[allow(
        clippy::cast_possible_truncation,
        reason = "settings-pane geometry is a small logical-pixel width"
    )]
    let note_lines = ctx.text.clamp_to_lines(
        &t("workspace-settings-agent-access-note"),
        &caption,
        (right - left) as f32,
        3,
    );
    for line in &note_lines {
        ctx.text.draw_style(
            ctx.scene,
            line,
            colors.text_dim,
            at(ctx.transform, left, y),
            None,
            &caption,
        );
        y += line_h;
    }
    y += NOTE_GAP + 6.0;

    // The header and note stay put; the apps and the always-allowed
    // actions scroll under them, however many there are. The region keeps
    // a focus ring's width beside the cards so it is never clipped.
    let ring = 3.0;
    layout.viewport = Rect::new(left - ring, y - ring, right + ring, bottom);
    let top = y;
    y -= view.scroll;
    ctx.scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        ctx.transform,
        &layout.viewport,
    );
    let rows = agent_access_rows(ws, view.agent, apps);
    let exceptions = agent_exceptions(ws, view.agent, apps);
    if rows.is_empty() {
        ctx.text.draw_style(
            ctx.scene,
            &t("workspace-settings-agent-access-empty"),
            colors.text_dim,
            at(ctx.transform, left, y),
            None,
            &caption,
        );
        y += line_h + ROW_GAP;
    }
    for row in &rows {
        y += draw_app(ctx, (left, right, y), row, view, &mut layout) + ROW_GAP;
    }
    let end = draw_exceptions(
        ctx,
        (left, right, y + 8.0),
        &exceptions,
        (view.editable, view.focus),
        &mut layout,
    );
    ctx.scene.pop_layer();
    layout.content_height = end.max(y) + view.scroll - top;
    layout
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const AGENT: &str = "scribe";
    const MAIL: &str = "so.tilo.mail";

    fn ws() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from(31), "access");
        ws.set_owner(OWNER);
        let edit = super::super::admit_agent(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            super::super::AgentPreset::Operator,
        );
        assert!(!edit.is_empty());
        ws
    }

    /// The workspace with answers for `count` apps, so the agent view
    /// lists more apps than fit.
    fn crowded(count: usize) -> Workspace {
        let mut ws = ws();
        let answers: Vec<_> = (0..count)
            .map(|index| {
                (
                    AnswerScope::app(format!("so.tilo.app{index:02}")),
                    ActionKind::Read,
                    Some(Answer::Deny),
                )
            })
            .collect();
        set_agent_answers(&mut ws, &OWNER.to_string(), AGENT, &answers);
        ws
    }

    fn paint(
        ws: &Workspace,
        pane: Rect,
        (scroll, focus): (f64, Option<&AccessFocus>),
    ) -> AgentAccessLayout {
        let mut scene = vello::Scene::new();
        let mut text = ui::text_render::TextRenderer::new();
        let theme = ui::Theme::dark();
        draw_agent_access(
            &mut PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            ),
            pane,
            (ws, &super::super::NoAppDirectory),
            &AgentAccessView {
                agent: AGENT,
                open: None,
                editable: true,
                scroll,
                focus,
            },
        )
    }

    #[test]
    fn a_long_app_list_scrolls_to_its_last_app_and_only_answers_where_shown() {
        let ws = crowded(24);
        let pane = Rect::new(0.0, 0.0, 640.0, 520.0);
        let last = AccessTarget::Expand(AnswerScope::app("so.tilo.app23"));
        let top = paint(&ws, pane, (0.0, None));
        assert!(top.max_scroll() > 0.0, "24 apps do not fit 520 px");
        let painted_below = top
            .targets
            .iter()
            .find_map(|(target, rect)| (*target == last).then_some(*rect))
            .expect("every app is painted, scrolled or not");
        assert!(painted_below.y0 > top.viewport.y1);
        assert_eq!(
            top.hit(painted_below.center()),
            None,
            "an app scrolled out of view answers nothing"
        );
        let end = paint(&ws, pane, (top.max_scroll(), None));
        let shown = end
            .targets
            .iter()
            .find_map(|(target, rect)| (*target == last).then_some(*rect))
            .unwrap();
        assert!(end.viewport.contains(shown.center()));
        assert_eq!(end.hit(shown.center()), Some(last));
        assert_eq!(
            end.hit(end.stop_rect(&AccessFocus::Back).unwrap().center()),
            Some(AccessTarget::Back),
            "the header stays put"
        );
    }

    #[test]
    fn every_answer_control_is_one_keyboard_stop() {
        let mut ws = ws();
        let mail = AnswerScope::app(MAIL);
        set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[(mail.clone(), ActionKind::Outside, Some(Answer::Deny))],
        );
        let mut scene = vello::Scene::new();
        let mut text = ui::text_render::TextRenderer::new();
        let theme = ui::Theme::dark();
        let layout = draw_agent_access(
            &mut PaintCtx::new(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            ),
            Rect::new(0.0, 0.0, 640.0, 520.0),
            (&ws, &super::super::NoAppDirectory),
            &AgentAccessView {
                agent: AGENT,
                open: Some(&mail),
                editable: true,
                scroll: 0.0,
                focus: None,
            },
        );
        let stops: Vec<_> = layout.stops.iter().map(|(stop, _)| stop.clone()).collect();
        assert_eq!(
            stops,
            [
                AccessFocus::Back,
                AccessFocus::Expand(mail.clone()),
                AccessFocus::Reset(mail.clone()),
                AccessFocus::Answers(mail.clone(), ActionKind::Read),
                AccessFocus::Answers(mail.clone(), ActionKind::Undoable),
                AccessFocus::Answers(mail.clone(), ActionKind::Outside),
            ]
        );
        assert_eq!(
            layout.answer_shown(&mail, ActionKind::Outside),
            Some(Answer::Deny)
        );
    }

    #[test]
    fn answers_follow_the_admission_until_set_here() {
        let mut ws = ws();
        let mail = AnswerScope::app(MAIL);
        let read = answer_at(&ws, AGENT, &mail, ActionKind::Read);
        let outside = answer_at(&ws, AGENT, &mail, ActionKind::Outside);
        assert_eq!((read.answer, read.explicit), (Answer::Allow, false));
        assert_eq!((outside.answer, outside.explicit), (Answer::Ask, false));

        let edit = set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[(mail.clone(), ActionKind::Outside, Some(Answer::Deny))],
        );
        assert_eq!(edit.granted.len(), 1);
        let outside = answer_at(&ws, AGENT, &mail, ActionKind::Outside);
        assert_eq!((outside.answer, outside.explicit), (Answer::Deny, true));
        // Another app is untouched.
        let notes = answer_at(
            &ws,
            AGENT,
            &AnswerScope::app("so.tilo.notes"),
            ActionKind::Outside,
        );
        assert_eq!((notes.answer, notes.explicit), (Answer::Ask, false));
    }

    #[test]
    fn setting_again_replaces_and_clearing_returns_to_the_admission() {
        let mut ws = ws();
        let mail = AnswerScope::app(MAIL);
        let set = |ws: &mut Workspace, answer| {
            set_agent_answers(
                ws,
                &OWNER.to_string(),
                AGENT,
                &[(mail.clone(), ActionKind::Undoable, answer)],
            )
        };
        set(&mut ws, Some(Answer::Ask));
        let edit = set(&mut ws, Some(Answer::Deny));
        assert_eq!(edit.granted.len(), 1);
        let held: Vec<_> = ws
            .policy
            .grants
            .iter()
            .filter(|grant| agent_answers::is_answer(grant))
            .collect();
        assert_eq!(held.len(), 1, "one answer per scope and kind");
        assert_eq!(held[0].answer, Answer::Deny);
        let cleared = set(&mut ws, None);
        assert_eq!((cleared.revoked.len(), cleared.granted.len()), (1, 0));
        let cell = answer_at(&ws, AGENT, &mail, ActionKind::Undoable);
        assert_eq!((cell.answer, cell.explicit), (Answer::Allow, false));
        // Nothing to clear: no edit.
        assert!(set(&mut ws, None).is_empty());
    }

    #[test]
    fn rows_list_the_apps_with_answers_and_exceptions_only_action_allows() {
        let mut ws = ws();
        let send = AnswerScope::action(MAIL, "mail.mailbox", "send_message");
        let purge = AnswerScope::action(MAIL, "mail.mailbox", "purge");
        set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[
                (send.clone(), ActionKind::Outside, Some(Answer::Allow)),
                (purge, ActionKind::Undoable, Some(Answer::Deny)),
            ],
        );
        let rows = agent_access_rows(&ws, AGENT, &super::super::NoAppDirectory);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scope, AnswerScope::app(MAIL));
        assert!(!rows[0].customised(), "an action's answer is not the app's");
        let exceptions = agent_exceptions(&ws, AGENT, &super::super::NoAppDirectory);
        assert_eq!(exceptions.len(), 1, "a Deny is no exception to list");
        assert_eq!(exceptions[0].scope, send);
        assert_eq!(exceptions[0].kind, ActionKind::Outside);
        assert_eq!(exceptions[0].label, format!("{MAIL} · Send message"));
        // Taking it back.
        let edit = set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[(send, ActionKind::Outside, None)],
        );
        assert_eq!(edit.revoked.len(), 1);
        assert!(agent_exceptions(&ws, AGENT, &super::super::NoAppDirectory).is_empty());
    }

    #[test]
    fn answers_keep_the_agents_preset_and_stay_out_of_custom() {
        let mut ws = ws();
        set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[
                (AnswerScope::app(MAIL), ActionKind::Read, Some(Answer::Deny)),
                (
                    AnswerScope::app(MAIL),
                    ActionKind::Outside,
                    Some(Answer::Ask),
                ),
            ],
        );
        assert_eq!(
            super::super::agent_authority_preset(&ws, AGENT),
            super::super::AgentPreset::Operator,
            "a narrowing answer is no broad grant"
        );
        set_agent_answers(
            &mut ws,
            &OWNER.to_string(),
            AGENT,
            &[(
                AnswerScope::app(MAIL),
                ActionKind::Outside,
                Some(Answer::Allow),
            )],
        );
        assert_eq!(
            super::super::agent_authority_preset(&ws, AGENT),
            super::super::AgentPreset::Custom,
            "letting it reach outside unasked needs a conscious review"
        );
    }

    #[test]
    fn someone_who_may_not_govern_changes_nothing() {
        let mut ws = ws();
        let edit = set_agent_answers(
            &mut ws,
            &"stranger".to_string(),
            AGENT,
            &[(AnswerScope::app(MAIL), ActionKind::Read, Some(Answer::Deny))],
        );
        assert!(edit.is_empty());
        assert!(!ws.policy.grants.iter().any(agent_answers::is_answer));
    }
}
