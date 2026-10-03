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
use ui::time::UNIX_EPOCH;
use ui::widget::toolbar::Orientation;
use ui::widget::{
    Avatar, AvatarSize, Badge, Banner, Button, ButtonRole, ChipStyle, Column, Container,
    ControlSize, Dropdown, Element, EmptyState, Field, Form, FormSection, ListRow, MediaAspect,
    MediaCard, MediaGrid, MediaSize, MediaSource, OwnedBadge, Row, Scrollable, Section,
    SectionHeader, Skeleton, SkeletonShape, Text, TextInput, Toggle, Tone, Toolbar,
};

use crate::action::{self, Action, ActionBus, Invocations, SettingFlag};
use crate::images::Imgs;
use crate::model::{Category, Route, SkipMode, TubeModel, Video};
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
fn page(theme: &Theme, children: Vec<Element>) -> Scrollable {
    let mut col = Column::new()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.rhythm.group);
    for child in children {
        col = col.push(child);
    }
    Scrollable::new_vertical().with_child(Box::new(col))
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
        (true, false) => format!("{}, {published}", human_views(views)),
        (true, true) => human_views(views),
        (false, false) => published.to_string(),
        (false, true) => String::new(),
    }
}

/// "12K subscribers", or `None` when the count is unknown.
fn subscribers(count: u64) -> Option<String> {
    (count > 0).then(|| format!("{} subscribers", human_count(count)))
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
    .meta(&v.channel_name);
    let reach = if v.live {
        format!("{} watching", human_count(v.views))
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

fn video_grid<'v>(ui: Ui<'_>, videos: impl IntoIterator<Item = &'v Video>) -> MediaGrid {
    MediaGrid::new(videos.into_iter().map(|v| video_card(ui, v)).collect())
}

/// A compact grid for a side column ("Up next").
fn compact_grid<'v>(ui: Ui<'_>, videos: impl IntoIterator<Item = &'v Video>) -> MediaGrid {
    video_grid(ui, videos).with_size(MediaSize::Small)
}

/// The loading state of a video grid: card-shaped placeholders that the
/// arriving cards replace without moving anything.
fn skeleton_grid(theme: &Theme, count: usize) -> Row {
    let width = MediaSize::Regular.extent(theme) as f32;
    let mut row = Row::new()
        .align_start()
        .with_gap(theme.tokens.spacing.rhythm.section);
    for _ in 0..count {
        row = row.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_width(width)
                .with_child(Box::new(Skeleton::new(SkeletonShape::Media(
                    MediaAspect::Wide,
                )))),
        ));
    }
    row
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
pub fn title(route: &Route) -> &'static str {
    match route {
        Route::Home => "Home",
        Route::Trending => "Trending",
        Route::Subscriptions => "Channels",
        Route::Queue => "Queue",
        Route::History => "History",
        Route::Settings => "Settings",
        Route::Watch(_) | Route::Channel(_) | Route::Playlist(_) => "",
    }
}

/// The content column's header actions: the data-source status while the
/// catalogue is not live, the page's own action, and the search launcher.
pub fn header_actions(ui: Ui<'_>) -> Vec<Element> {
    let mut actions: Vec<Element> = Vec::new();
    if let Some(status) = data_source_badge(ui.model) {
        actions.push(Box::new(status));
    }
    if matches!(ui.model.route, Route::History) && !ui.model.history.is_empty() {
        actions.push(Box::new(
            Button::new("clear_history", "Clear history", None)
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .on_click(ui.push(Action::ClearHistory)),
        ));
    }
    actions.push(Box::new(ui::search::search_button(
        SearchRequest::this_app(),
    )));
    actions
}

/// Where the catalogue comes from, shown only when it is not live data.
fn data_source_badge(model: &TubeModel) -> Option<Badge> {
    if model.data_source.live {
        return None;
    }
    let (label, tone) = if model.data_source.label.contains("connect") {
        ("Connecting", Tone::Info)
    } else if model.data_source.label.contains("offline") {
        ("Offline", Tone::Neutral)
    } else {
        return None;
    };
    Some(Badge::text(label).style(ChipStyle::Tinted(tone)))
}

// ---------------------------------------------------------------------------
// browse screens
// ---------------------------------------------------------------------------

pub fn home(ui: Ui<'_>) -> Scrollable {
    let model = ui.model;
    let body: Element = if !model.videos.is_empty() {
        Box::new(video_grid(ui, &model.videos))
    } else if model.feed_loading {
        Box::new(skeleton_grid(ui.theme, 8))
    } else if let Some(error) = &model.feed_error {
        Box::new(EmptyState::error("Home couldn't refresh", error).with_icon(Icon::WifiOff))
    } else if model.data_source.label.contains("offline") {
        Box::new(
            EmptyState::new("You're offline")
                .with_icon(Icon::WifiOff)
                .with_message("Reconnect to load videos. Your queue and history are still here."),
        )
    } else {
        Box::new(
            EmptyState::new("Nothing to show yet")
                .with_icon(Icon::Play)
                .with_message("Search for a video to start watching.")
                .with_action("Search")
                .on_action(search_action()),
        )
    };
    page(ui.theme, vec![body])
}

pub fn trending(ui: Ui<'_>) -> Scrollable {
    let mut videos: Vec<&Video> = ui.model.videos.iter().collect();
    videos.sort_by_key(|v| std::cmp::Reverse(v.views));
    let body: Element = if videos.is_empty() {
        Box::new(
            EmptyState::new("Nothing trending yet")
                .with_icon(Icon::Flame)
                .with_message("Popular videos show here once the catalogue loads."),
        )
    } else {
        Box::new(video_grid(ui, videos))
    };
    page(ui.theme, vec![body])
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
                        .with_gap(ui.theme.tokens.spacing.rhythm.related)
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
            EmptyState::new("No subscriptions yet")
                .with_icon(Icon::Bell)
                .with_message("Subscribe to creators and their new videos show here."),
        )
    } else {
        Box::new(
            Section::new(vec![Box::new(video_grid(ui, feed))])
                .with_header(SectionHeader::heading("Latest videos")),
        )
    };
    let mut children: Vec<Element> = Vec::new();
    if any {
        children.push(Box::new(creators));
    }
    children.push(latest);
    page(ui.theme, children)
}

pub fn history(ui: Ui<'_>) -> Scrollable {
    let model = ui.model;
    let mut children: Vec<Element> = Vec::new();
    if let Some(recovery) = &model.history_recovery_notice {
        children.push(Box::new(
            Banner::warning("Watch history isn't saving").with_message(recovery.clone()),
        ));
    }
    let videos = model.history_videos();
    if videos.is_empty() {
        children.push(Box::new(
            EmptyState::new("Nothing watched yet")
                .with_icon(Icon::History)
                .with_message("Videos you watch show up here."),
        ));
    } else {
        children.push(Box::new(video_grid(ui, videos)));
    }
    page(ui.theme, children)
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
            EmptyState::new("Your queue is empty")
                .with_icon(Icon::ListVideo)
                .with_message("Add videos from their watch page."),
        ));
    } else {
        children.push(Box::new(video_grid(ui, videos)));
    }
    if waiting > 0 {
        let noun = if waiting == 1 { "video" } else { "videos" };
        children.push(Box::new(
            text(
                &format!("{waiting} more queued {noun} will show once their details load."),
                TextRole::Caption,
            )
            .dimmed(),
        ));
    }
    page(ui.theme, children)
}

pub fn playlist(ui: Ui<'_>, id: &str) -> Scrollable {
    let model = ui.model;
    let Some(pl) = model.playlist(id) else {
        return page(
            ui.theme,
            vec![Box::new(
                EmptyState::new("This playlist isn't available").with_icon(Icon::ListVideo),
            )],
        );
    };
    let videos: Vec<&Video> = pl
        .video_ids
        .iter()
        .filter_map(|vid| model.video(vid))
        .collect();

    let mut info = stack()
        .with_gap(ui.theme.tokens.spacing.rhythm.related)
        .push(Box::new(text(&pl.title, TextRole::Title)))
        .push(Box::new(text(&pl.channel_name, TextRole::Body).dimmed()))
        .push(Box::new(
            text(
                &format!("{} videos", human_count(pl.video_count)),
                TextRole::Caption,
            )
            .dimmed(),
        ));
    if let Some(first) = videos.first() {
        info = info.push(Box::new(
            Button::new("playlist_play", "Play all", None)
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
        .with_gap(ui.theme.tokens.spacing.rhythm.group);
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
        Box::new(skeleton_grid(ui.theme, 4))
    } else {
        Box::new(Section::titled(
            "Videos",
            vec![Box::new(video_grid(ui, videos))],
        ))
    };
    page(ui.theme, vec![Box::new(header), list])
}

pub fn channel(ui: Ui<'_>, id: &str) -> Scrollable {
    let model = ui.model;
    let Some(ch) = model.channel(id) else {
        return page(
            ui.theme,
            vec![Box::new(
                EmptyState::new("This creator isn't available").with_icon(Icon::Users),
            )],
        );
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
        .with_gap(ui.theme.tokens.spacing.rhythm.related)
        .push(Box::new(text(&ch.name, TextRole::Title)));
    if ch.verified {
        name = name.push(Box::new(ui::widget::IconWidget::new(
            Icon::Check,
            ui.theme.tokens.metrics.icon_md,
        )));
    }
    let mut about = stack()
        .with_gap(ui.theme.tokens.spacing.rhythm.tight)
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
        .with_gap(ui.theme.tokens.spacing.rhythm.section)
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
        children.push(Box::new(skeleton_grid(ui.theme, 4)));
    } else {
        children.push(Box::new(Section::titled(
            "Videos",
            vec![Box::new(video_grid(ui, videos))],
        )));
    }
    page(ui.theme, children)
}

fn subscribe_button(ui: Ui<'_>, channel_id: &str) -> Button {
    let action = Action::ToggleSubscribe(channel_id.to_string());
    if ui.model.is_subscribed(channel_id) {
        Button::new("subscribe_btn", "Subscribed", None)
            .with_icon(Icon::Bell)
            .on_click(ui.push(action))
    } else {
        Button::new("subscribe_btn", "Subscribe", None)
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
        return page(
            theme,
            vec![Box::new(
                EmptyState::new("This video isn't available").with_icon(Icon::Film),
            )],
        );
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
                .with_child(Box::new(PlayerHost::new(player, player_h))),
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
                .with_gap(theme.tokens.spacing.rhythm.group)
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
                .with_gap(theme.tokens.spacing.rhythm.group)
                .push(Box::new(details))
                .push(Box::new(up_next)),
        )
    };

    // The player sits flush under the header row; the details below it
    // stand in the page gutter.
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
        "Resolution",
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
            format!("Skip {}", segment.category.label().to_lowercase()),
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

    let mut facts = Row::new().with_gap(theme.tokens.spacing.s4);
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
    let likes = model.display_likes(v);
    if likes > 0 {
        fact(format!("{} likes", human_count(likes)));
    }
    if model.settings.return_dislike_enabled
        && let Some(dislikes) = model.dislikes_of(&v.id)
    {
        fact(format!("{} dislikes", human_count(dislikes)));
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
        details = details.push(Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(text(&v.description, TextRole::Body))),
        ));
    }
    details.push(Box::new(comments(ui, v)))
}

/// Like, queue, share and playback settings, as one command strip.
fn watch_actions(ui: Ui<'_>, v: &Video) -> Toolbar {
    let model = ui.model;
    let liked = model.is_liked(&v.id);
    let queued = model.is_queued(&v.id);
    let invocations = ui.invocations.clone();
    let queue = crate::actions::queue_binding(model, &v.id);
    let mut toolbar = Toolbar::new(Orientation::Horizontal)
        .push_toggle(
            if liked { "Liked" } else { "Like" },
            liked,
            ui.push(Action::ToggleLike(v.id.clone())),
        )
        .push_toggle(
            if queued { "In queue" } else { "Add to queue" },
            queued,
            move || action::invoke(&invocations, queue.clone()),
        )
        .push_button("Share", ui.push(Action::Share(v.id.clone())));
    toolbar = toolbar.push_spacer().push_icon(
        Icon::Settings,
        "Playback settings",
        ui.push(Action::Navigate(Route::Settings)),
    );
    toolbar
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
    let creator = Container::new()
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
    let header = SectionHeader::heading("Comments").count(v.comments.len());
    if !ui.model.settings.show_comments {
        return Section::new(vec![Box::new(
            text("Comments are turned off in Settings.", TextRole::Body).dimmed(),
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
        head = head.push(Box::new(Badge::text("Pinned").style(ChipStyle::Neutral)));
    }
    let mut reactions = Row::new().with_gap(ui.theme.tokens.spacing.s4);
    if c.likes > 0 {
        reactions = reactions.push(Box::new(
            text(
                &format!("{} likes", human_count(c.likes)),
                TextRole::Caption,
            )
            .numeric()
            .dimmed(),
        ));
    }
    if c.replies > 0 {
        let noun = if c.replies == 1 { "reply" } else { "replies" };
        reactions = reactions.push(Box::new(
            text(&format!("{} {noun}", c.replies), TextRole::Caption)
                .numeric()
                .dimmed(),
        ));
    }
    if c.hearted {
        reactions = reactions.push(Box::new(
            text("Liked by the creator", TextRole::Caption).dimmed(),
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
    let autoplay = Toggle::from_state(&ui::LocalState::new(ui.model.settings.autoplay))
        .with_id("switch_up_next_autoplay")
        .with_accessible_name("Autoplay")
        .on_change({
            let bus = ui.bus.clone();
            move |_| action::push(&bus, Action::ToggleSetting(SettingFlag::Autoplay))
        });
    let header = Row::new()
        .no_wrap()
        .with_gap(rhythm.related)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(text("Up next", TextRole::Heading))),
        ))
        .push(Box::new(text("Autoplay", TextRole::Label).dimmed()))
        .push(Box::new(autoplay));
    let related = ui.model.related_display(id);
    let mut col = stack().with_gap(rhythm.related).push(Box::new(header));
    if !related.is_empty() {
        col = col.push(Box::new(compact_grid(ui, related.into_iter().take(6))));
    }
    col
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

/// A settings row whose switch sits at the trailing edge.
fn toggle_field(label: &str, toggle: Toggle) -> Field {
    Field::new(label, toggle).with_trailing_control()
}

/// A switch that reports a flip of `flag` on the bus.
fn switch(ui: Ui<'_>, id: &str, on: bool, flag: SettingFlag) -> Toggle {
    let bus = ui.bus.clone();
    Toggle::from_state(&ui::LocalState::new(on))
        .with_id(id)
        .on_change(move |_| action::push(&bus, Action::ToggleSetting(flag)))
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
        .section(sponsorblock_settings(ui))
        .section(dearrow_settings(ui))
        .section(device_sync_settings(ui, sync, pairing_input));
    // Forms are bounded to a readable measure: the content minimum plus one
    // detail column.
    let metrics = &ui.theme.tokens.metrics;
    let column = Column::new().with_intrinsic_height().push(Box::new(
        Container::new()
            .with_no_padding()
            .with_centered_max_width(metrics.content_min_width + metrics.inspector_width)
            .with_child(Box::new(form)),
    ));
    Scrollable::new_vertical().with_child(Box::new(column))
}

fn playback_settings(ui: Ui<'_>) -> FormSection {
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
            action::push(&bus, Action::SetQuality((*q).to_string()));
        }
    });
    FormSection::new("Playback")
        .grouped()
        .field(toggle_field(
            "Autoplay next video",
            switch(ui, "switch_autoplay", s.autoplay, SettingFlag::Autoplay),
        ))
        .field(toggle_field(
            "Show comments",
            switch(
                ui,
                "switch_comments",
                s.show_comments,
                SettingFlag::ShowComments,
            ),
        ))
        .field(toggle_field(
            "Show dislike counts",
            switch(
                ui,
                "switch_dislikes",
                s.return_dislike_enabled,
                SettingFlag::ReturnDislike,
            ),
        ))
        .field(Field::new("Preferred quality", quality).with_trailing_control())
}

/// "1080p" → "1080p"; "auto" → "Automatic".
fn quality_label(q: &str) -> String {
    if q == "auto" {
        "Automatic".to_string()
    } else {
        q.to_string()
    }
}

fn sponsorblock_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    let mut section = FormSection::new("SponsorBlock")
        .grouped()
        .field(toggle_field(
            "Skip sponsored segments",
            switch(
                ui,
                "switch_sponsorblock",
                s.sponsorblock_enabled,
                SettingFlag::SponsorBlock,
            ),
        ));
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
        .with_enabled(s.sponsorblock_enabled)
        .on_change(move |label| {
            if let Some(mode) = modes.iter().find(|m| m.label() == label) {
                action::push(&bus, Action::SetCategory(category, *mode));
            }
        });
        section = section.field(Field::new(category.label(), dropdown).with_trailing_control());
    }
    section
}

fn dearrow_settings(ui: Ui<'_>) -> FormSection {
    let s = &ui.model.settings;
    FormSection::new("DeArrow")
        .grouped()
        .field(toggle_field(
            "Use community titles and thumbnails",
            switch(
                ui,
                "switch_dearrow",
                s.dearrow_enabled,
                SettingFlag::DeArrow,
            ),
        ))
        .field(toggle_field(
            "Replace titles",
            switch(
                ui,
                "switch_dearrow_titles",
                s.dearrow_titles,
                SettingFlag::DeArrowTitles,
            )
            .with_enabled(s.dearrow_enabled),
        ))
        .field(toggle_field(
            "Replace thumbnails",
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
    let mut section = FormSection::new("Device sync");
    let capability = crate::sync::capability();
    let Some((state, input)) = sync.zip(pairing_input) else {
        let message = if capability.available {
            "Device sync isn't available right now."
        } else {
            "This build of Tube doesn't include Grayjay device sync."
        };
        return section.content(Box::new(text(message, TextRole::Body).dimmed()));
    };
    let Some(status) = sync_status(state.status) else {
        return section.content(Box::new(
            Banner::warning("Device sync is unavailable").with_message(
                "Unlock the desktop's secret store, then reopen Tube to pair Grayjay devices.",
            ),
        ));
    };
    let rhythm = &ui.theme.tokens.spacing.rhythm;
    section = section.content(Box::new(
        Row::new()
            .with_gap(rhythm.related)
            .push(Box::new(text(&state.summary, TextRole::Body).dimmed()))
            .push(Box::new(
                Badge::text(status.0).style(ChipStyle::Tinted(status.1)),
            )),
    ));
    if let Some(public_key) = &state.device_public_key {
        let fingerprint: String = public_key.chars().take(18).collect();
        section = section.content(Box::new(
            ListRow::new("this_device", "This device")
                .icon(Icon::Shield)
                .secondary(format!(
                    "Key {fingerprint}, held by the desktop secret store"
                )),
        ));
    }
    section = section.content(Box::new(
        stack()
            .with_gap(rhythm.related)
            .push(Box::new(text("Pair a device", TextRole::Subheading)))
            .push(Box::new(
                text(
                    "In Grayjay, open Sync and choose Add device, then paste its invite here.",
                    TextRole::Body,
                )
                .dimmed(),
            ))
            .push(Box::new(
                TextInput::local()
                    .with_id("grayjay_pairing_invite")
                    .with_external_buffer(input)
                    .with_placeholder("Pairing invite from Grayjay")
                    .with_accessible_name("Grayjay pairing invite")
                    .with_enter_submits(true),
            ))
            .push(Box::new(
                Button::new("grayjay_pair", "Pair device", None)
                    .on_click(ui.push(Action::PairDevice)),
            )),
    ));

    for peer in state
        .peers
        .iter()
        .filter(|peer| peer.pending_approval && peer.revoked_at.is_none())
    {
        section = section.content(Box::new(
            ListRow::new(
                format!("grayjay_pending_{}", peer.short_id()),
                "Confirm this device",
            )
            .icon(Icon::Shield)
            .secondary(format!(
                "Check that Grayjay shows fingerprint {}",
                peer.short_id()
            ))
            .warning(),
        ));
        section = section.content(Box::new(
            Button::new(
                &format!("grayjay_approve_{}", peer.short_id()),
                "Approve device",
                None,
            )
            .role(ButtonRole::Primary)
            .on_click(ui.push(Action::ApproveDevice(peer.id.clone()))),
        ));
    }

    let active: Vec<&crate::sync::PeerRecord> =
        state.peers.iter().filter(|peer| peer.is_active()).collect();
    let header = SectionHeader::group("Approved devices");
    if active.is_empty() {
        section = section.content(Box::new(
            Section::new(vec![Box::new(
                text(
                    "No approved devices yet. Pairing needs a confirmation on both devices.",
                    TextRole::Body,
                )
                .dimmed(),
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
                            .map_or_else(|| "Not synced yet".into(), relative_sync_time),
                        false,
                    )
                },
                |error| (format!("Couldn't sync: {error}"), true),
            );
            let bus = ui.bus.clone();
            let id = peer.id.clone();
            let mut row = ListRow::new(format!("grayjay_peer_{}", peer.short_id()), &peer.name)
                .icon(Icon::Monitor)
                .secondary(detail)
                .action(Icon::Trash, "Revoke device", move || {
                    action::push(&bus, Action::RevokeDevice(id.clone()));
                });
            if failed {
                row = row.error();
            }
            rows.push(Box::new(row));
        }
        section = section.content(Box::new(
            Section::new(rows).with_header(
                header.action(
                    Button::new("grayjay_sync_now", "Sync now", None)
                        .on_click(ui.push(Action::SyncDevices)),
                ),
            ),
        ));
    }
    section
}

/// The badge for a working device-sync status; `None` while it is unavailable.
fn sync_status(status: crate::sync::SyncStatus) -> Option<(&'static str, Tone)> {
    match status {
        crate::sync::SyncStatus::Unavailable => None,
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Ready => Some(("Ready", Tone::Success)),
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Pairing => Some(("Pairing", Tone::Info)),
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Syncing => Some(("Syncing", Tone::Info)),
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Error => Some(("Needs attention", Tone::Warning)),
    }
}

fn relative_sync_time(time: i64) -> String {
    let current = ui::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    format!("Last synced {}", ui::format::relative(current - time))
}
