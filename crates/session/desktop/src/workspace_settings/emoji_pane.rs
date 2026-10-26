//! The Emoji pane of Workspace Settings: the workspace's own emoji, which
//! every app of the workspace draws for `:name:`, each with its picture, its
//! name and who added it. Add emoji is live for whoever the room's grants let
//! add one; Remove stands on each emoji the acting person may remove: their
//! own, or anyone's as a moderator.
//!
//! The pane decides nothing itself. It reads what the room's grants answer
//! for the record's actions at the node of the app that keeps the record
//! ([`EmojiGrants::of`]), the node every door decides those actions at, and
//! reports typed hits; the host applies them as the record's actions, which
//! the grants decide again.

use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use tilo_app::tilo_resource::Resource as _;
use tilo_app::tilo_workspace_emoji::{KIND, OWNER, WorkspaceEmoji as Record};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{
    Banner, BannerContent, BannerKind, Button, ButtonFace, ButtonRole, ControlSize, ListRowFace,
    RowLeading,
};

use super::overlay::EventOutcome;
use super::{
    CONTROL_HEIGHT, PANE_INTRO_GAP, PANE_INTRO_HEIGHT, PANE_PAD_BOTTOM, PANE_PAD_TOP, PANE_PAD_X,
    ROW_GAP, ROW_HEIGHT, Workspace, draw_authority_note, draw_empty_state, draw_row_card,
};
use crate::i18n::{t, t_args};

/// The record's action adding an emoji.
pub const ADD: &str = "add-emoji";
/// The record's action removing an emoji one added oneself.
pub const REMOVE_OWN: &str = "remove-emoji";
/// The record's action removing anyone's emoji, as moderation.
pub const REMOVE_ANY: &str = "remove-any-emoji";

/// A cell is never narrower than this: below two of them side by side the
/// grid is one column.
const MIN_CELL_WIDTH: f64 = 240.0;
/// The Add emoji button in the intro card.
const ADD_WIDTH: f64 = 160.0;

/// One emoji as the pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiEntry {
    /// Its name, without colons.
    pub name: String,
    /// The person who added it.
    pub added_by: String,
}

/// What the acting person may do with the workspace's emoji, as the room's
/// grants answer now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one answer per action of the record, each independent of the others"
)]
pub struct EmojiGrants {
    pub add: bool,
    pub remove_own: bool,
    pub remove_any: bool,
}

/// How an emoji is removed: one's own by the record's own removal, anyone
/// else's as moderation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    Own,
    Moderation,
}

impl Removal {
    /// The record's action for this removal.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Own => REMOVE_OWN,
            Self::Moderation => REMOVE_ANY,
        }
    }
}

/// The node the room's grants decide the record's action `verb` at, below
/// `ws`'s room, and the kind of action it is, as the record declares it.
/// `None` for a verb the record does not declare.
#[must_use]
pub fn emoji_action(
    ws: &Workspace,
    verb: &str,
) -> Option<(tilo_action_policy::TreePath, tilo_actions::ActionKind)> {
    let declared = Record::declarations()
        .into_iter()
        .find(|declared| declared.id == verb)?;
    Some((
        tilo_actions::action_node(
            &ws.node(),
            OWNER,
            KIND,
            &collab_types::workspace_record_id(KIND),
            declared.field,
            verb,
        ),
        declared.kind,
    ))
}

impl EmojiGrants {
    /// What the room's grants let `user` do with `ws`'s emoji at `now`
    /// (Unix seconds).
    #[must_use]
    pub fn of(ws: &Workspace, user: &str, now: u64) -> Self {
        let may = |verb: &str| {
            emoji_action(ws, verb).is_some_and(|(node, kind)| {
                ws.answer_at(user, &node, kind, now) == tilo_action_policy::Answer::Allow
            })
        };
        Self {
            add: may(ADD),
            remove_own: may(REMOVE_OWN),
            remove_any: may(REMOVE_ANY),
        }
    }

    /// How `user` may remove `entry`, if they may.
    #[must_use]
    pub fn removal(self, entry: &EmojiEntry, user: &str) -> Option<Removal> {
        if entry.added_by == user {
            // One's own goes by the plain removal where that is allowed; a
            // moderator who may not edit still removes it as moderation.
            if self.remove_own {
                return Some(Removal::Own);
            }
        }
        self.remove_any.then_some(Removal::Moderation)
    }
}

/// Something the person should know about the last add or remove: why it
/// did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiNotice {
    pub title: String,
    pub detail: Option<String>,
}

impl EmojiNotice {
    /// No picture came from the chooser: why.
    #[must_use]
    pub fn not_chosen(why: impl Into<String>) -> Self {
        Self {
            title: t("workspace-settings-emoji-not-chosen"),
            detail: Some(why.into()),
        }
    }

    /// The chosen picture was not added: why.
    #[must_use]
    pub fn not_added(file: &str, why: impl Into<String>) -> Self {
        Self {
            title: t_args(
                "workspace-settings-emoji-not-added",
                &[("file", file.into())],
            ),
            detail: Some(why.into()),
        }
    }

    /// Nothing of the chosen file's name can name an emoji.
    #[must_use]
    pub fn unnamed(file: &str) -> Self {
        Self::not_added(file, t("workspace-settings-emoji-no-name"))
    }

    /// The chosen file was not added: the workspace's emoji are not
    /// available on this device.
    #[must_use]
    pub fn unavailable(file: &str) -> Self {
        Self::not_added(file, t("workspace-settings-emoji-unavailable"))
    }

    /// `name` was not removed: why.
    #[must_use]
    pub fn not_removed(name: &str, why: impl Into<String>) -> Self {
        Self {
            title: t_args(
                "workspace-settings-emoji-not-removed",
                &[("name", name.into())],
            ),
            detail: Some(why.into()),
        }
    }
}

/// Why a chosen picture could not be made one an emoji keeps, as the person
/// reads it.
#[must_use]
pub fn small_refusal(small: &ui::workspace_emoji::Small) -> Option<String> {
    use ui::workspace_emoji::Small;
    match small {
        Small::Ready(_) => None,
        Small::Pending => Some(t("workspace-settings-emoji-tools-loading")),
        Small::NotAPicture => Some(t("workspace-settings-emoji-not-a-picture")),
        Small::TooLarge => Some(t("workspace-settings-emoji-too-detailed")),
    }
}

/// What the Emoji pane shows, read from the host once per frame.
pub struct EmojiPaneModel<'a> {
    /// Whether the workspace's emoji could be opened here at all.
    pub available: bool,
    /// The emoji, by name.
    pub entries: &'a [EmojiEntry],
    /// Their pictures, decoded once.
    pub pictures: Option<&'a ui::workspace_emoji::WorkspaceEmoji>,
    pub grants: EmojiGrants,
    /// The person acting, as the record names who added an emoji.
    pub me: &'a str,
    pub notice: Option<&'a EmojiNotice>,
    /// The chooser is open: Add waits for it.
    pub choosing: bool,
}

impl EmojiPaneModel<'_> {
    /// A pane with nothing to show: the workspace's emoji are not open here.
    #[must_use]
    pub fn unavailable(me: &str) -> EmojiPaneModel<'_> {
        EmojiPaneModel {
            available: false,
            entries: &[],
            pictures: None,
            grants: EmojiGrants::default(),
            me,
            notice: None,
            choosing: false,
        }
    }

    /// Whether Add emoji takes a press now.
    #[must_use]
    pub fn may_add(&self) -> bool {
        self.available && self.grants.add && !self.choosing
    }
}

/// One emoji's cell, as painted (scroll applied).
#[derive(Clone, Debug, PartialEq)]
pub struct EmojiCell {
    pub name: String,
    pub rect: Rect,
    /// Its Remove control; zero where the person may not remove it.
    pub remove: Rect,
    pub removal: Option<Removal>,
}

/// Where the pane put its parts, for painting and hit-testing alike.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmojiPaneLayout {
    /// The Add emoji button; zero while it takes no press.
    pub add: Rect,
    /// Where Add emoji is painted, live or not.
    pub add_slot: Rect,
    pub notice: Rect,
    /// The notice's dismiss control, once painted.
    pub dismiss: Rect,
    /// The region the cells scroll in.
    pub grid: Rect,
    /// The cells at least partly inside [`Self::grid`].
    pub cells: Vec<EmojiCell>,
    pub max_scroll: f64,
}

impl EmojiPaneLayout {
    /// What a press at `point` means.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<EventOutcome> {
        let inside = |rect: Rect| rect.area() > 0.0 && rect.contains(point);
        if inside(self.add) {
            return Some(EventOutcome::AddEmoji);
        }
        if inside(self.dismiss) {
            return Some(EventOutcome::DismissEmojiNotice);
        }
        if !self.grid.contains(point) {
            return None;
        }
        self.cells.iter().find_map(|cell| {
            (inside(cell.remove))
                .then(|| {
                    cell.removal
                        .map(|how| EventOutcome::RemoveEmoji(cell.name.clone(), how))
                })
                .flatten()
        })
    }
}

/// How many columns the grid has in `width`.
fn columns(width: f64) -> usize {
    ui::num::usize_of_f64((width / MIN_CELL_WIDTH).floor()).clamp(1, 3)
}

/// Pure geometry of the pane in `pane`, the notice taking `notice_height`
/// and the grid scrolled by `scroll`.
#[must_use]
pub fn emoji_pane_layout(
    pane: Rect,
    model: &EmojiPaneModel<'_>,
    notice_height: f64,
    scroll: f64,
) -> EmojiPaneLayout {
    let mut layout = EmojiPaneLayout::default();
    if pane.width() <= 0.0 || pane.height() <= 0.0 {
        return layout;
    }
    let x0 = pane.x0 + PANE_PAD_X;
    let x1 = pane.x1 - PANE_PAD_X;
    let intro_y = pane.y0 + PANE_PAD_TOP;
    let centre = intro_y + PANE_INTRO_HEIGHT / 2.0;
    layout.add_slot = Rect::new(
        x1 - 8.0 - ADD_WIDTH,
        centre - CONTROL_HEIGHT / 2.0,
        x1 - 8.0,
        centre + CONTROL_HEIGHT / 2.0,
    );
    if model.may_add() {
        layout.add = layout.add_slot;
    }
    let mut y = intro_y + PANE_INTRO_HEIGHT + PANE_INTRO_GAP;
    if notice_height > 0.0 {
        layout.notice = Rect::new(x0, y, x1, y + notice_height);
        y += notice_height + PANE_INTRO_GAP;
    }
    // The note under the grid, saying why there is no Add, keeps its line.
    let bottom = pane.y1 - PANE_PAD_BOTTOM - if model.grants.add { 0.0 } else { 24.0 };
    layout.grid = Rect::new(x0, y, x1, bottom.max(y));
    let columns = columns(x1 - x0);
    let gap = ROW_GAP;
    let cell_w = (x1 - x0 - gap * ui::num::f64_of_usize(columns.saturating_sub(1)))
        / ui::num::f64_of_usize(columns);
    let rows = model.entries.len().div_ceil(columns);
    let content = ui::num::f64_of_usize(rows) * (ROW_HEIGHT + gap) - gap;
    layout.max_scroll = (content - layout.grid.height()).max(0.0);
    let scroll = scroll.clamp(0.0, layout.max_scroll);
    for (index, entry) in model.entries.iter().enumerate() {
        let column = index.checked_rem(columns).unwrap_or(0);
        let row = index.checked_div(columns).unwrap_or(0);
        let cx = x0 + ui::num::f64_of_usize(column) * (cell_w + gap);
        let cy = y + ui::num::f64_of_usize(row) * (ROW_HEIGHT + gap) - scroll;
        let rect = Rect::new(cx, cy, cx + cell_w, cy + ROW_HEIGHT);
        if rect.y1 <= layout.grid.y0 || rect.y0 >= layout.grid.y1 {
            continue;
        }
        let removal = model
            .available
            .then(|| model.grants.removal(entry, model.me))
            .flatten();
        let remove = if removal.is_some() {
            Rect::new(
                rect.x1 - 8.0 - CONTROL_HEIGHT,
                rect.center().y - CONTROL_HEIGHT / 2.0,
                rect.x1 - 8.0,
                rect.center().y + CONTROL_HEIGHT / 2.0,
            )
            // Only the part of the control inside the grid takes a press.
            .intersect(layout.grid)
        } else {
            Rect::ZERO
        };
        layout.cells.push(EmojiCell {
            name: entry.name.clone(),
            rect,
            remove,
            removal,
        });
    }
    layout
}

/// What [`draw_emoji_pane`] needs of the workspace besides the pane's model:
/// the workspace, and what each person is called.
pub struct EmojiPaneContext<'a> {
    pub ws: &'a Workspace,
    pub people: &'a crate::collaborators::PeopleNames,
}

/// Paint the pane in `pane`, scrolled by `scroll`, and answer where its
/// parts are.
pub fn draw_emoji_pane(
    ctx: &mut PaintCtx<'_>,
    pane: Rect,
    model: &EmojiPaneModel<'_>,
    workspace: &EmojiPaneContext<'_>,
    scroll: f64,
) -> EmojiPaneLayout {
    let theme = ctx.theme;
    let notice = model.notice.map(|notice| {
        BannerContent::new(BannerKind::Error, &notice.title)
            .with_message(notice.detail.as_deref())
            .dismissible(true)
    });
    let notice_height = notice.as_ref().map_or(0.0, |content| {
        Banner::height(content, pane.width() - 2.0 * PANE_PAD_X, ctx.text, theme)
    });
    let mut layout = emoji_pane_layout(pane, model, notice_height, scroll);
    if layout.grid.area() <= 0.0 && layout.add_slot.area() <= 0.0 {
        return layout;
    }
    draw_intro(ctx, pane, model, &layout);
    if let Some(content) = &notice {
        let hit = Banner::paint(ctx, layout.notice, content, Point::new(-1.0, -1.0));
        layout.dismiss = hit.dismiss.unwrap_or(Rect::ZERO);
    }
    if model.entries.is_empty() {
        let detail = if !model.available {
            t("workspace-settings-emoji-unavailable")
        } else if model.grants.add {
            t("workspace-settings-emoji-empty-detail")
        } else {
            t("workspace-settings-emoji-empty-detail-viewer")
        };
        draw_empty_state(
            ctx.scene,
            ctx.transform,
            layout.grid,
            Icon::Smile,
            (&t("workspace-settings-emoji-empty-title"), &detail),
            ctx.text,
            theme,
        );
    } else {
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &layout.grid,
        );
        for cell in &layout.cells {
            draw_cell(ctx, cell, model, workspace);
        }
        ctx.scene.pop_layer();
    }
    if model.available && !model.grants.add {
        draw_authority_note(
            ctx.scene,
            ctx.transform,
            pane,
            &t("workspace-settings-emoji-editors-add"),
            ctx.text,
            theme,
        );
    }
    layout
}

/// How many emoji there are, how to use them, and Add emoji.
fn draw_intro(
    ctx: &mut PaintCtx<'_>,
    pane: Rect,
    model: &EmojiPaneModel<'_>,
    layout: &EmojiPaneLayout,
) {
    let theme = ctx.theme;
    let rect = Rect::new(
        pane.x0 + PANE_PAD_X,
        pane.y0 + PANE_PAD_TOP,
        pane.x1 - PANE_PAD_X,
        pane.y0 + PANE_PAD_TOP + PANE_INTRO_HEIGHT,
    );
    let count = t_args(
        "workspace-settings-emoji-count",
        &[("count", model.entries.len().into())],
    );
    let help = t("workspace-settings-emoji-help");
    ListRowFace::new(&count)
        .leading(RowLeading::Icon(Icon::Smile))
        .secondary(&help)
        .secondary_lines(2)
        .trailing_width(rect.x1 - layout.add_slot.x0)
        .paint(ctx, rect);
    let label = t("workspace-settings-emoji-add");
    let height = ButtonFace::height(ControlSize::Regular, theme);
    let slot = layout.add_slot;
    Button::paint(
        ctx.scene,
        ctx.transform,
        ctx.text,
        theme,
        &ButtonFace::new(
            Rect::new(
                slot.x0,
                slot.center().y - height / 2.0,
                slot.x1,
                slot.center().y + height / 2.0,
            ),
            &label,
        )
        .icon(Icon::SmilePlus)
        .role(ButtonRole::Secondary)
        .disabled(!model.may_add()),
    );
}

/// One emoji: its picture, `:name:`, who added it, and Remove where the
/// person may.
fn draw_cell(
    ctx: &mut PaintCtx<'_>,
    cell: &EmojiCell,
    model: &EmojiPaneModel<'_>,
    workspace: &EmojiPaneContext<'_>,
) {
    let theme = ctx.theme;
    draw_row_card(ctx.scene, ctx.transform, cell.rect, theme);
    let title = format!(":{}:", cell.name);
    let added_by = model
        .entries
        .iter()
        .find(|entry| entry.name == cell.name)
        .map(|entry| {
            if entry.added_by == model.me {
                return t("workspace-settings-emoji-added-by-you");
            }
            let name =
                super::member_row_name(workspace.people, workspace.ws, model.me, &entry.added_by);
            t_args(
                "workspace-settings-emoji-added-by",
                &[("name", name.into())],
            )
        })
        .unwrap_or_default();
    let picture = model
        .pictures
        .and_then(|pictures| pictures.image(&cell.name));
    let paint_picture = |ctx: &mut PaintCtx<'_>, slot: Rect| {
        if let Some(picture) = &picture {
            ui::image_store::paint_image_contain(ctx.scene, ctx.transform, slot, picture, 0.0);
        }
    };
    let mut face = ListRowFace::new(&title)
        .leading(RowLeading::Paint(&paint_picture))
        .secondary(&added_by);
    if cell.remove.area() > 0.0 {
        face = face.trailing_width(cell.rect.x1 - cell.remove.x0);
    }
    face.paint(ctx, cell.rect);
    if cell.removal.is_some() {
        let side = ButtonFace::height(ControlSize::Small, theme);
        let centre = Point::new(
            cell.rect.x1 - 8.0 - CONTROL_HEIGHT / 2.0,
            cell.rect.center().y,
        );
        let name = t_args(
            "workspace-settings-emoji-remove",
            &[("name", cell.name.as_str().into())],
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::icon_only(
                Rect::from_center_size(centre, (side, side)),
                Icon::Trash2,
                &name,
            )
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small),
        );
    }
}

#[cfg(test)]
#[path = "emoji_pane_tests.rs"]
mod tests;
