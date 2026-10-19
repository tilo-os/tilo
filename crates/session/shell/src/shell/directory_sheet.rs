//! "Join a workspace…": the public workspaces a person can join, as a sheet
//! in place, the way chat apps let people browse and join servers.
//!
//! The sheet lists what the sites this computer knows serve in their
//! directories ([`crate::shell::network::DirectoryListing`]), the site's own
//! community workspace first: each with its name, what it is, who is in it
//! now and how lively it is, and Join. Search narrows the list as the person
//! types; Enter on a site (`example.org`) asks that site's directory too,
//! and a link or an invitation code goes to the join field
//! ([`crate::shell::peer_sync_overlay`]), which "Join with a link or code"
//! opens as well. Join adds the workspace and switches into it at once.
//!
//! The component owns presentation, input and its painted geometry. The
//! host carries out the [`DirectoryRequest`]s it emits off the frame loop
//! and hands the answers back ([`DirectorySheet::show`],
//! [`DirectorySheet::joined`], [`DirectorySheet::failed`]). It comes and
//! goes on the standard [`ui::motion::Appear`] motion.

use tilo_desktop::text_entry::{self, Mods};
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::num::{f32_of_f64, f64_of_usize};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::widget::{ButtonRole, FieldFace, TextInput, Widget as _};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::shell::i18n::{t, t_args};
use crate::shell::network::join_field::{self, JoinText};
use crate::shell::network::{DirectoryListing, JoinProgress, SiteJoinState};
use tilo_relay_protocol::directory::Activity;
use tilo_relay_protocol::rooms::RoomRole;

/// Assistive targets this sheet publishes start with this.
const TARGET_PREFIX: &str = "directory:";
/// The tallest the sheet grows.
const MAX_HEIGHT: f64 = 600.0;
/// One listing's row.
const ROW_H: f64 = 76.0;
/// The workspace's mark at a row's start.
const MARK: f64 = 40.0;
/// One face of who is here now.
const FACE: f64 = 18.0;
/// How far each face overlaps the one before.
const FACE_STEP: f64 = 13.0;
/// Faces shown before the rest collapse into "+N".
const FACES_SHOWN: usize = 4;

/// What the host carries out for the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryRequest {
    /// Read the directories again; with `site`, that site's too.
    Fetch { site: Option<String> },
    /// Join this listing, with the role its owner set.
    Join(Box<DirectoryListing>),
    /// Switch to this listing's workspace, which this computer holds.
    Open(Box<DirectoryListing>),
    /// Open the join field with this text (a link, a code, or nothing).
    LinkOrCode(String),
}

/// What one event did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryOutcome {
    pub consumed: bool,
    pub request: Option<DirectoryRequest>,
}

impl DirectoryOutcome {
    fn consumed() -> Self {
        Self {
            consumed: true,
            request: None,
        }
    }

    fn ignored() -> Self {
        Self::default()
    }

    fn request(request: DirectoryRequest) -> Self {
        Self {
            consumed: true,
            request: Some(request),
        }
    }
}

/// Where the sheet's list stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Status {
    /// Asking the directories.
    #[default]
    Loading,
    /// The listings are shown.
    Ready,
    /// No directory answered; why.
    Failed(String),
}

/// Something the sheet paints that a press or an assistive action lands on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// The row button of the `n`th listing shown: Join, or Open once held.
    Row(usize),
    Search,
    LinkOrCode,
    Close,
}

/// The directory sheet.
#[derive(Default)]
pub struct DirectorySheet {
    open: bool,
    appearing: ui::motion::Appearing,
    query: String,
    /// The search field's caret and selection, byte indices into `query`.
    query_selection: Selection,
    /// The shared text input that paints the search while it holds text.
    input: Option<TextInput>,
    listings: Vec<DirectoryListing>,
    status: Status,
    /// The listing being joined, by its key, and why the last join failed.
    joining: Option<String>,
    note: Option<String>,
    /// Where the joins this session runs stand, as the network last said:
    /// a listing joining shows it, one that failed says why.
    joins: Vec<SiteJoinState>,
    /// The first row shown.
    scroll: usize,
    /// The row the keyboard chose.
    selected: Option<usize>,
    /// The sheet as last painted, and its pose then.
    rect: Rect,
    pose: Option<ui::motion::Pose>,
    targets: Vec<(Target, Rect)>,
}

/// The key a listing is joined and found again by.
fn key_of(listing: &DirectoryListing) -> String {
    listing.room.map_or_else(
        || {
            listing
                .site
                .as_ref()
                .map_or_else(|| listing.entry.name.clone(), url::Url::to_string)
        },
        |room| room.to_string(),
    )
}

/// The words a role is shown with.
fn role_label(role: RoomRole) -> String {
    t(match role {
        RoomRole::Viewer => "directory-role-viewer",
        RoomRole::Commenter => "directory-role-commenter",
        RoomRole::Editor => "directory-role-editor",
        RoomRole::Moderator => "directory-role-moderator",
    })
}

/// How lively a listing is, and how many belong to it, in a line.
pub(crate) fn activity_line(listing: &DirectoryListing, now_ms: u64) -> String {
    let activity = match listing.activity(now_ms) {
        Activity::HereNow(count) => t_args("directory-here-now", &[("count", count.into())]),
        Activity::ActiveToday => t("directory-active-today"),
        Activity::ActiveThisWeek => t("directory-active-week"),
        Activity::Quiet => t("directory-quiet"),
    };
    match listing.entry.members {
        0 => activity,
        members => format!(
            "{activity} · {}",
            t_args("directory-members", &[("count", members.into())])
        ),
    }
}

/// The role people who join `listing` get, as its button's description
/// says it.
fn joins_as(listing: &DirectoryListing) -> String {
    t_args(
        "directory-joins-as",
        &[("role", role_label(listing.entry.default_role).into())],
    )
}

/// What Enter does with what was typed into the search field, when no row
/// is chosen: a link or an invitation goes to the join field, a site is
/// looked up; a word is a search.
fn typed_request(query: &str) -> Option<DirectoryRequest> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let parsed = join_field::classify(query).ok();
    let link_or_code = matches!(
        parsed,
        Some(JoinText::InvitationLink { .. } | JoinText::Site { room: Some(_), .. })
    ) || (matches!(parsed, Some(JoinText::Invitation(_)))
        && join_field::is_invitation(query));
    if link_or_code {
        return Some(DirectoryRequest::LinkOrCode(query.to_owned()));
    }
    (matches!(parsed, Some(JoinText::Site { room: None, .. })) && query.contains('.')).then(|| {
        DirectoryRequest::Fetch {
            site: Some(query.to_owned()),
        }
    })
}

impl DirectorySheet {
    /// Open the sheet, asking the directories afresh.
    pub fn open(&mut self) -> DirectoryRequest {
        self.open = true;
        self.appearing.show(true);
        self.set_query(String::new());
        self.status = Status::Loading;
        self.note = None;
        self.joining = None;
        self.scroll = 0;
        self.selected = None;
        DirectoryRequest::Fetch { site: None }
    }

    /// Close it; it leaves on its motion and takes no more input.
    pub fn close(&mut self) {
        self.open = false;
        self.appearing.show(false);
        self.joining = None;
    }

    /// Whether it takes input: open, not leaving.
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

    /// Show what the directories listed.
    pub fn show(&mut self, listings: Vec<DirectoryListing>) {
        self.listings = listings;
        self.status = Status::Ready;
        self.scroll = 0;
        self.selected = None;
    }

    /// Say why the directories could not be read, or why a join failed.
    pub fn failed(&mut self, error: &str) {
        if let Some(key) = self.joining.take() {
            let name = self
                .listings
                .iter()
                .find(|listing| key_of(listing) == key)
                .map(|listing| listing.entry.name.clone())
                .unwrap_or_default();
            self.note = Some(t_args(
                "directory-join-failed",
                &[("name", name.into()), ("error", error.to_owned().into())],
            ));
            return;
        }
        if self.listings.is_empty() {
            self.status = Status::Failed(error.to_owned());
        } else {
            self.note = Some(t_args(
                "directory-failed",
                &[("error", error.to_owned().into())],
            ));
        }
    }

    /// A join finished: the sheet's work is done.
    pub fn joined(&mut self) {
        self.joining = None;
        self.close();
    }

    /// What it shows now: the listings that answer the search.
    #[must_use]
    pub fn shown(&self) -> Vec<&DirectoryListing> {
        self.listings
            .iter()
            .filter(|listing| listing.matches(&self.query))
            .collect()
    }

    /// What was typed into the search field.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the listing `key` names is being joined: asked here, or
    /// joining in this session already.
    #[must_use]
    pub fn is_joining(&self, listing: &DirectoryListing) -> bool {
        self.joining.as_deref() == Some(key_of(listing).as_str())
            || self.progress(listing) == Some(&JoinProgress::Joining)
    }

    /// Say where the joins this session runs stand.
    pub fn set_joins(&mut self, joins: Vec<SiteJoinState>) {
        self.joins = joins;
    }

    /// Where joining `listing` stands in this session, if it does.
    #[must_use]
    pub fn progress(&self, listing: &DirectoryListing) -> Option<&JoinProgress> {
        if listing.joined {
            return None;
        }
        let site = listing.site.as_ref().map(url::Url::as_str);
        self.joins
            .iter()
            .find(|join| {
                listing.room == Some(join.room) || (site.is_some() && join.site.as_deref() == site)
            })
            .map(|join| &join.progress)
    }

    /// Why joining `listing` failed last, if it did.
    fn failure(&self, listing: &DirectoryListing) -> Option<&str> {
        match self.progress(listing) {
            Some(JoinProgress::Failed(reason)) => Some(reason),
            _ => None,
        }
    }

    /// Route one event to the sheet while it is open. Every key belongs to
    /// it: a link pasted into the search shares characters with chords.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        cursor: Option<Point>,
    ) -> DirectoryOutcome {
        if !self.open {
            return DirectoryOutcome::ignored();
        }
        let mods = modifiers.state();
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key, mods),
            WindowEvent::KeyboardInput { .. } => DirectoryOutcome::consumed(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(point) = cursor.map(|point| self.to_local(point)) else {
                    return DirectoryOutcome::consumed();
                };
                let hit = self
                    .targets
                    .iter()
                    .find(|(_, rect)| rect.area() > 0.0 && rect.contains(point))
                    .map(|(target, _)| target.clone());
                match hit {
                    Some(target) => self.activate(&target),
                    // A press outside the sheet closes it, as a press
                    // outside any sheet does.
                    None if !self.rect.contains(point) => {
                        self.close();
                        DirectoryOutcome::consumed()
                    }
                    None => DirectoryOutcome::consumed(),
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let down = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y < 0.0,
                    MouseScrollDelta::PixelDelta(point) => point.y < 0.0,
                };
                let last = self.shown().len().saturating_sub(1);
                self.scroll = if down {
                    self.scroll.saturating_add(1).min(last)
                } else {
                    self.scroll.saturating_sub(1)
                };
                DirectoryOutcome::consumed()
            }
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } => {
                DirectoryOutcome::consumed()
            }
            _ => DirectoryOutcome::ignored(),
        }
    }

    fn handle_key(&mut self, key: &Key, mods: ModifiersState) -> DirectoryOutcome {
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.query.is_empty() {
                    self.close();
                } else {
                    self.set_query(String::new());
                }
                DirectoryOutcome::consumed()
            }
            Key::Named(NamedKey::ArrowDown) => {
                let count = self.shown().len();
                if count > 0 {
                    self.selected = Some(self.selected.map_or(0, |at| (at + 1).min(count - 1)));
                    self.keep_selection_in_view();
                }
                DirectoryOutcome::consumed()
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.selected = match self.selected {
                    Some(0) | None => None,
                    Some(at) => Some(at - 1),
                };
                self.keep_selection_in_view();
                DirectoryOutcome::consumed()
            }
            Key::Named(NamedKey::Enter) => match self.selected {
                Some(row) => self.activate(&Target::Row(row)),
                None => match typed_request(&self.query) {
                    Some(DirectoryRequest::Fetch { site }) => {
                        self.status = Status::Loading;
                        DirectoryOutcome::request(DirectoryRequest::Fetch { site })
                    }
                    Some(request) => DirectoryOutcome::request(request),
                    // One workspace answers the search: Enter joins it.
                    None if self.shown().len() == 1 => self.activate(&Target::Row(0)),
                    None => DirectoryOutcome::consumed(),
                },
            },
            // Everything else edits the search, as in every field.
            _ => {
                let before = self.query.clone();
                let _ = text_entry::apply(
                    key,
                    Mods::from_state(mods),
                    &mut self.query,
                    &mut self.query_selection,
                );
                if self.query != before {
                    self.selected = None;
                    self.scroll = 0;
                }
                DirectoryOutcome::consumed()
            }
        }
    }

    /// Text an input method committed into the search.
    pub fn commit_text(&mut self, text: &str) -> bool {
        if !self.open {
            return false;
        }
        text_entry::commit(text, &mut self.query, &mut self.query_selection);
        self.selected = None;
        self.scroll = 0;
        true
    }

    fn set_query(&mut self, query: String) {
        self.query_selection = Selection::collapsed(query.len());
        self.query = query;
        self.selected = None;
        self.scroll = 0;
    }

    fn keep_selection_in_view(&mut self) {
        let Some(row) = self.selected else {
            return;
        };
        let rows = self.rows_fitting();
        if row < self.scroll {
            self.scroll = row;
        } else if rows > 0 && row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    /// How many rows the last paint had room for.
    fn rows_fitting(&self) -> usize {
        self.targets
            .iter()
            .filter(|(target, _)| matches!(target, Target::Row(_)))
            .count()
            .max(1)
    }

    fn activate(&mut self, target: &Target) -> DirectoryOutcome {
        match target {
            Target::Row(row) => {
                let Some(listing) = self.shown().get(*row).map(|listing| (*listing).clone()) else {
                    return DirectoryOutcome::consumed();
                };
                if listing.joined {
                    return DirectoryOutcome::request(DirectoryRequest::Open(Box::new(listing)));
                }
                if self.progress(&listing) == Some(&JoinProgress::Joining) {
                    // Joining already: say so, and that it opens by itself.
                    self.note = Some(t_args(
                        "directory-already-joining",
                        &[("name", listing.entry.name.clone().into())],
                    ));
                    return DirectoryOutcome::consumed();
                }
                if self.joining.is_some() {
                    // One join at a time: the first is still on its way.
                    return DirectoryOutcome::consumed();
                }
                self.joining = Some(key_of(&listing));
                self.note = None;
                DirectoryOutcome::request(DirectoryRequest::Join(Box::new(listing)))
            }
            Target::Search => DirectoryOutcome::consumed(),
            Target::LinkOrCode => {
                // What was typed goes along when it is a link, a code or a
                // site; a search word does not.
                let typed = self.query.trim().to_owned();
                let text = if typed_request(&typed).is_some() {
                    typed
                } else {
                    String::new()
                };
                DirectoryOutcome::request(DirectoryRequest::LinkOrCode(text))
            }
            Target::Close => {
                self.close();
                DirectoryOutcome::consumed()
            }
        }
    }

    fn to_local(&self, point: Point) -> Point {
        self.pose.map_or(point, |pose| pose.to_local(point))
    }

    /// Paint the sheet over `host_rect`, centred.
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
        // Theme type tokens already carry the person's text scale.
        let renderer_scale = text.text_scale();
        text.set_text_scale(1.0);
        let margin = f64::from(theme.tokens.spacing.s4);
        let w = f64::from(theme.tokens.metrics.panel_width_sheet)
            .min(host_rect.width() - 2.0 * margin)
            .max(0.0);
        let h = self
            .content_height(theme)
            .min(MAX_HEIGHT)
            .min(host_rect.height() - 2.0 * margin)
            .max(0.0);
        let center = host_rect.center();
        self.rect = Rect::from_center_size(center, (w, h));
        let pose = self.appearing.pose(self.rect, center);
        self.pose = Some(pose);
        if let Some(drawn) = pose.begin(scene, transform, self.rect.inflate(32.0, 32.0)) {
            self.paint(scene, drawn, text, theme);
            pose.end(scene);
        }
        text.set_text_scale(renderer_scale);
    }

    /// How tall the sheet is with what it shows: its title, the search,
    /// the note, the rows (or the line saying why there are none) and the
    /// footer, so a short list makes a short sheet.
    fn content_height(&self, theme: &Theme) -> f64 {
        let tokens = &theme.tokens;
        let pad = f64::from(tokens.spacing.s4);
        let gap = f64::from(tokens.spacing.s2);
        let control_h = f64::from(tokens.metrics.min_pointer_target);
        let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
        let heading = f64::from(theme.text(TextRole::Heading).line_height_px());
        let note = if self.note.is_some() {
            caption + gap
        } else {
            0.0
        };
        let rows = match self.status {
            Status::Ready if !self.shown().is_empty() => ROW_H * f64_of_usize(self.shown().len()),
            _ => caption + gap * 2.0,
        };
        pad + heading + gap + control_h + gap + note + rows + gap + control_h + pad
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one paint pass laid out top to bottom from shared running offsets"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let rect = self.rect;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let _ = ui::chrome::shell::paint_panel(scene, transform, rect, theme, 1.0);
        let pad = f64::from(tokens.spacing.s4);
        let gap = f64::from(tokens.spacing.s2);
        let control_h = f64::from(tokens.metrics.min_pointer_target);
        let avail = (rect.width() - pad * 2.0).max(0.0);
        let mut y = rect.y0 + pad;

        // The title.
        let heading = theme.text(TextRole::Heading);
        let title = text.elide_to_width_style(&t("directory-title"), f32_of_f64(avail), &heading);
        text.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
            &heading,
        );
        y += f64::from(heading.line_height_px()) + gap;

        // The search field: the shared field face and text input, so caret,
        // selection and input methods behave as in every other field.
        let field = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + control_h);
        self.paint_search(scene, transform, text, theme, field);
        self.targets.push((Target::Search, field));
        y = field.y1 + gap;

        // The footer: Join with a link or code, and Close.
        let mut paint = PaintCtx::new(scene, transform, text, theme);
        let footer_y = rect.y1 - pad - control_h;
        let close_label = t("directory-close");
        let link_label = t("directory-link-or-code");
        let close_w = button_face(Rect::ZERO, &close_label, false)
            .intrinsic_width(paint.text, theme)
            .min(avail / 2.0);
        let close = Rect::new(
            rect.x1 - pad - close_w,
            footer_y,
            rect.x1 - pad,
            footer_y + control_h,
        );
        let link_w = button_face(Rect::ZERO, &link_label, false)
            .intrinsic_width(paint.text, theme)
            .min(avail - close_w - gap);
        let link = Rect::new(
            rect.x0 + pad,
            footer_y,
            rect.x0 + pad + link_w,
            footer_y + control_h,
        );
        ui::widget::Button::paint(
            paint.scene,
            transform,
            paint.text,
            theme,
            &button_face(link, &link_label, false),
        );
        ui::widget::Button::paint(
            paint.scene,
            transform,
            paint.text,
            theme,
            &button_face(close, &close_label, false),
        );
        self.targets.push((Target::LinkOrCode, link));
        self.targets.push((Target::Close, close));
        let list_bottom = footer_y - gap;

        // A note above the list: a join that failed, a site not reached.
        let caption = theme.text(TextRole::Caption);
        if let Some(note) = self.note.as_deref() {
            let note = paint
                .text
                .elide_to_width_style(note, f32_of_f64(avail), &caption);
            paint.text.draw_style(
                paint.scene,
                &note,
                colors.text_primary,
                transform.pre_translate((rect.x0 + pad, y).into()),
                None,
                &caption,
            );
            y += f64::from(caption.line_height_px()) + gap;
        }

        // The list, or why there is none.
        let empty = match &self.status {
            Status::Loading => Some(t("directory-loading")),
            Status::Failed(error) => Some(t_args(
                "directory-failed",
                &[("error", error.clone().into())],
            )),
            Status::Ready if self.listings.is_empty() => Some(t("directory-empty")),
            Status::Ready if self.shown().is_empty() => Some(t_args(
                "directory-no-match",
                &[("query", self.query.trim().to_owned().into())],
            )),
            Status::Ready => None,
        };
        if let Some(empty) = empty {
            let line = paint
                .text
                .elide_to_width_style(&empty, f32_of_f64(avail), &caption);
            paint.text.draw_style(
                paint.scene,
                &line,
                colors.text_dim,
                transform.pre_translate((rect.x0 + pad, y + gap).into()),
                None,
                &caption,
            );
            return;
        }
        let now_ms = tilo_clock::unix_now_ms();
        let rows: Vec<DirectoryListing> = self
            .shown()
            .into_iter()
            .skip(self.scroll)
            .cloned()
            .collect();
        let ground = ui::chrome::shell::panel_ground(theme);
        for (offset, listing) in rows.iter().enumerate() {
            if y + ROW_H > list_bottom + 0.5 {
                break;
            }
            let index = self.scroll + offset;
            let row = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + ROW_H);
            if self.selected == Some(index) {
                paint.scene.fill(
                    Fill::NonZero,
                    transform,
                    rest_floor(theme),
                    None,
                    &RoundedRect::from_rect(row, f64::from(tokens.radii.control)),
                );
            }
            // The workspace's mark.
            let mark = Rect::new(
                row.x0 + gap,
                row.center().y - MARK / 2.0,
                row.x0 + gap + MARK,
                row.center().y + MARK / 2.0,
            );
            let identity = key_of(listing);
            ui::widget::AvatarFace::new(&listing.entry.name, &identity)
                .square()
                .paint(&mut paint, mark);
            // Its button: Join, Joining…, or Open once held.
            let label = if listing.joined {
                t("directory-open")
            } else if self.is_joining(listing) {
                t("directory-joining")
            } else if self.failure(listing).is_some() {
                t("directory-try-again")
            } else {
                t("directory-join")
            };
            let primary = !listing.joined;
            let button_w = button_face(Rect::ZERO, &label, primary)
                .intrinsic_width(paint.text, theme)
                .min(row.width() / 3.0);
            let button = Rect::new(
                row.x1 - gap - button_w,
                row.center().y - control_h / 2.0,
                row.x1 - gap,
                row.center().y + control_h / 2.0,
            );
            ui::widget::Button::paint(
                paint.scene,
                transform,
                paint.text,
                theme,
                &button_face(button, &label, primary).disabled(
                    (self.joining.is_some() && !self.is_joining(listing) && !listing.joined)
                        || self.progress(listing) == Some(&JoinProgress::Joining),
                ),
            );
            self.targets.push((Target::Row(index), button));
            // Who is here now, before the button.
            let faces_end = button.x0 - gap;
            let faces_start = draw_faces(&mut paint, listing, faces_end, row.center().y, ground);
            // Name, what it is, how lively.
            let text_x = mark.x1 + gap * 1.5;
            let text_w = (faces_start - gap - text_x).max(0.0);
            let name_style = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
            // A site's own workspace carries a tag saying whose it is,
            // after its name: two things, never joined into one string.
            let tag = listing.entry.community.then(|| {
                t_args(
                    "directory-community-tag",
                    &[("host", listing.host.clone().into())],
                )
            });
            let tag_w = tag.as_deref().map_or(0.0, |tag| {
                ui::widget::BadgeFace::intrinsic_width(tag, paint.text, theme).min(text_w / 2.0)
            });
            let name_w = if tag_w > 0.0 {
                (text_w - tag_w - gap).max(0.0)
            } else {
                text_w
            };
            let third = match self.failure(listing) {
                Some(reason) => (
                    t_args(
                        "directory-join-failed-line",
                        &[("error", reason.to_owned().into())],
                    ),
                    colors.semantic.danger,
                ),
                None => (activity_line(listing, now_ms), colors.text_dim),
            };
            let lines = [
                (listing.entry.name.clone(), name_style, colors.text_primary),
                (
                    if listing.entry.description.is_empty() {
                        listing.host.clone()
                    } else {
                        listing.entry.description.replace('\n', " ")
                    },
                    caption.clone(),
                    colors.text_dim,
                ),
                (third.0, caption.clone(), third.1),
            ];
            let line_h = f64::from(caption.line_height_px());
            let mut line_y = row.center().y - (line_h * 3.0) / 2.0 - 2.0;
            for (index, (content, style, color)) in lines.into_iter().enumerate() {
                let width = if index == 0 { name_w } else { text_w };
                let shown = paint
                    .text
                    .elide_to_width_style(&content, f32_of_f64(width), &style);
                paint.text.draw_style(
                    paint.scene,
                    &shown,
                    color,
                    transform.pre_translate((text_x, line_y).into()),
                    None,
                    &style,
                );
                let line = f64::from(style.line_height_px()).max(line_h);
                if index == 0
                    && let Some(tag) = tag.as_deref()
                {
                    let tag_x =
                        text_x + f64::from(paint.text.measure_style(&shown, None, &style).0) + gap;
                    ui::widget::BadgeFace::new(
                        Rect::new(tag_x, line_y, tag_x + tag_w, line_y + line),
                        tag,
                    )
                    .paint(paint.scene, transform, paint.text, theme);
                }
                line_y += line;
            }
            y += ROW_H;
        }
    }

    fn paint_search(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        field: Rect,
    ) {
        let placeholder = t("directory-search");
        let face = FieldFace::new(field, &self.query)
            .placeholder(&placeholder)
            .text_role(TextRole::Body)
            .focused(self.open);
        if self.query.is_empty() {
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
        input.set_text(&self.query);
        input.set_selection(self.query_selection.anchor, self.query_selection.cursor);
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

    /// The sheet for assistive technology: the search field, each listing
    /// as a button that joins (or opens) it, and the footer.
    #[must_use]
    pub fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let mut sheet = AccessNode::new(Role::Dialog)
            .with_name(t("directory-title"))
            .with_bounds(self.rect);
        let shown = self.shown();
        for (target, rect) in &self.targets {
            let node = match target {
                Target::Search => AccessNode::new(Role::TextInput)
                    .with_name(t("directory-search"))
                    .with_value(self.query.clone()),
                Target::LinkOrCode => {
                    AccessNode::new(Role::Button).with_name(t("directory-link-or-code"))
                }
                Target::Close => AccessNode::new(Role::Button).with_name(t("directory-close")),
                Target::Row(row) => {
                    let Some(listing) = shown.get(*row) else {
                        continue;
                    };
                    let verb = if listing.joined {
                        "directory-open-named"
                    } else {
                        "directory-join-named"
                    };
                    let standing = match self.progress(listing) {
                        Some(JoinProgress::Joining) => format!(" {}", t("directory-joining")),
                        Some(JoinProgress::Failed(reason)) => format!(
                            " {}",
                            t_args(
                                "directory-join-failed-line",
                                &[("error", reason.clone().into())]
                            )
                        ),
                        None => String::new(),
                    };
                    AccessNode::new(Role::Button)
                        .with_name(t_args(verb, &[("name", listing.entry.name.clone().into())]))
                        .with_description(format!(
                            "{} {}. {}.{standing}",
                            listing.entry.description,
                            activity_line(listing, tilo_clock::unix_now_ms()),
                            joins_as(listing)
                        ))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.selected == Some(*row)),
                        )
                }
            };
            sheet.children.push(
                node.with_bounds(*rect)
                    .with_action_target(format!("{TARGET_PREFIX}{}", self.slug(target))),
            );
        }
        Some(sheet)
    }

    /// What a target does, as its assistive name: never its position.
    fn slug(&self, target: &Target) -> String {
        match target {
            Target::Row(row) => format!(
                "join/{}",
                self.shown()
                    .get(*row)
                    .map(|listing| key_of(listing))
                    .unwrap_or_default()
            ),
            Target::Search => "search".into(),
            Target::LinkOrCode => "link-or-code".into(),
            Target::Close => "close".into(),
        }
    }

    /// Carry out `action` on the target `target` names.
    pub fn perform_accessibility_action(
        &mut self,
        target: &str,
        action: &Action,
    ) -> Option<DirectoryOutcome> {
        let part = target.strip_prefix(TARGET_PREFIX)?;
        if !self.open {
            return None;
        }
        if part == "search" {
            return match action {
                Action::SetValue(value) => {
                    self.set_query(value.chars().filter(|c| !c.is_control()).collect());
                    Some(DirectoryOutcome::consumed())
                }
                Action::Focus | Action::Activate | Action::ScrollIntoView => {
                    Some(DirectoryOutcome::consumed())
                }
                _ => None,
            };
        }
        let target = match part {
            "link-or-code" => Target::LinkOrCode,
            "close" => Target::Close,
            other => {
                let key = other.strip_prefix("join/")?;
                Target::Row(
                    self.shown()
                        .iter()
                        .position(|listing| key_of(listing) == key)?,
                )
            }
        };
        match action {
            Action::Activate => Some(self.activate(&target)),
            Action::Focus | Action::ScrollIntoView => Some(DirectoryOutcome::consumed()),
            _ => None,
        }
    }

    /// The painted targets by name, for harnesses.
    #[must_use]
    pub fn test_rects(&self) -> Vec<(String, Rect)> {
        self.targets
            .iter()
            .map(|(target, rect)| (format!("directory.{}", self.slug(target)), *rect))
            .collect()
    }
}

/// Whether `target` was published by this sheet.
#[must_use]
pub fn is_accessibility_target(target: &str) -> bool {
    target.starts_with(TARGET_PREFIX)
}

/// The people of `listing` in it now, ending at `x_end`: their faces, then
/// "+N" for the rest. Returns where they begin.
fn draw_faces(
    paint: &mut PaintCtx<'_>,
    listing: &DirectoryListing,
    x_end: f64,
    cy: f64,
    ground: vello::peniko::Color,
) -> f64 {
    let faces = &listing.entry.here;
    let total = usize::try_from(listing.entry.here_count)
        .unwrap_or(usize::MAX)
        .max(faces.len());
    if total == 0 {
        return x_end;
    }
    let shown = faces.len().min(FACES_SHOWN);
    let extra = total.saturating_sub(shown);
    let slots = shown.saturating_add(usize::from(extra > 0));
    let width = FACE + FACE_STEP * f64_of_usize(slots.saturating_sub(1));
    let x0 = x_end - width;
    for (k, face) in faces.iter().take(shown).enumerate() {
        let x = x0 + FACE_STEP * f64_of_usize(k);
        let rect = Rect::new(x, cy - FACE / 2.0, x + FACE, cy + FACE / 2.0);
        ui::widget::AvatarFace::new(&face.name, &face.user_public_key)
            .ringed_on(ground)
            .paint(paint, rect);
    }
    if extra > 0 {
        let x = x0 + FACE_STEP * f64_of_usize(shown);
        let rect = Rect::new(x, cy - FACE / 2.0, x + FACE, cy + FACE / 2.0);
        ui::widget::AvatarFace::paint_overflow(paint, rect, extra, Some(ground));
    }
    x0
}

/// A button's face: the Regular shared `Button`, the primary role for Join.
fn button_face(at: Rect, label: &str, primary: bool) -> ui::widget::ButtonFace<'_> {
    let face = ui::widget::ButtonFace::new(at, label);
    if primary {
        face.role(ButtonRole::Primary)
    } else {
        face
    }
}

/// The resting floor of the search field and the chosen row.
fn rest_floor(theme: &Theme) -> vello::peniko::Color {
    ui::chrome::selection_floor_ramp(
        theme,
        ui::chrome::SelectionState::active(false),
        ui::chrome::shell::panel_ground(theme),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::RoomId;
    use tilo_relay_protocol::directory::DirectoryEntry;

    fn listing(name: &str, joined: bool) -> DirectoryListing {
        let room = RoomId::new();
        DirectoryListing {
            entry: DirectoryEntry {
                room: room.to_string(),
                name: name.into(),
                description: format!("{name} people"),
                default_role: RoomRole::Commenter,
                door: String::new(),
                here: Vec::new(),
                here_count: 0,
                members: 2,
                last_active_ms: None,
                community: false,
            },
            room: Some(room),
            door: url::Url::parse(&format!("https://eu1.example/peer/v1/directory/{room}/")).ok(),
            site: None,
            host: "eu1.example".into(),
            joined,
        }
    }

    fn key(key: Key) -> WindowEvent {
        let event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
        ui::synthetic_input::keyboard_input(event)
    }

    fn typed(sheet: &mut DirectorySheet, text: &str) {
        for ch in text.chars() {
            let _ = sheet.handle_event(
                &key(Key::Character(ch.to_string().into())),
                Modifiers::default(),
                None,
            );
        }
    }

    #[test]
    fn opening_asks_the_directories_and_search_narrows_what_was_listed() {
        let mut sheet = DirectorySheet::default();
        assert_eq!(sheet.open(), DirectoryRequest::Fetch { site: None });
        sheet.show(vec![listing("Rust Club", false), listing("Bakers", false)]);
        assert_eq!(sheet.shown().len(), 2);
        typed(&mut sheet, "rust");
        let names: Vec<_> = sheet.shown().iter().map(|l| l.entry.name.clone()).collect();
        assert_eq!(names, ["Rust Club"]);
        // Enter with one answer joins it.
        let outcome = sheet.handle_event(
            &key(Key::Named(NamedKey::Enter)),
            Modifiers::default(),
            None,
        );
        assert!(
            matches!(outcome.request, Some(DirectoryRequest::Join(ref joined)) if joined.entry.name == "Rust Club")
        );
        assert!(sheet.is_joining(&sheet.shown()[0].clone()));
    }

    #[test]
    fn a_held_workspace_opens_instead_of_joining_and_joins_go_one_at_a_time() {
        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        sheet.show(vec![
            listing("Held", true),
            listing("New", false),
            listing("Other", false),
        ]);
        assert!(matches!(
            sheet.activate(&Target::Row(0)).request,
            Some(DirectoryRequest::Open(_))
        ));
        assert!(matches!(
            sheet.activate(&Target::Row(1)).request,
            Some(DirectoryRequest::Join(_))
        ));
        assert_eq!(
            sheet.activate(&Target::Row(2)).request,
            None,
            "the first join is still on its way"
        );
        sheet.failed("the host did not answer");
        assert!(
            sheet.is_open(),
            "a failed join leaves the sheet up to try again"
        );
        assert!(matches!(
            sheet.activate(&Target::Row(2)).request,
            Some(DirectoryRequest::Join(_))
        ));
        sheet.joined();
        assert!(!sheet.is_open(), "a join that finished closes the sheet");
    }

    #[test]
    fn a_workspace_joining_says_so_and_pressing_join_again_says_it_is_on_its_way() {
        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        let joining = listing("Lounge", false);
        let failed = listing("Garden", false);
        sheet.show(vec![joining.clone(), failed.clone()]);
        let state = |listing: &DirectoryListing, progress| SiteJoinState {
            room: listing.room.unwrap(),
            site: None,
            workspace: None,
            progress,
        };
        sheet.set_joins(vec![
            state(&joining, JoinProgress::Joining),
            state(&failed, JoinProgress::Failed("the host refused".into())),
        ]);
        assert!(
            sheet.is_joining(&joining),
            "a join running shows on its row"
        );
        assert_eq!(
            sheet.activate(&Target::Row(0)).request,
            None,
            "a join already on its way is not asked again"
        );
        let note = sheet
            .note
            .clone()
            .expect("the sheet says why nothing happened");
        assert!(note.contains("already joining Lounge"), "{note}");
        assert_eq!(sheet.failure(&failed), Some("the host refused"));
        assert!(
            matches!(
                sheet.activate(&Target::Row(1)).request,
                Some(DirectoryRequest::Join(_))
            ),
            "a failed join is tried again"
        );
    }

    #[test]
    fn a_short_list_makes_a_short_sheet() {
        let theme = Theme::default();
        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        sheet.show(vec![listing("Lounge", false)]);
        let one = sheet.content_height(&theme);
        sheet.show(
            (0..12)
                .map(|k| listing(&format!("Room {k}"), false))
                .collect(),
        );
        let twelve = sheet.content_height(&theme);
        assert!(
            one < MAX_HEIGHT,
            "one row does not fill the tallest sheet: {one}"
        );
        assert!((twelve - one - ROW_H * 11.0).abs() < 0.5, "{one} {twelve}");
    }

    #[test]
    fn enter_looks_up_a_typed_site_and_hands_links_and_codes_to_the_join_field() {
        assert_eq!(
            typed_request("example.org"),
            Some(DirectoryRequest::Fetch {
                site: Some("example.org".into())
            })
        );
        let room = RoomId::new();
        let link = format!("https://app.example/app/#/@rooms.example/w/{room}?invite=abc");
        assert_eq!(
            typed_request(&link),
            Some(DirectoryRequest::LinkOrCode(link.clone()))
        );
        assert_eq!(
            typed_request("rust"),
            None,
            "a word that is no invitation searches"
        );
        assert_eq!(typed_request("   "), None);

        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        sheet.show(Vec::new());
        assert_eq!(
            sheet.activate(&Target::LinkOrCode).request,
            Some(DirectoryRequest::LinkOrCode(String::new())),
            "nothing typed: the field opens empty"
        );
    }

    #[test]
    fn escape_clears_the_search_first_then_closes() {
        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        sheet.show(vec![listing("A", false)]);
        typed(&mut sheet, "zz");
        let _ = sheet.handle_event(
            &key(Key::Named(NamedKey::Escape)),
            Modifiers::default(),
            None,
        );
        assert!(sheet.is_open());
        assert_eq!(sheet.query(), "");
        let _ = sheet.handle_event(
            &key(Key::Named(NamedKey::Escape)),
            Modifiers::default(),
            None,
        );
        assert!(!sheet.is_open());
    }

    #[test]
    fn the_activity_line_says_who_is_here_and_how_many_belong() {
        let mut busy = listing("Busy", false);
        busy.entry.here_count = 3;
        busy.entry.members = 12;
        let line = activity_line(&busy, 0);
        assert!(line.contains('3'), "{line}");
        assert!(line.contains("12"), "{line}");
        assert_eq!(
            line.matches(" · ").count(),
            1,
            "a row says one thing: {line}"
        );
        assert!(joins_as(&busy).contains(&role_label(RoomRole::Commenter)));
    }
}
