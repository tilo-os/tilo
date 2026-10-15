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

/// The view's geometry: what was painted where.
#[derive(Clone, Debug, Default)]
pub struct AgentAccessLayout {
    pub targets: Vec<(AccessTarget, Rect)>,
}

impl AgentAccessLayout {
    /// The control at `point`; the last painted wins, so a control inside
    /// a row beats the row itself.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<AccessTarget> {
        self.targets
            .iter()
            .rev()
            .find(|(_, rect)| rect.contains(point))
            .map(|(target, _)| target.clone())
    }
}

/// Which agent the view shows, and which of its apps is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentAccessView<'a> {
    pub agent: &'a str,
    pub open: Option<&'a AnswerScope>,
    pub editable: bool,
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
    name: &str,
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
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face(back));
    layout.targets.push((AccessTarget::Back, back));
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
    let text_x = draw_app_head(ctx, head, row, (open, view.editable), layout);
    if open {
        let segments_x0 = right - 12.0 - SEGMENT_WIDTH * 3.0;
        for (index, cell) in row.cells.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "three kinds")]
            let line_y = head.y1 + KIND_LINE_HEIGHT * index as f64;
            draw_kind_line(
                ctx,
                (text_x, segments_x0, line_y + KIND_LINE_HEIGHT / 2.0),
                (&row.scope, cell),
                view.editable,
                layout,
            );
        }
    }
    height
}

/// Paint an app's head: chevron, name, its answers or where they come
/// from, and Reset or Custom. Returns where its text starts.
fn draw_app_head(
    ctx: &mut PaintCtx<'_>,
    head: Rect,
    row: &AgentAccessRow,
    (open, editable): (bool, bool),
    layout: &mut AgentAccessLayout,
) -> f64 {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let caption = theme.tokens.typography.style(TextRole::Caption);
    let line_h = f64::from(caption.line_height_px());
    layout
        .targets
        .push((AccessTarget::Expand(row.scope.clone()), head));
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
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(reset, &label).disabled(!editable),
        );
        if editable {
            layout
                .targets
                .push((AccessTarget::Reset(row.scope.clone()), reset));
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
    editable: bool,
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
    ui::widget::ToggleGroup::paint(
        ctx.scene,
        ctx.transform,
        ctx.text,
        theme,
        &SegmentedFace::new(&rects, &label_refs, selected),
    );
    if editable {
        for (segment, answer) in rects.iter().zip(ANSWERS) {
            layout.targets.push((
                AccessTarget::Answer(scope.clone(), cell.kind, answer),
                *segment,
            ));
        }
    } else {
        ctx.scene.pop_layer();
    }
}

/// Paint the actions always allowed without asking first, from `y`.
fn draw_exceptions(
    ctx: &mut PaintCtx<'_>,
    (left, right, mut y, bottom): (f64, f64, f64, f64),
    exceptions: &[AgentException],
    editable: bool,
    layout: &mut AgentAccessLayout,
) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let caption = theme.tokens.typography.style(TextRole::Caption);
    let line_h = f64::from(caption.line_height_px());
    if exceptions.is_empty() || y + line_h + ACCESS_ROW_HEIGHT > bottom {
        return;
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
        if rect.y1 > bottom {
            break;
        }
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
        super::draw_danger_button(
            ctx.scene,
            ctx.transform,
            remove,
            &stop,
            editable,
            ctx.text,
            theme,
        );
        if editable {
            layout.targets.push((
                AccessTarget::RemoveException(exception.scope.clone(), exception.kind),
                remove,
            ));
        }
        y += ACCESS_ROW_HEIGHT + ROW_GAP;
    }
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
    draw_header(ctx, (left, right, y), &name, &mut layout);
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

    let rows = agent_access_rows(ws, view.agent, apps);
    let exceptions = agent_exceptions(ws, view.agent, apps);
    // The always-allowed actions keep their place below however many apps
    // there are: they are what lifts asking first, so never cut off.
    #[allow(clippy::cast_precision_loss, reason = "a short list")]
    let reserved = if exceptions.is_empty() {
        0.0
    } else {
        8.0 + line_h + 8.0 + (ACCESS_ROW_HEIGHT + ROW_GAP) * exceptions.len().min(3) as f64
    };
    let rows_bottom = bottom - reserved;
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
        let open = view.open == Some(&row.scope);
        let needed = if open {
            ACCESS_ROW_HEIGHT + KIND_LINE_HEIGHT * 3.0 + 8.0
        } else {
            ACCESS_ROW_HEIGHT
        };
        if y + needed > rows_bottom {
            break;
        }
        y += draw_app(ctx, (left, right, y), row, view, &mut layout) + ROW_GAP;
    }
    draw_exceptions(
        ctx,
        (left, right, y + 8.0, bottom),
        &exceptions,
        view.editable,
        &mut layout,
    );
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
