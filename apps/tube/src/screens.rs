//! Screen composition. Each `fn` here takes the immutable [`TubeModel`] (in a
//! [`Ui`] context) and returns a widget subtree built from the toolkit's
//! semantic widgets: media cards in media grids, sections, forms, buttons
//! with roles. There is no per-screen state: navigation rebuilds the screen
//! from the model, so these functions are pure views. The window shell (the
//! navigation rail, the header row and its actions) lives in [`crate::app`].

use std::sync::{Arc, Mutex};

use ui::TextRole;
use ui::icons::Icon;
use ui::search::{SearchRequest, open_search_binding};
use ui::theme::Theme;
use ui::widget::{
    Avatar, AvatarSize, Badge, Banner, Button, ButtonRole, Chip, ChipLead, ChipStyle, Column,
    Container, ControlSize, Dropdown, Element, EmptyState, Field, Form, FormSection, ListRow,
    MediaAspect, MediaCard, MediaGrid, MediaSize, MediaSource, OwnedBadge, Row, Scrollable,
    Section, SectionHeader, Skeleton, SkeletonShape, Spacer, Text, TextInput, Toggle, Tone,
    card_column_width, card_columns,
};

use crate::action::{self, Action, ActionBus, Invocations, SettingFlag};
use crate::actions::{TubeAction, TubeEffect};
use crate::i18n::{arg, t, t_args};
use crate::images::Imgs;
use crate::model::{Category, Route, SearchType, SkipMode, TubeModel, Video};
use crate::util::{clock, human_count, human_views};
use crate::widgets::{PlayerHost, SharedPlayer};

/// Fraction of the content viewport height the 16:9 player may fill, so the
/// title and actions still show below it.
const PLAYER_VH_FRAC: f64 = 0.65;

/// Everything a screen reads: the model, the theme it is built for, the
/// action queue its controls write to, the image store, and the queue of
/// resource invocations for controls that report through a closure.
#[derive(Clone, Copy)]
pub struct Ui<'a> {
    pub model: &'a TubeModel,
    pub theme: &'a Theme,
    pub bus: &'a ActionBus,
    pub imgs: Imgs<'a>,
    pub invocations: &'a Invocations,
    /// Width of the shell's content column, which sizes the media grids.
    pub content_width: f64,
}

impl Ui<'_> {
    /// A callback that queues `action` on the bus.
    fn push(&self, action: Action) -> impl FnMut() + 'static {
        let bus = self.bus.clone();
        move || action::push(&bus, action.clone())
    }
}

// ---------------------------------------------------------------------------
// shared pieces
// ---------------------------------------------------------------------------

fn text(s: &str, role: TextRole) -> Text {
    Text::new(s).with_role(role)
}

/// A vertical stack as tall as its content, with no inset of its own.
fn stack() -> Column {
    Column::new().with_no_padding().with_intrinsic_height()
}

/// `child` taking the room its row has left.
fn grow(child: impl ui::widget::Widget + 'static) -> Container {
    Container::new()
        .with_no_padding()
        .with_flex_grow()
        .with_child(Box::new(child))
}

/// A vertical page: groups `rhythm.group` apart inside the page gutter.
fn page(children: Vec<Element>) -> Scrollable {
    let mut col = Column::new()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Group);
    for child in children {
        col = col.push(child);
    }
    Scrollable::new_vertical()
        .with_stable_vertical_gutter()
        .with_child(Box::new(col))
}

/// Art for a card: the image from the store when there is one, the toolkit's
/// generated placeholder otherwise (offline, `--shots`, no thumbnail).
fn art(imgs: Imgs<'_>, url: &str) -> MediaSource {
    match imgs.store_handle() {
        Some(store) if !url.is_empty() => MediaSource::Url {
            store,
            url: url.to_string(),
        },
        _ => MediaSource::Placeholder,
    }
}

/// A creator's avatar: the picture from the store, initials until it lands.
fn channel_avatar(ui: Ui<'_>, channel_id: &str, name: &str, size: AvatarSize) -> Avatar {
    let url = ui
        .model
        .channel(channel_id)
        .map(|c| c.avatar_url.clone())
        .unwrap_or_default();
    let avatar = Avatar::person(name, channel_id).size(size);
    match ui.imgs.store_handle() {
        Some(store) if !url.is_empty() => avatar.with_image_url(store, url),
        _ => avatar,
    }
}

/// "1.2M views, 3 days ago": the reach and age of a video as one phrase.
fn views_and_age(views: u64, published: &str) -> String {
    let published = published.trim();
    match (views > 0, published.is_empty()) {
        (true, false) => t_args(
            "tube-views-and-age",
            &[arg("views", human_views(views)), arg("age", published)],
        ),
        (true, true) => human_views(views),
        (false, false) => published.to_string(),
        (false, true) => String::new(),
    }
}

/// "12K subscribers", or `None` when the count is unknown.
fn subscribers(count: u64) -> Option<String> {
    (count > 0).then(|| {
        t_args(
            "tube-subscribers",
            &[arg("count", count), arg("shown", human_count(count))],
        )
    })
}

/// One video as a media card: thumbnail, title, creator, reach and age.
fn video_card(ui: Ui<'_>, v: &Video) -> MediaCard {
    let dearrow = ui.model.settings.dearrow_titles_on();
    let mut card = MediaCard::new(
        format!("video_card_{}", v.id),
        art(ui.imgs, ui.model.display_thumbnail_url(v)),
    )
    .aspect(MediaAspect::Wide)
    .title(v.display_title(dearrow))
    .meta(&v.channel_name)
    .creator(&v.channel_name, &v.channel_id);
    if let Some(store) = ui.imgs.store_handle()
        && let Some(channel) = ui.model.channel(&v.channel_id)
        && !channel.avatar_url.is_empty()
    {
        card = card.creator_image_url(store, &channel.avatar_url);
    }
    if ui
        .model
        .channel(&v.channel_id)
        .is_some_and(|channel| channel.verified)
    {
        card = card.meta_mark(Icon::CircleCheck);
    }
    let reach = if v.live {
        t_args("tube-watching", &[arg("shown", human_count(v.views))])
    } else {
        views_and_age(v.views, &v.published)
    };
    if !reach.is_empty() {
        card = card.meta(reach);
    }
    card = if v.live {
        card.badge(OwnedBadge::Live)
    } else if v.duration > 0 {
        card.badge(OwnedBadge::Duration(clock(f64::from(v.duration))))
    } else {
        card
    };
    card.on_activate(ui.push(Action::OpenVideo(v.id.clone())))
}

/// The grid's columns at the page's width: how many cards fit, and how wide
/// each is. Every grid of the app shares them, so a card is the same size on
/// every page.
fn grid_columns(ui: Ui<'_>) -> (usize, f64) {
    let theme = ui.theme;
    let gap = f64::from(theme.tokens.spacing.rhythm.section);
    let width = (ui.content_width
        - 2.0 * f64::from(theme.tokens.spacing.rhythm.group)
        - f64::from(theme.tokens.metrics.scrollbar_gutter))
    .max(1.0);
    let columns = card_columns(width, MediaSize::Regular.extent(theme), gap);
    (columns, card_column_width(width, columns, gap))
}

/// Videos as media cards. A grid shorter than one row keeps the card size of
/// a full one instead of stretching its few cards across the page.
fn video_grid<'v>(ui: Ui<'_>, videos: impl IntoIterator<Item = &'v Video>) -> Element {
    let cards: Vec<MediaCard> = videos.into_iter().map(|v| video_card(ui, v)).collect();
    // The shared grid chooses tracks from the full page width even when some
    // are empty. Shrinking its parent would make it choose fewer, wider tracks.
    Box::new(MediaGrid::new(cards))
}

/// The loading state of a video grid: two rows of card-shaped placeholders
/// in the grid's own columns, each as tall as a card with its two title
/// lines and meta, so the arriving cards replace them without moving
/// anything.
fn skeleton_grid(ui: Ui<'_>) -> Column {
    let theme = ui.theme;
    let (columns, column) = grid_columns(ui);
    let mut grid = stack().with_rhythm(ui::tokens::Rhythm::Group);
    for _ in 0..2 {
        let mut row = Row::new()
            .no_wrap()
            .align_start()
            .with_rhythm(ui::tokens::Rhythm::Section);
        for _ in 0..columns {
            row = row.push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(column as f32)
                    .with_child(Box::new(
                        stack()
                            .with_gap(theme.tokens.spacing.s0)
                            .push(Box::new(Skeleton::new(SkeletonShape::Media(
                                MediaAspect::Wide,
                            ))))
                            .push(Box::new(Skeleton::new(SkeletonShape::Text { lines: 2 }))),
                    )),
            ));
        }
        grid = grid.push(Box::new(row));
    }
    grid
}

fn search_action() -> ui::tilo_actions::ActionBinding<()> {
    open_search_binding(&SearchRequest::this_app())
}

// ---------------------------------------------------------------------------
// header row
// ---------------------------------------------------------------------------

/// The content column's title for `route`, for the window's header row.
/// Pages that title themselves (a video, a creator, a playlist) leave it
/// empty.
pub fn title(route: &Route) -> String {
    match route {
        Route::Home => t("tube-nav-home"),
        Route::Trending => t("tube-nav-trending"),
        Route::Subscriptions => t("tube-nav-subscriptions"),
        Route::Queue => t("tube-nav-queue"),
        Route::History => t("tube-nav-history"),
        Route::Settings => t("tube-nav-settings"),
        Route::Search(query) => format!("Search: {query}"),
        Route::Watch(_) | Route::Channel(_) | Route::Playlist(_) => String::new(),
    }
}

/// The content column's header actions: the connection status while the
/// catalogue is still on its way, the page's own action, and the search
/// launcher.
pub fn header_actions(ui: Ui<'_>) -> Vec<Element> {
    let mut actions: Vec<Element> = Vec::new();
    if let Some(status) = connection_badge(ui.model) {
        actions.push(Box::new(status));
    }
    if matches!(ui.model.route, Route::History) && !ui.model.history.is_empty() {
        actions.push(Box::new(
            Button::new("clear_history", &t("tube-clear-history"))
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .on_click(ui.push(Action::Effect(TubeEffect::ClearHistory))),
        ));
    }
    actions.push(Box::new(ui::search::search_button(
        SearchRequest::this_app(),
    )));
    actions
}

/// "Connecting" while the catalogue is on its way. Being offline is not
/// repeated here: a page with nothing to show says so itself, and a request
/// that fails says so in its toast.
fn connection_badge(model: &TubeModel) -> Option<Badge> {
    (!model.data_source.live && model.data_source.label.contains("connect"))
        .then(|| Badge::text(t("tube-connecting")).style(ChipStyle::Tinted(Tone::Info)))
}

// ---------------------------------------------------------------------------
// browse screens
// ---------------------------------------------------------------------------

pub fn home(ui: Ui<'_>) -> Scrollable {
    let model = ui.model;
    let body: Element = if !model.videos.is_empty() {
        video_grid(ui, &model.videos)
    } else if model.feed_loading {
        Box::new(skeleton_grid(ui))
    } else if let Some(error) = &model.feed_error {
        Box::new(EmptyState::error(t("tube-home-error"), error).with_icon(Icon::WifiOff))
    } else if model.data_source.label.contains("offline") {
        Box::new(
            EmptyState::new(t("tube-offline-title"))
                .with_icon(Icon::WifiOff)
                .with_message(t("tube-offline-message")),
        )
    } else {
        Box::new(
            EmptyState::new(t("tube-home-empty-title"))
                .with_icon(Icon::Play)
                .with_message(t("tube-home-empty-message"))
                .with_action(t("tube-search"))
                .on_action(search_action()),
        )
    };
    page(vec![body])
}

pub fn trending(ui: Ui<'_>) -> Scrollable {
    let mut videos: Vec<&Video> = ui.model.videos.iter().collect();
    videos.sort_by_key(|v| std::cmp::Reverse(v.views));
    let body: Element = if videos.is_empty() {
        Box::new(
            EmptyState::new(t("tube-trending-empty-title"))
                .with_icon(Icon::Flame)
                .with_message(t("tube-trending-empty-message")),
        )
    } else {
        video_grid(ui, videos)
    };
    page(vec![body])
}

pub fn subscriptions(ui: Ui<'_>) -> Scrollable {
    let model = ui.model;
    let mut creators = Row::new().align_start();
    let mut any = false;
    for id in &model.subscriptions {
        let Some(channel) = model.channel(id) else {
            continue;
        };
        any = true;
        // A tile as wide as a hero avatar: the creator's picture over their
        // name, both centred.
        creators = creators.push(Box::new(
            Container::new()
                .with_width(AvatarSize::Hero.side(ui.theme) as f32)
                .interactive(
                    format!("creator_{}", channel.id),
                    ui.push(Action::OpenChannel(channel.id.clone())),
                )
                .with_child(Box::new(
                    stack()
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .push(Box::new(
                            Row::new()
                                .no_wrap()
                                .with_justify(taffy::JustifyContent::Center)
                                .push(Box::new(channel_avatar(
                                    ui,
                                    &channel.id,
                                    &channel.name,
                                    AvatarSize::Large,
                                ))),
                        ))
                        .push(Box::new(
                            text(&channel.name, TextRole::Caption)
                                .with_align(ui::text_render::ParagraphAlign::Center),
                        )),
                )),
        ));
    }
    let feed = model.subscription_feed();
    let latest: Element = if feed.is_empty() {
        Box::new(
            EmptyState::new(t("tube-subscriptions-empty-title"))
                .with_icon(Icon::Bell)
                .with_message(t("tube-subscriptions-empty-message")),
        )
    } else {
        Box::new(
            Section::new(vec![video_grid(ui, feed)])
                .with_header(SectionHeader::heading(t("tube-latest-videos"))),
        )
    };
    let mut children: Vec<Element> = Vec::new();
    if any {
        children.push(Box::new(creators));
    }
    children.push(latest);
    page(children)
}

pub fn history(ui: Ui<'_>) -> Scrollable {
    let model = ui.model;
    let mut children: Vec<Element> = Vec::new();
    if let Some(recovery) = &model.history_notice {
        children.push(Box::new(
            Banner::warning(t("tube-history-not-saving")).with_message(recovery.clone()),
        ));
    }
    let videos = model.history_videos();
    if videos.is_empty() {
        children.push(Box::new(
            EmptyState::new(t("tube-history-empty-title"))
                .with_icon(Icon::History)
                .with_message(t("tube-history-empty-message")),
        ));
    } else {
        children.push(video_grid(ui, videos));
    }
    page(children)
}

/// The shared queue: membership syncs with collaborators, while each device
/// keeps its own playback order.
pub fn queue(ui: Ui<'_>) -> Scrollable {
    let model = ui.model;
    let videos: Vec<&Video> = model
        .queue
        .iter()
        .filter_map(|id| model.video(id))
        .collect();
    let waiting = model.queue.len().saturating_sub(videos.len());
    let mut children: Vec<Element> = Vec::new();
    if videos.is_empty() && waiting == 0 {
        children.push(Box::new(
            EmptyState::new(t("tube-queue-empty-title"))
                .with_icon(Icon::ListVideo)
                .with_message(t("tube-queue-empty-message")),
        ));
    } else {
        children.push(video_grid(ui, videos));
    }
    if waiting > 0 {
        children.push(Box::new(
            text(
                &t_args("tube-queue-waiting", &[arg("count", waiting)]),
                TextRole::Caption,
            )
            .dimmed(),
        ));
    }
    Scrollable::new_vertical().with_child(Box::new(
        children.into_iter().fold(
            Column::new()
                .with_id("queue_page")
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Group),
            Column::push,
        ),
    ))
}

pub fn playlist(ui: Ui<'_>, id: &str) -> Scrollable {
    let model = ui.model;
    let Some(pl) = model.playlist(id) else {
        return page(vec![Box::new(
            EmptyState::new(t("tube-playlist-missing")).with_icon(Icon::ListVideo),
        )]);
    };
    let videos: Vec<&Video> = pl
        .video_ids
        .iter()
        .filter_map(|vid| model.video(vid))
        .collect();

    let rhythm = &ui.theme.tokens.spacing.rhythm;
    // The facts read as one block; the action stands a section step below.
    let mut info = stack().with_gap(rhythm.section).push(Box::new(
        stack()
            .with_gap(rhythm.tight)
            .push(Box::new(text(&pl.title, TextRole::Title)))
            .push(Box::new(text(&pl.channel_name, TextRole::Body).dimmed()))
            .push(Box::new(
                text(
                    &t_args(
                        "tube-video-count",
                        &[
                            arg("count", pl.video_count),
                            arg("shown", human_count(pl.video_count)),
                        ],
                    ),
                    TextRole::Caption,
                )
                .numeric()
                .dimmed(),
            )),
    ));
    if let Some(first) = videos.first() {
        info = info.push(Box::new(
            Button::new("playlist_play", &t("tube-play-all"))
                .with_icon(Icon::Play)
                .role(ButtonRole::Primary)
                .on_click(ui.push(Action::OpenVideo(first.id.clone()))),
        ));
    }
    // The cover shows once there is real art for it; a generated placeholder
    // beside the title would only repeat it.
    let mut header = Row::new()
        .no_wrap()
        .align_start()
        .with_rhythm(ui::tokens::Rhythm::Group);
    if !pl.thumbnail_url.is_empty() && ui.imgs.store_handle().is_some() {
        header = header.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_width(MediaSize::Regular.extent(ui.theme) as f32)
                .with_child(Box::new(
                    MediaCard::new(
                        format!("playlist_cover_{}", pl.id),
                        art(ui.imgs, &pl.thumbnail_url),
                    )
                    .aspect(MediaAspect::Wide),
                )),
        ));
    }
    let header = header.push(Box::new(grow(info)));

    let list: Element = if videos.is_empty() {
        Box::new(skeleton_grid(ui))
    } else {
        Box::new(Section::titled(
            t("tube-videos"),
            vec![video_grid(ui, videos)],
        ))
    };
    let mut children: Vec<Element> = vec![Box::new(header)];
    children.push(list);
    page(children)
}

pub fn channel(ui: Ui<'_>, id: &str) -> Scrollable {
    let model = ui.model;
    let Some(ch) = model.channel(id) else {
        return page(vec![Box::new(
            EmptyState::new(t("tube-creator-missing")).with_icon(Icon::Users),
        )]);
    };
    let mut children: Vec<Element> = Vec::new();
    if !ch.banner_url.is_empty() && ui.imgs.store_handle().is_some() {
        children.push(Box::new(
            MediaCard::new(
                format!("channel_banner_{}", ch.id),
                art(ui.imgs, &ch.banner_url),
            )
            .aspect(MediaAspect::Ratio(6.2)),
        ));
    }

    let mut name = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(Box::new(text(&ch.name, TextRole::Title)));
    if ch.verified {
        name = name.push(Box::new(ui::widget::IconWidget::new(Icon::CircleCheck)));
    }
    let mut about = stack()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(Box::new(name));
    if !ch.handle.is_empty() && ch.handle != ch.id {
        about = about.push(Box::new(
            text(&ch.handle, TextRole::Caption)
                .with_kind(ui::TextKind::Identifier)
                .dimmed(),
        ));
    }
    if let Some(subs) = subscribers(ch.subscribers) {
        about = about.push(Box::new(text(&subs, TextRole::Caption).dimmed()));
    }
    let header = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Section)
        .push(Box::new(channel_avatar(
            ui,
            &ch.id,
            &ch.name,
            AvatarSize::Hero,
        )))
        .push(Box::new(grow(about)))
        .push(Box::new(subscribe_button(ui, &ch.id)));
    children.push(Box::new(header));
    if !ch.description.trim().is_empty() {
        children.push(Box::new(text(&ch.description, TextRole::Body).dimmed()));
    }
    let videos = model.channel_videos(id);
    if videos.is_empty() {
        children.push(Box::new(skeleton_grid(ui)));
    } else {
        children.push(Box::new(Section::titled(
            t("tube-videos"),
            vec![video_grid(ui, videos)],
        )));
    }
    page(children)
}

fn subscribe_button(ui: Ui<'_>, channel_id: &str) -> Button {
    let action = Action::Edit(TubeAction::SetSubscription {
        channel_id: channel_id.to_string(),
        subscribed: !ui.model.is_subscribed(channel_id),
    });
    if ui.model.is_subscribed(channel_id) {
        Button::new("subscribe_btn", &t("tube-subscribed"))
            .with_icon(Icon::Bell)
            .on_click(ui.push(action))
    } else {
        Button::new("subscribe_btn", &t("tube-subscribe"))
            .role(ButtonRole::Primary)
            .on_click(ui.push(action))
    }
}

// ---------------------------------------------------------------------------
// watch page
// ---------------------------------------------------------------------------

/// The watch page: the player, then the video's details beside "Up next" on
/// a wide window (stacked under them on a narrow one).
pub fn watch(ui: Ui<'_>, id: &str, content_size: (f64, f64), player: SharedPlayer) -> Scrollable {
    let model = ui.model;
    let theme = ui.theme;
    let (content_w, content_h) = content_size;
    let Some(v) = model.video(id) else {
        return page(vec![Box::new(
            EmptyState::new(t("tube-video-missing")).with_icon(Icon::Film),
        )]);
    };

    prepare_player(ui, v, &player);

    // The player is sized by height (more picture, less letterbox), capped so
    // the title still shows below it, and centred in the column.
    let player_w = (content_h * PLAYER_VH_FRAC * 16.0 / 9.0)
        .min(content_w)
        .max(1.0);
    let player_h = (player_w * 9.0 / 16.0).max(1.0) as f32;
    let stage = Row::new()
        .no_wrap()
        .with_justify(taffy::JustifyContent::Center)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_size(player_w as f32, player_h)
                .with_child(Box::new(PlayerHost::new(player, player_h).with_poster(
                    ui.imgs.store_handle(),
                    ui.model.display_thumbnail_url(v),
                    v.display_title(model.settings.dearrow_titles_on()),
                ))),
        ));

    let details = watch_details(ui, v);
    let up_next = up_next(ui, id);

    let gutter = f64::from(theme.tokens.spacing.rhythm.group);
    let metrics = &theme.tokens.metrics;
    let side = f64::from(metrics.inspector_width);
    let two_columns =
        content_w - 2.0 * gutter >= f64::from(metrics.content_min_width) + side + gutter;
    let lower: Element = if two_columns {
        Box::new(
            Row::new()
                .no_wrap()
                .align_start()
                .with_rhythm(ui::tokens::Rhythm::Group)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(details)),
                ))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_width(side as f32)
                        .with_child(Box::new(up_next)),
                )),
        )
    } else {
        Box::new(
            stack()
                .with_rhythm(ui::tokens::Rhythm::Group)
                .push(Box::new(details))
                .push(Box::new(up_next)),
        )
    };

    // The player sits under the header row, whose Back returns to the page
    // this one was opened from; the details below it stand in the gutter.
    let content = stack()
        .with_gap(theme.tokens.spacing.s0)
        .push(Box::new(stage))
        .push(Box::new(Column::new().with_intrinsic_height().push(lower)));
    Scrollable::new_vertical().with_child(Box::new(content))
}

/// Hand the shared player this video's `SponsorBlock` segments, the quality
/// choice and, inside a segment the person skips by hand, the Skip command.
fn prepare_player(ui: Ui<'_>, v: &Video, player: &SharedPlayer) {
    use ui::widget::video_player::PlayerSetting;
    let model = ui.model;
    let duration = v.duration.max(1) as f32;
    let marks = if model.settings.sponsorblock_enabled {
        v.sponsor
            .iter()
            .filter(|s| model.settings.category_enabled(s.category))
            .map(|s| {
                (
                    s.start as f32 / duration,
                    s.end as f32 / duration,
                    category_colour(ui.theme, s.category),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut player = player.borrow_mut();
    player.set_sponsor_marks(marks);

    let qualities: Vec<String> = crate::app::QUALITIES
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    let selected = crate::app::QUALITIES
        .iter()
        .position(|q| *q == model.settings.preferred_quality)
        .unwrap_or(0);
    player.set_setting(PlayerSetting::choice(
        "resolution",
        Icon::Monitor,
        t("tube-resolution"),
        1,
        qualities,
        selected,
    ));

    let manual = v
        .sponsor
        .iter()
        .find(|s| model.settings.should_show_skip(s.category) && s.contains(model.playhead));
    match manual {
        Some(segment) => player.set_setting(PlayerSetting::button(
            SKIP_SETTING,
            Icon::SkipForward,
            t_args(
                "tube-skip-segment",
                &[arg("segment", segment.category.label().to_lowercase())],
            ),
            2,
        )),
        None => player.remove_setting(SKIP_SETTING),
    }
}

/// The player command that skips the segment under the playhead.
pub const SKIP_SETTING: &str = "skip-segment";

/// A `SponsorBlock` category's colour on the timeline and in Settings: one
/// series of the theme's presence palette per category, so every category
/// stays distinct and every theme keeps them legible.
pub fn category_colour(theme: &Theme, category: Category) -> vello::peniko::Color {
    let index = Category::ALL
        .iter()
        .position(|c| *c == category)
        .unwrap_or(0);
    theme.tokens.colors.presence.series(index)
}

fn watch_details(ui: Ui<'_>, v: &Video) -> Column {
    let model = ui.model;
    let theme = ui.theme;
    let rhythm = &theme.tokens.spacing.rhythm;
    let dearrow = model.settings.dearrow_titles_on();

    let mut facts = Row::new().with_rhythm(ui::tokens::Rhythm::Section);
    let mut fact = |s: String| {
        facts = std::mem::take(&mut facts)
            .push(Box::new(text(&s, TextRole::Caption).numeric().dimmed()));
    };
    if v.views > 0 {
        fact(human_views(v.views));
    }
    if !v.published.trim().is_empty() {
        fact(v.published.clone());
    }
    if model.settings.return_dislike_enabled
        && let Some(dislikes) = model.dislikes_of(&v.id)
    {
        facts = facts.push(Box::new(
            text(
                &t_args(
                    "tube-dislikes",
                    &[arg("count", dislikes), arg("shown", human_count(dislikes))],
                ),
                TextRole::Caption,
            )
            .numeric()
            .dimmed()
            .with_id("tube-dislikes"),
        ));
    }

    let heading = stack()
        .with_gap(rhythm.tight)
        .push(Box::new(
            text(v.display_title(dearrow), TextRole::Title).with_heading_level(1),
        ))
        .push(Box::new(facts));

    let mut details = stack()
        .with_gap(rhythm.section)
        .push(Box::new(heading))
        .push(Box::new(watch_actions(ui, v)))
        .push(Box::new(creator_row(ui, v)));
    if !v.description.trim().is_empty() {
        details = details.push(Box::new(text(&v.description, TextRole::Body)));
    }
    details.push(Box::new(comments(ui, v)))
}

/// Like (with the like count), queue and copy link, then playback settings
/// at the trailing edge. Each is a control with a face, so the row's edges
/// line up with the title above and with Subscribe below.
fn watch_actions(ui: Ui<'_>, v: &Video) -> Row {
    let model = ui.model;
    let liked = model.is_liked(&v.id);
    let queued = model.is_queued(&v.id);
    let like_count = model.display_likes(v);
    let like_label = if like_count > 0 {
        human_count(like_count)
    } else {
        t("tube-like")
    };
    let like_name = if like_count > 0 {
        t_args(
            "tube-like-count",
            &[arg(
                "likes",
                t_args(
                    "tube-likes",
                    &[arg("count", like_count), arg("shown", like_label.as_str())],
                ),
            )],
        )
    } else {
        t("tube-like")
    };
    let like = Button::new("watch_like", &like_label)
        .with_icon(Icon::ThumbsUp)
        .with_accessible_name(&like_name)
        .toggled(liked)
        .on_click(ui.push(Action::Edit(TubeAction::SetVideoLiked {
            video_id: v.id.clone(),
            liked: !liked,
        })));
    let queue = Button::new(
        "watch_queue",
        &t(if queued {
            "tube-remove-from-queue"
        } else {
            "tube-add-to-queue"
        }),
    )
    .with_icon(Icon::ListVideo);
    let queue = queue.on_click(ui.push(Action::Edit(TubeAction::SetQueueMembership {
        video_id: v.id.clone(),
        queued: !queued,
    })));
    let copy_link = Button::new("watch_copy_link", &t("tube-copy-link"))
        .with_icon(Icon::Copy)
        .on_click(ui.push(Action::Share(v.id.clone())));
    let settings = Button::icon_only(
        "watch_settings",
        Icon::Settings,
        &t("tube-playback-settings"),
    )
    .role(ButtonRole::Secondary)
    .on_click(ui.push(Action::Navigate(Route::Settings)));
    Row::new()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(Box::new(like))
        .push(Box::new(queue))
        .push(Box::new(copy_link))
        .push(Box::new(Spacer::new()))
        .push(Box::new(settings))
}

/// The creator's avatar and name (opening the creator) and Subscribe.
fn creator_row(ui: Ui<'_>, v: &Video) -> Row {
    let model = ui.model;
    let rhythm = &ui.theme.tokens.spacing.rhythm;
    let mut name = stack()
        .with_gap(rhythm.tight)
        .push(Box::new(text(&v.channel_name, TextRole::Body).strong()));
    if let Some(subs) = model
        .channel(&v.channel_id)
        .and_then(|c| subscribers(c.subscribers))
    {
        name = name.push(Box::new(text(&subs, TextRole::Caption).dimmed()));
    }
    // No inset: the avatar stands on the page's leading edge, like the title.
    let creator = Container::new()
        .with_no_padding()
        .with_flex_grow()
        .interactive(
            "watch_creator",
            ui.push(Action::OpenChannel(v.channel_id.clone())),
        )
        .with_child(Box::new(
            Row::new()
                .no_wrap()
                .with_gap(rhythm.sibling)
                .push(Box::new(channel_avatar(
                    ui,
                    &v.channel_id,
                    &v.channel_name,
                    AvatarSize::Regular,
                )))
                .push(Box::new(grow(name))),
        ));
    Row::new()
        .no_wrap()
        .with_gap(rhythm.section)
        .push(Box::new(creator))
        .push(Box::new(subscribe_button(ui, &v.channel_id)))
}

fn comments(ui: Ui<'_>, v: &Video) -> Section {
    let header = SectionHeader::heading(t("tube-comments")).count(v.comments.len());
    if !ui.model.settings.show_comments {
        return Section::new(vec![Box::new(
            text(&t("tube-comments-off"), TextRole::Body).dimmed(),
        )])
        .with_header(header);
    }
    let rows: Vec<Element> = v
        .comments
        .iter()
        .map(|c| Box::new(comment(ui, c)) as Element)
        .collect();
    Section::new(rows).with_header(header)
}

fn comment(ui: Ui<'_>, c: &crate::model::Comment) -> Row {
    let rhythm = &ui.theme.tokens.spacing.rhythm;
    let mut head = Row::new()
        .with_gap(rhythm.related)
        .push(Box::new(text(&c.author, TextRole::Label).strong()))
        .push(Box::new(text(&c.time_ago, TextRole::Caption).dimmed()));
    if c.pinned {
        head = head.push(Box::new(
            Badge::text(t("tube-pinned")).style(ChipStyle::Neutral),
        ));
    }
    let mut reactions = Row::new().with_rhythm(ui::tokens::Rhythm::Section);
    if c.likes > 0 {
        reactions = reactions.push(Box::new(
            text(
                &t_args(
                    "tube-likes",
                    &[arg("count", c.likes), arg("shown", human_count(c.likes))],
                ),
                TextRole::Caption,
            )
            .numeric()
            .dimmed(),
        ));
    }
    if c.replies > 0 {
        reactions = reactions.push(Box::new(
            text(
                &t_args(
                    "tube-replies",
                    &[
                        arg("count", c.replies),
                        arg("shown", human_count(u64::from(c.replies))),
                    ],
                ),
                TextRole::Caption,
            )
            .numeric()
            .dimmed(),
        ));
    }
    if c.hearted {
        reactions = reactions.push(Box::new(
            text(&t("tube-hearted"), TextRole::Caption).dimmed(),
        ));
    }
    Row::new()
        .no_wrap()
        .align_start()
        .with_gap(rhythm.sibling)
        .push(Box::new(Avatar::person(&c.author, &c.author)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(
                    stack()
                        .with_gap(rhythm.tight)
                        .push(Box::new(head))
                        .push(Box::new(text(&c.text, TextRole::Body)))
                        .push(Box::new(reactions)),
                )),
        ))
}

fn up_next(ui: Ui<'_>, id: &str) -> Column {
    let rhythm = &ui.theme.tokens.spacing.rhythm;
    let enabled = !ui.model.settings.autoplay;
    let autoplay = Toggle::from_state(&ui::LocalState::new(ui.model.settings.autoplay))
        .with_id("switch_up_next_autoplay")
        .with_accessible_name(&t("tube-autoplay"))
        .on_change({
            let bus = ui.bus.clone();
            move |_| {
                action::push(
                    &bus,
                    Action::Effect(TubeEffect::SetPreference {
                        preference: SettingFlag::Autoplay.id().into(),
                        enabled,
                    }),
                );
            }
        });
    let header = Row::new()
        .no_wrap()
        .with_gap(rhythm.related)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(text(&t("tube-up-next"), TextRole::Heading))),
        ))
        .push(Box::new(
            text(&t("tube-autoplay"), TextRole::Label).dimmed(),
        ))
        .push(Box::new(autoplay));
    let related = ui.model.related_display(id);
    let mut rows = stack().with_gap(rhythm.sibling);
    for v in related.into_iter().take(6) {
        rows = rows.push(Box::new(up_next_row(ui, v)));
    }
    stack()
        .with_gap(rhythm.section)
        .push(Box::new(header))
        .push(Box::new(rows))
}

/// One compact "Up next" row: the thumbnail with its duration, and beside it
/// the title, the creator and the reach. The whole row opens the video.
fn up_next_row(ui: Ui<'_>, v: &Video) -> Container {
    let rhythm = &ui.theme.tokens.spacing.rhythm;
    let dearrow = ui.model.settings.dearrow_titles_on();
    let side = f64::from(ui.theme.tokens.metrics.inspector_width);
    let thumb_w = (side * UP_NEXT_THUMB_SHARE).round() as f32;
    let mut thumb = MediaCard::new(
        format!("up_next_thumb_{}", v.id),
        art(ui.imgs, ui.model.display_thumbnail_url(v)),
    )
    .aspect(MediaAspect::Wide)
    .seed(v.display_title(dearrow));
    thumb = if v.live {
        thumb.badge(OwnedBadge::Live)
    } else if v.duration > 0 {
        thumb.badge(OwnedBadge::Duration(clock(f64::from(v.duration))))
    } else {
        thumb
    };
    let mut about = stack()
        .with_gap(rhythm.tight)
        .push(Box::new(text(v.display_title(dearrow), TextRole::Label)))
        .push(Box::new(text(&v.channel_name, TextRole::Caption).dimmed()));
    let reach = if v.live {
        t_args("tube-watching", &[arg("shown", human_count(v.views))])
    } else {
        views_and_age(v.views, &v.published)
    };
    if !reach.is_empty() {
        about = about.push(Box::new(text(&reach, TextRole::Caption).dimmed()));
    }
    Container::new()
        .with_no_padding()
        .interactive(
            format!("up_next_{}", v.id),
            ui.push(Action::OpenVideo(v.id.clone())),
        )
        .with_child(Box::new(
            Row::new()
                .no_wrap()
                .align_start()
                .with_gap(rhythm.sibling)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_width(thumb_w)
                        .with_child(Box::new(thumb)),
                ))
                .push(Box::new(grow(about))),
        ))
}

/// The share of the "Up next" column a row's thumbnail takes.
const UP_NEXT_THUMB_SHARE: f64 = 0.45;

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

/// A settings row whose switch sits at the trailing edge. A section of
/// switches holds only switches, so every switch of the page shares that
/// edge with the choosers of the other sections.
fn toggle_field(label: &str, toggle: Toggle) -> Field {
    Field::new(label, toggle).with_trailing_control()
}

/// A switch that reports a flip of `flag` on the bus.
fn switch(ui: Ui<'_>, id: &str, on: bool, flag: SettingFlag) -> Toggle {
    let bus = ui.bus.clone();
    Toggle::from_state(&ui::LocalState::new(on))
        .with_id(id)
        .on_change(move |_| {
            action::push(
                &bus,
                Action::Effect(TubeEffect::SetPreference {
                    preference: flag.id().into(),
                    enabled: !on,
                }),
            );
        })
}

/// A sentence under a group of settings that the person needs to read the
/// values right, a related step below the last row.
fn section_note(ui: Ui<'_>, note: &str) -> Element {
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_horizontal_padding(0.0, 0.0)
            .with_vertical_padding(ui.theme.tokens.spacing.rhythm.related)
            .push(Box::new(text(note, TextRole::Caption).dimmed())),
    )
}

/// The width a chooser takes in a settings row: the comfortable control
/// measure a form gives a fluid control (sixteen body ems, as `Field`
/// reserves it), so the choosers of every section share both edges. The
/// theme has no token for it yet.
fn chooser_width(theme: &Theme) -> f32 {
    theme.tokens.typography.size(TextRole::Body) * 16.0
}

pub fn settings(ui: Ui<'_>) -> Scrollable {
    settings_impl(ui, None, None)
}

/// Settings with the process-local Grayjay device surface. The pairing input
/// is explicitly external memory, so invite codes never enter Automerge or
/// collaboration history.
pub fn settings_with_sync(
    ui: Ui<'_>,
    sync: &crate::sync::SyncView,
    pairing_input: Arc<Mutex<String>>,
) -> Scrollable {
    settings_impl(ui, Some(sync), Some(pairing_input))
}

fn settings_impl(
    ui: Ui<'_>,
    sync: Option<&crate::sync::SyncView>,
    pairing_input: Option<Arc<Mutex<String>>>,
) -> Scrollable {
    let form = Form::new()
        .section(playback_settings(ui))
        .section(quality_settings(ui))
        .section(sponsorblock_settings(ui))
        .section(segment_settings(ui))
        .section(dearrow_settings(ui))
        .section(device_sync_settings(ui, sync, pairing_input));
    // Forms keep to the readable width on the leading edge.
    let column = Column::new().with_intrinsic_height().push(Box::new(
        Container::new()
            .with_no_padding()
            .readable()
            .with_child(Box::new(form)),
    ));
    Scrollable::new_vertical().with_child(Box::new(column))
}

fn playback_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    FormSection::new(t("tube-playback"))
        .grouped()
        .field(toggle_field(
            &t("tube-autoplay-next"),
            switch(ui, "switch_autoplay", s.autoplay, SettingFlag::Autoplay),
        ))
        .field(toggle_field(
            &t("tube-show-comments"),
            switch(
                ui,
                "switch_comments",
                s.show_comments,
                SettingFlag::ShowComments,
            ),
        ))
        .field(toggle_field(
            &t("tube-show-dislikes"),
            switch(
                ui,
                "switch_dislikes",
                s.return_dislike_enabled,
                SettingFlag::ReturnDislike,
            ),
        ))
        .content(section_note(ui, &t("tube-dislikes-note")))
}

fn quality_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    let bus = ui.bus.clone();
    let quality = Dropdown::from_state(
        crate::app::QUALITIES
            .iter()
            .map(|q| quality_label(q))
            .collect(),
        &ui::LocalState::new(quality_label(&s.preferred_quality)),
    )
    .with_id("preferred_quality")
    .on_change(move |label| {
        if let Some(q) = crate::app::QUALITIES
            .iter()
            .find(|q| quality_label(q) == label)
        {
            action::push(
                &bus,
                Action::Effect(TubeEffect::SetPreferredQuality {
                    quality: (*q).to_string(),
                }),
            );
        }
    });
    FormSection::new(t("tube-quality"))
        .grouped()
        .field(Field::new(t("tube-preferred-quality"), quality).with_trailing_control())
}

/// "1080p" → "1080p"; "auto" → "Automatic".
fn quality_label(q: &str) -> String {
    if q == "auto" {
        t("tube-quality-automatic")
    } else {
        q.to_string()
    }
}

fn sponsorblock_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    FormSection::new(t("tube-sponsorblock"))
        .grouped()
        .field(toggle_field(
            &t("tube-skip-sponsored"),
            switch(
                ui,
                "switch_sponsorblock",
                s.sponsorblock_enabled,
                SettingFlag::SponsorBlock,
            ),
        ))
        .content(section_note(ui, &t("tube-sponsorblock-note")))
}

/// One row per kind of segment: its timeline colour and name, what it
/// covers, and what Tube does with it.
fn segment_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    let theme = ui.theme;
    let rhythm = &theme.tokens.spacing.rhythm;
    let mut section = FormSection::new(t("tube-segments"));
    for category in Category::ALL {
        let stored = s
            .categories
            .get(category.api_key())
            .copied()
            .unwrap_or(SkipMode::Skip);
        let bus = ui.bus.clone();
        let modes = [SkipMode::Skip, SkipMode::Show, SkipMode::Disabled];
        let dropdown = Dropdown::from_state(
            modes.iter().map(|m| m.label().to_string()).collect(),
            &ui::LocalState::new(stored.label().to_string()),
        )
        .with_id(&format!("mode_{}", category.api_key()))
        .with_accessible_name(category.label())
        .with_enabled(s.sponsorblock_enabled)
        .on_change(move |label| {
            if let Some(mode) = modes.iter().find(|m| m.label() == label) {
                action::push(
                    &bus,
                    Action::Effect(TubeEffect::SetSegmentHandling {
                        category: category.api_key().into(),
                        mode: mode.key().into(),
                    }),
                );
            }
        });
        let name =
            Chip::new(category.label()).with_lead(ChipLead::Dot(category_colour(theme, category)));
        let about = stack()
            .with_gap(rhythm.tight)
            .push(Box::new(Row::new().no_wrap().push(Box::new(name))))
            .push(Box::new(text(category.blurb(), TextRole::Caption).dimmed()));
        section = section.content(Box::new(
            Row::new()
                .no_wrap()
                .with_gap(rhythm.group)
                .push(Box::new(grow(about)))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_width(chooser_width(theme))
                        .with_child(Box::new(dropdown)),
                )),
        ));
    }
    section
}

fn dearrow_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    FormSection::new(t("tube-dearrow"))
        .grouped()
        .field(toggle_field(
            &t("tube-dearrow-community"),
            switch(
                ui,
                "switch_dearrow",
                s.dearrow_enabled,
                SettingFlag::DeArrow,
            ),
        ))
        .field(toggle_field(
            &t("tube-dearrow-titles"),
            switch(
                ui,
                "switch_dearrow_titles",
                s.dearrow_titles,
                SettingFlag::DeArrowTitles,
            )
            .with_enabled(s.dearrow_enabled),
        ))
        .field(toggle_field(
            &t("tube-dearrow-thumbnails"),
            switch(
                ui,
                "switch_dearrow_thumbs",
                s.dearrow_thumbnails,
                SettingFlag::DeArrowThumbnails,
            )
            .with_enabled(s.dearrow_enabled),
        ))
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear build of the device section: status, this device, pairing, \
              pending and approved devices"
)]
fn device_sync_settings(
    ui: Ui<'_>,
    sync: Option<&crate::sync::SyncView>,
    pairing_input: Option<Arc<Mutex<String>>>,
) -> FormSection {
    let mut section = FormSection::new(t("tube-device-sync"));
    let capability = crate::sync::capability();
    let Some((state, input)) = sync.zip(pairing_input) else {
        let message = if capability.available {
            t("tube-sync-unavailable-now")
        } else {
            t("tube-sync-not-built")
        };
        return section.content(Box::new(text(&message, TextRole::Body).dimmed()));
    };
    let Some(status) = sync_status(state.status) else {
        return section.content(Box::new(
            Banner::warning(t("tube-sync-locked-title"))
                .with_message(t("tube-sync-locked-message")),
        ));
    };
    let rhythm = &ui.theme.tokens.spacing.rhythm;
    let active: Vec<&crate::sync::PeerRecord> =
        state.peers.iter().filter(|peer| peer.is_active()).collect();

    // Where sync stands, and Sync now once there is a device to sync with.
    let mut summary = Row::new()
        .no_wrap()
        .with_gap(rhythm.related)
        .push(Box::new(grow(
            Row::new()
                .with_gap(rhythm.related)
                .push(Box::new(text(&state.summary, TextRole::Body).dimmed()))
                .push(Box::new(
                    Badge::text(status.0).style(ChipStyle::Tinted(status.1)),
                )),
        )));
    if !active.is_empty() {
        summary = summary.push(Box::new(
            Button::new("grayjay_sync_now", &t("tube-sync-now"))
                .with_icon(Icon::Refresh)
                .on_click(ui.push(Action::Effect(TubeEffect::SyncDevices))),
        ));
    }
    section = section.content(Box::new(summary));

    section = section.content(Box::new(
        stack()
            .with_gap(rhythm.related)
            .push(Box::new(text(&t("tube-pair-title"), TextRole::Subheading)))
            .push(Box::new(text(&t("tube-pair-how"), TextRole::Body).dimmed()))
            .push(Box::new(
                TextInput::local()
                    .with_id("grayjay_pairing_invite")
                    .with_external_buffer(input)
                    .with_placeholder(&t("tube-pair-placeholder"))
                    .with_accessible_name(t("tube-pair-name"))
                    .with_enter_submits(true),
            ))
            .push(Box::new(
                Button::new("grayjay_pair", &t("tube-pair-device"))
                    .on_click(ui.push(Action::PairDevice)),
            )),
    ));

    // A device that paired from its side waits until the person compares
    // its fingerprint with the one Grayjay shows and approves it.
    let pending: Vec<&crate::sync::PeerRecord> = state
        .peers
        .iter()
        .filter(|peer| peer.pending_approval && peer.revoked_at.is_none())
        .collect();
    if !pending.is_empty() {
        let mut items: Vec<Element> = Vec::new();
        for peer in pending {
            items.push(Box::new(
                ListRow::new(format!("grayjay_pending_{}", peer.short_id()), &peer.name)
                    .icon(Icon::Shield)
                    .secondary(t_args(
                        "tube-pending-check",
                        &[arg("fingerprint", peer.id.as_str())],
                    )),
            ));
            items.push(Box::new(
                Row::new().no_wrap().push(Box::new(
                    Button::new(
                        &format!("grayjay_approve_{}", peer.short_id()),
                        &t("tube-approve-device"),
                    )
                    .role(ButtonRole::Primary)
                    .on_click(ui.push(Action::Effect(
                        TubeEffect::ApproveDevice {
                            device: peer.id.clone(),
                        },
                    ))),
                )),
            ));
        }
        section = section.content(Box::new(
            Section::new(items).with_header(SectionHeader::group(t("tube-pending-title"))),
        ));
    }

    let header = SectionHeader::group(t("tube-approved-devices"));
    if active.is_empty() {
        section = section.content(Box::new(
            Section::new(vec![Box::new(
                text(&t("tube-no-approved-devices"), TextRole::Body).dimmed(),
            )])
            .with_header(header),
        ));
    } else {
        let mut rows: Vec<Element> = Vec::new();
        for peer in active {
            let (detail, failed) = peer.last_error.as_ref().map_or_else(
                || {
                    (
                        peer.last_sync_at
                            .map_or_else(|| t("tube-not-synced"), relative_sync_time),
                        false,
                    )
                },
                |error| {
                    (
                        t_args("tube-sync-failed", &[arg("error", error.as_str())]),
                        true,
                    )
                },
            );
            let bus = ui.bus.clone();
            let id = peer.id.clone();
            let mut row = ListRow::new(format!("grayjay_peer_{}", peer.short_id()), &peer.name)
                .icon(Icon::Monitor)
                .secondary(detail)
                .action(Icon::Trash, t("tube-revoke-device"), move || {
                    action::push(
                        &bus,
                        Action::Effect(TubeEffect::RevokeDevice { device: id.clone() }),
                    );
                });
            if failed {
                row = row.error();
            }
            rows.push(Box::new(row));
        }
        section = section.content(Box::new(Section::new(rows).with_header(header)));
    }
    section
}

/// The badge for a working device-sync status; `None` while it is unavailable.
fn sync_status(status: crate::sync::SyncStatus) -> Option<(String, Tone)> {
    match status {
        crate::sync::SyncStatus::Unavailable => None,
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Ready => Some((t("tube-sync-ready"), Tone::Success)),
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Pairing => Some((t("tube-sync-pairing"), Tone::Info)),
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Syncing => Some((t("tube-sync-syncing"), Tone::Info)),
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Error => Some((t("tube-sync-attention"), Tone::Warning)),
    }
}

fn relative_sync_time(time: i64) -> String {
    let current = ui::time::unix_now_i64();
    t_args(
        "tube-last-synced",
        &[arg("when", ui::format::relative(current - time))],
    )
}

/// Backend results displayed as the same semantic media cards used by the catalogue.
pub fn search_results(ui: Ui<'_>, query: &str) -> Scrollable {
    let filters = SearchType::ALL.into_iter().fold(Row::new(), |row, kind| {
        row.push(Box::new(
            Chip::new(kind.label())
                .selected(ui.model.search_type_filter.shows(kind))
                .on_click(ui.push(Action::ToggleSearchType(kind))),
        ))
    });
    let mut children: Vec<Element> = vec![Box::new(filters)];
    if ui.model.search_loading {
        children.push(Box::new(skeleton_grid(ui)));
    } else if let Some(error) = &ui.model.search_error {
        children.push(Box::new(EmptyState::new(error).with_icon(Icon::Search)));
    } else {
        if ui.model.search_type_filter.shows(SearchType::Video) {
            children.push(video_grid(ui, ui.model.search_display(query)));
        }
        if ui.model.search_type_filter.shows(SearchType::Playlist) {
            let rows = ui
                .model
                .search_playlists_display(query)
                .into_iter()
                .map(|playlist| {
                    Box::new(
                        ListRow::new(format!("tube.playlist:{}", playlist.id), &playlist.title)
                            .icon(Icon::ListVideo)
                            .secondary(&playlist.channel_name)
                            .on_activate(ui.push(Action::Effect(TubeEffect::OpenPlaylist {
                                playlist: playlist.id.clone(),
                            }))),
                    ) as Element
                })
                .collect();
            children.push(Box::new(
                Section::new(rows).with_header(SectionHeader::group(SearchType::Playlist.label())),
            ));
        }
        if ui.model.search_type_filter.shows(SearchType::Creator) {
            let rows = ui
                .model
                .search_creators_display(query)
                .into_iter()
                .map(|creator| {
                    Box::new(
                        ListRow::new(format!("tube.channel:{}", creator.id), &creator.name)
                            .icon(Icon::Users)
                            .secondary(&creator.handle)
                            .on_activate(ui.push(Action::Effect(TubeEffect::OpenCreator {
                                creator: creator.id.clone(),
                            }))),
                    ) as Element
                })
                .collect();
            children.push(Box::new(
                Section::new(rows).with_header(SectionHeader::group(SearchType::Creator.label())),
            ));
        }
    }
    page(vec![Box::new(Section::new(children).with_header(
        SectionHeader::heading(format!("Search: {query}")),
    ))])
}
