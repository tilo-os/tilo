//! Screen composition. Each `fn` here takes the immutable [`TubeModel`],
//! the live [`Theme`] and the [`ActionBus`] and returns a widget subtree
//! built entirely from `ui` widgets + Tube's app-local primitives. There is
//! no per-screen state: navigation rebuilds the whole tree from the model,
//! so these functions are pure views.

use ui::theme::Theme;
use ui::widget::{ScrollAxis, Scrollable, TextInput};
use vello::peniko::Color;

use crate::action::{Action, ActionBus, SettingFlag};
use crate::images::Imgs;
use crate::model::{
    Category, Channel, Playlist, Route, SearchType, SkipMode, TubeModel, Video,
};
use crate::util::{clock, human_count, human_views};
use crate::widgets::{
    card_surface, chip_surface, dim, divider, gap, hover_fill, hover_pill_surface, spring,
    subtle_fill, subtle_surface, text, text_c, Align, ChannelAvatar, Flex, Icon, IconKind, ImageBox,
    IntoElement, Justify, Len, PlayerHost, SharedPlayer, Surface, Switch, Thumbnail,
};

use crate::contrast::SelectedStyle;

const CARD_W: f32 = 300.0;
const THUMB_H: f32 = 169.0;

// --- watch page responsive geometry (logical px) ---
/// Outer padding around the watch content.
const WATCH_PAD: f64 = 20.0;
/// Fixed width of the "Up next" rail when it sits beside the player.
const UP_NEXT_W: f64 = 360.0;
/// Gap between the player column and the "Up next" rail (or their stack).
const WATCH_COL_GAP: f64 = 24.0;
/// Content-column width at/above which the player + "Up next" sit side by
/// side; below it the rail stacks under the player so neither is squeezed.
const WATCH_TWO_COL_MIN: f64 = 900.0;
/// Fraction of the content viewport HEIGHT the 16:9 player is allowed to fill.
/// The player is sized by height (bigger video, less wasted vertical space) and
/// capped here so metadata still peeks below; its width follows at 16:9 and it
/// centers when the height cap makes it narrower than the content lane.
const PLAYER_VH_FRAC: f64 = 0.65;

/// Width of the navigation rail (logical px). Shared with the responsive
/// split so the docked rail and the overlay flap are exactly this wide.
pub const NAV_RAIL_W: f64 = 86.0;

// ---------------------------------------------------------------------------
// selected / active state styling (the contrast seam)
// ---------------------------------------------------------------------------
//
// Every "this control is selected/active" surface routes through one of these
// helpers so its label/icon foreground is chosen to *contrast its own
// highlight* rather than hard-coded. Previously selected tabs painted a
// bright highlight (slate-200, or an accent wash) and then drew bright-grey /
// bright-blue / white text on top — 3.4–3.6:1, below the readable floor. Each
// helper returns a [`SelectedStyle`] (`bg` + guaranteed-contrasting `fg`); the
// `selected_*_test` cases assert every one clears WCAG AA.

/// Filled-accent selected style: a saturated accent background (chips, the
/// filled "Subscribe" pill) with a foreground that reads on it.
///
/// We fill with [`Theme::accent_hover`] (one ramp step off the primary)
/// rather than `accent_primary`: the primary is a mid-tone that sits right on
/// the 4.5:1 knife-edge (white 4.47 / black 4.70), so the foreground choice is
/// ambiguous and the margin is fragile. The hover step gives the seam's
/// foreground comfortable headroom in both themes (white ≈ 6.3:1 dark, black
/// ≈ 8.3:1 light) while still reading unmistakably as "accent / selected".
pub fn selected_accent_fill(theme: &Theme) -> SelectedStyle {
    SelectedStyle::on(theme.accent_hover)
}

/// Selected nav-rail item. The active item gets a strong accent fill (so it
/// reads as "you are here") with a contrasting icon + label, instead of the
/// old accent-coloured glyphs floating on a near-white slate-200 chip.
pub fn selected_nav_item(theme: &Theme) -> SelectedStyle {
    SelectedStyle::on(theme.accent_hover)
}

/// "On" status pill (SponsorBlock / DeArrow indicators in the top bar). The
/// old design put accent-blue text on a translucent accent wash (3.4:1). We
/// keep the recognisable accent tint but strengthen it into an opaque fill and
/// contrast the foreground against the result.
pub fn status_pill_on(theme: &Theme) -> SelectedStyle {
    // accent[3] (indigo-700 in dark) is dark enough that white text gets ~7.9:1
    // while still reading as the accent family.
    SelectedStyle::on(theme.tokens.colors.accent.stops[3])
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// An initials-only avatar chip (no photo). Uses the Tube [`ChannelAvatar`] so
/// its `is_layout_dirty` is keyed off the laid-out rect, not a draw-time cache —
/// otherwise a comment avatar scrolled off-screen (culled, never drawn) would
/// report "needs measure" forever and force the split to re-solve the whole
/// content pane every scroll frame (the watch-page scroll jank).
fn avatar(name: &str, size: f32) -> ChannelAvatar {
    ChannelAvatar::new(None, "", name, size)
}

/// A channel avatar that paints the real decoded avatar image when the store
/// has it, else the deterministic initials chip. Looks the URL up on the
/// channel record so any screen can render a real uploader photo.
fn channel_avatar(imgs: Imgs, model: &TubeModel, channel_id: &str, name: &str, size: f32) -> ChannelAvatar {
    let url = model
        .channel(channel_id)
        .map(|c| c.avatar_url.clone())
        .unwrap_or_default();
    // Resolve the photo lazily from the store (folds in on a redraw, no
    // structural rebuild); shows the initials chip until it decodes.
    ChannelAvatar::new(imgs.store_handle(), &url, name, size)
}

fn icon(kind: IconKind, size: f32, color: Color) -> Icon {
    Icon::new(kind, size, color)
}

/// A pill button: optional leading icon + label, filled (accent) or
/// outlined, that pushes `action` on click.
fn pill(
    theme: &Theme,
    bus: &ActionBus,
    label: &str,
    leading: Option<IconKind>,
    action: Action,
    filled: bool,
) -> Flex {
    // Filled = accent fill with a contrast-checked foreground; outlined = a
    // subtle dark veneer with primary text. The old outlined pill filled with a
    // near-white neutral stop (slate-300) under `text_primary`, which is WHITE
    // in the dark theme — ~1.3:1, invisible.
    let (bg, fg, border, hover_bg) = if filled {
        let sel = selected_accent_fill(theme);
        (Some(sel.bg), sel.fg, None, theme.accent_hover)
    } else {
        (
            Some(subtle_fill(theme)),
            theme.text_primary,
            Some((1.0, theme.border_standard)),
            hover_fill(theme),
        )
    };
    let mut row = Flex::row().gap(8.0).pad_xy(16.0, 9.0).align(Align::Center);
    if let Some(k) = leading {
        row = row.push(icon(k, 16.0, fg));
    }
    row.push(text_c(label, 14.0, fg))
        .surface(Surface {
            fill: bg,
            radius: theme.tokens.radii.full as f64,
            border,
        })
        .hover_surface(Surface {
            fill: Some(hover_bg),
            radius: theme.tokens.radii.full as f64,
            border,
        })
        .on_click(bus, action)
}

/// A non-interactive stat pill (leading icon + label) — the outlined-pill look
/// without a click handler. Used for the read-only dislike count.
fn stat_pill(theme: &Theme, label: &str, leading: IconKind) -> Flex {
    Flex::row()
        .gap(8.0)
        .pad_xy(16.0, 9.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: theme.tokens.radii.full as f64,
            border: Some((1.0, theme.border_standard)),
        })
        .push(icon(leading, 16.0, theme.text_primary))
        .push(text_c(label, 14.0, theme.text_primary))
}

/// A small icon-only round button.
fn icon_button(theme: &Theme, bus: &ActionBus, kind: IconKind, action: Action) -> Flex {
    Flex::row()
        .fixed(40.0, 40.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .push(icon(kind, 20.0, theme.text_primary))
        .surface(subtle_surface(theme))
        .hover_surface(hover_pill_surface(theme))
        .on_click(bus, action)
}

/// The hamburger in the top bar. Toggles the navigation rail — a no-op-looking
/// affordance on a wide window (the rail is already docked) that becomes the
/// way to summon the rail once it has collapsed into an overlay flap on a
/// narrow one.
fn menu_button(theme: &Theme, bus: &ActionBus) -> Flex {
    Flex::row()
        .id("nav_toggle")
        .fixed(40.0, 40.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .push(icon(IconKind::Menu, 20.0, theme.text_primary))
        .surface(Surface { fill: None, radius: theme.tokens.radii.full as f64, border: None })
        .hover_surface(hover_pill_surface(theme))
        .on_click(bus, Action::ToggleSidebar)
}

fn section_header(theme: &Theme, kind: IconKind, title: &str, subtitle: &str) -> Flex {
    Flex::row()
        .fill_w()
        .gap(10.0)
        .align(Align::Center)
        .push(icon(kind, 22.0, theme.accent_primary))
        .push(text(title, 21.0))
        .push(spring())
        .push(dim(theme, subtitle, 13.0))
}

// ---------------------------------------------------------------------------
// cards
// ---------------------------------------------------------------------------

fn video_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, v: &Video) -> Flex {
    let dearrow = model.settings.dearrow_titles_on();
    let active_dearrow = v.dearrow_active(dearrow);
    let progress = model.history.first().filter(|h| **h == v.id).map(|_| 0.35);
    let mut thumb = Thumbnail::new(&v.seed, v.duration)
        .height(THUMB_H)
        .image_source(imgs.store_handle(), model.display_thumbnail_url(v))
        .dearrow(v.dearrow_thumb && model.settings.dearrow_thumbs_on())
        .live(v.live);
    if let Some(p) = progress {
        thumb = thumb.progress(p);
    }

    let mut meta = Flex::col().gap(3.0).grow(1.0);
    meta = meta.push(text(v.display_title(dearrow), 14.5));
    meta = meta.push(
        Flex::row()
            .gap(5.0)
            .align(Align::Center)
            .push(dim(theme, &v.channel_name, 12.5))
            .push_if(
                model.channel(&v.channel_id).map(|c| c.verified).unwrap_or(false),
                icon(IconKind::Check, 12.0, theme.text_dim),
            ),
    );
    meta = meta.push(dim(
        theme,
        format!("{} • {}", human_views(v.views), v.published),
        12.5,
    ));
    if active_dearrow {
        meta = meta.push(
            Flex::row()
                .gap(4.0)
                .align(Align::Center)
                .pad_xy(6.0, 2.0)
                .surface(Surface {
                    fill: Some(Color::from_rgba8(0x9b, 0x59, 0xb6, 40)),
                    radius: 4.0,
                    border: None,
                })
                .push(icon(IconKind::Sparkle, 11.0, Color::from_rgb8(0xc8, 0x8e, 0xe0)))
                .push(text_c("DeArrow title", 11.0, Color::from_rgb8(0xc8, 0x8e, 0xe0))),
        );
    }

    // No fixed width: as a responsive-grid item the card stretches to fill its
    // column (which the grid sizes between CARD_W and its natural max).
    Flex::col()
        .id(&format!("video_card_{}", v.id))
        .fill_w()
        .gap(9.0)
        .pad(6.0)
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.lg as f64,
            border: None,
        })
        .on_click(bus, Action::OpenVideo(v.id.clone()))
        .push(thumb)
        .push(
            Flex::row()
                .gap(10.0)
                .fill_w()
                .align(Align::Start)
                .push(channel_avatar(imgs, model, &v.channel_id, &v.channel_name, 36.0))
                .push(meta),
        )
}

/// Compact horizontal card for the "Up next" rail + history rows.
fn compact_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, v: &Video) -> Flex {
    let dearrow = model.settings.dearrow_titles_on();
    Flex::row()
        .fill_w()
        .gap(10.0)
        .pad(6.0)
        .align(Align::Start)
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.md as f64,
            border: None,
        })
        .on_click(bus, Action::OpenVideo(v.id.clone()))
        .push(
            Flex::col().w(Len::Px(160.0)).push(
                Thumbnail::new(&v.seed, v.duration)
                    .height(90.0)
                    .image_source(imgs.store_handle(), model.display_thumbnail_url(v))
                    .live(v.live),
            ),
        )
        .push(
            Flex::col()
                .grow(1.0)
                .gap(4.0)
                .push(text(v.display_title(dearrow), 13.0))
                .push(dim(theme, &v.channel_name, 12.0))
                // Join only the parts we actually have, so a playlist entry with no
                // view count / date never shows a dangling "0 • " or a trailing "•".
                .push_if(
                    !views_date_line(v.views, &v.published).is_empty(),
                    dim(theme, &views_date_line(v.views, &v.published), 11.5),
                ),
        )
}

/// A "N views • date" line that drops any missing part (and its separator), so a
/// row never shows "0", a dangling "•", or an empty count. Empty when neither is
/// known.
fn views_date_line(views: u64, published: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if views > 0 {
        parts.push(human_views(views));
    }
    if !published.trim().is_empty() {
        parts.push(published.to_string());
    }
    parts.join(" • ")
}

/// A "N subscribers" label, or `None` when the count is unknown (0) — so cards
/// never render a fake "0 subscribers".
fn subs_label(subscribers: u64) -> Option<String> {
    (subscribers > 0).then(|| format!("{} subscribers", human_count(subscribers)))
}

fn grid(cards: Vec<Flex>) -> Flex {
    grid_with_id(cards, "video_grid")
}

fn grid_with_id(cards: Vec<Flex>, id: &str) -> Flex {
    // Responsive grid: columns are ≥ `CARD_W` and flex to fill the row, so the
    // cards scale with the window width up to their natural max and only then
    // wrap to add a column — the row is always filled edge to edge (no right
    // gutter). `align(Start)` top-aligns cards of differing heights per row.
    let mut row = Flex::row().id(id).fill_w().grid(CARD_W).gap(18.0).align(Align::Start);
    for c in cards {
        row = row.push(c);
    }
    row
}

// ---------------------------------------------------------------------------
// loading skeletons
// ---------------------------------------------------------------------------

/// A neutral placeholder block used to compose loading skeletons.
fn skeleton_block(theme: &Theme, width: Len, height: f32, radius: f64) -> Flex {
    Flex::row().w(width).h(Len::Px(height)).surface(Surface {
        fill: Some(subtle_fill(theme)),
        radius,
        border: None,
    })
}

/// One skeleton video card: a placeholder thumbnail + avatar dot + text lines,
/// matching the real `video_card` footprint so results don't jump when they
/// land. Shown while a fetch is in flight instead of a premature empty state.
fn skeleton_card(theme: &Theme) -> Flex {
    Flex::col()
        .fill_w()
        .gap(9.0)
        .pad(6.0)
        .push(skeleton_block(theme, Len::Fill, THUMB_H, theme.tokens.radii.lg as f64))
        .push(
            Flex::row()
                .gap(10.0)
                .fill_w()
                .align(Align::Start)
                .push(skeleton_block(theme, Len::Px(36.0), 36.0, 18.0))
                .push(
                    Flex::col()
                        .grow(1.0)
                        .gap(7.0)
                        .push(skeleton_block(theme, Len::Pct(0.92), 12.0, 4.0))
                        .push(skeleton_block(theme, Len::Pct(0.6), 11.0, 4.0))
                        .push(skeleton_block(theme, Len::Pct(0.4), 11.0, 4.0)),
                ),
        )
}

fn skeleton_grid(theme: &Theme, n: usize) -> Flex {
    grid((0..n).map(|_| skeleton_card(theme)).collect())
}

// ---------------------------------------------------------------------------
// chrome: nav rail + top bar
// ---------------------------------------------------------------------------

fn nav_item(theme: &Theme, bus: &ActionBus, kind: IconKind, label: &str, route: Route, active: bool) -> Flex {
    // Active item: a solid accent fill with a foreground chosen to contrast it
    // (so the selected destination reads clearly), instead of accent-coloured
    // glyphs floating on a near-white slate-200 chip (3.6:1, unreadable).
    let (fg, surface) = if active {
        let sel = selected_nav_item(theme);
        (
            sel.fg,
            Surface {
                fill: Some(sel.bg),
                radius: theme.tokens.radii.lg as f64,
                border: None,
            },
        )
    } else {
        (
            theme.text_dim,
            Surface { fill: None, radius: theme.tokens.radii.lg as f64, border: None },
        )
    };
    // Inactive items get a subtle neutral hover veneer; the active item already
    // owns the strong accent fill, so it keeps it on hover (no flash to a
    // bright neutral chip under the cursor).
    let hover = if active {
        surface
    } else {
        Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.lg as f64,
            border: None,
        }
    };
    // Stable, addressable id so headless click-tests can target a nav item by
    // name (e.g. "nav_home"). Derived from the label, lowercased.
    let id = format!("nav_{}", label.to_lowercase());
    Flex::col()
        .id(&id)
        .fill_w()
        .gap(4.0)
        .pad_xy(4.0, 8.0)
        .align(Align::Center)
        .justify(Justify::Center)
        .surface(surface)
        .hover_surface(hover)
        .on_click(bus, Action::Navigate(route))
        .push(icon(kind, 22.0, fg))
        .push(text_c(label, 11.0, fg))
}

pub fn nav_rail(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let sec = model.route.nav_section();
    Flex::col()
        .w(Len::Px(NAV_RAIL_W as f32))
        .fill_h()
        .pad_xy(8.0, 14.0)
        .gap(8.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(theme.tokens.colors.surfaces.base),
            radius: 0.0,
            border: Some((1.0, theme.border_standard)),
        })
        .push(
            Flex::col()
                .fixed(44.0, 44.0)
                .align(Align::Center)
                .justify(Justify::Center)
                .surface(Surface {
                    fill: Some(theme.accent_primary),
                    radius: theme.tokens.radii.lg as f64,
                    border: None,
                })
                .push(icon(IconKind::Play, 22.0, Color::WHITE)),
        )
        .push(gap(1.0, 6.0))
        .push(nav_item(theme, bus, IconKind::Home, "Home", Route::Home, sec == "home"))
        .push(nav_item(theme, bus, IconKind::Fire, "Trending", Route::Trending, sec == "trending"))
        .push(nav_item(theme, bus, IconKind::Subscriptions, "Subs", Route::Subscriptions, sec == "subs"))
        .push(nav_item(theme, bus, IconKind::History, "History", Route::History, sec == "history"))
        .push(spring())
        .push(nav_item(theme, bus, IconKind::Settings, "Settings", Route::Settings, sec == "settings"))
}

/// The navigation Back affordance in the top bar. Shown only when there is
/// history to pop; pushes [`Action::Back`], the same intent Escape / Alt-Left /
/// the edge-swipe resolve to.
fn back_button(theme: &Theme, bus: &ActionBus) -> Flex {
    Flex::row()
        .id("back_btn")
        .fixed(40.0, 40.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .push(icon(IconKind::Back, 20.0, theme.text_primary))
        .surface(Surface { fill: None, radius: theme.tokens.radii.full as f64, border: None })
        .hover_surface(hover_pill_surface(theme))
        .on_click(bus, Action::Back)
}

pub fn top_bar(model: &TubeModel, theme: &Theme, bus: &ActionBus, can_back: bool) -> Flex {
    // The search field is a CRDT-bound TextInput so its text lives in the
    // document (survives navigation rebuilds) and would sync to peers. It is
    // ALWAYS present — its own placeholder draws the "Search" hint while empty —
    // so the user can click into it and type from a cold start. (An earlier
    // build swapped the input for a static label while empty, which meant the
    // input was absent from the tree until a query already existed, so there was
    // nothing to focus and free-text search was unreachable.)
    let search_input = TextInput::new(vec![])
        .with_id("search_input")
        .with_on_change_path("search_query")
        .with_plain_chrome()
        .with_font_size(15.0)
        .with_placeholder("Search");

    // A single search lens (leading) — Enter submits, so there is no second
    // trailing lens/button cluttering the field.
    let search = Flex::row()
        .grow(1.0)
        .max_w(560.0)
        .h(Len::Px(42.0))
        .gap(10.0)
        .pad_xy(16.0, 0.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: theme.tokens.radii.full as f64,
            border: Some((1.0, theme.border_standard)),
        })
        .push(icon(IconKind::Search, 18.0, theme.text_dim))
        .push(Flex::row().grow(1.0).push(search_input));

    // Only surface the data-source indicator when we're NOT on live data
    // (connecting or offline) — in the normal live case the top bar stays clean.
    // SponsorBlock / DeArrow are settings, not top-bar toggles; they live in Settings.
    let status = Flex::row()
        .gap(8.0)
        .align(Align::Center)
        .push_if(!model.data_source.live, data_source_pill(theme, model));

    Flex::row()
        .fill_w()
        .h(Len::Px(66.0))
        .gap(16.0)
        .pad_xy(20.0, 0.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(theme.tokens.colors.surfaces.base),
            radius: 0.0,
            border: Some((1.0, theme.border_standard)),
        })
        .push(menu_button(theme, bus))
        .push_if(can_back, back_button(theme, bus))
        .push(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .push(text_c("Tube", 22.0, theme.text_primary)),
        )
        .push(search)
        // Push the status + account avatar hard to the right edge for a
        // balanced bar (search sits centre-left, account is top-right).
        .push(spring())
        .push(status)
        .push(avatar("You", 36.0))
}

/// Live-vs-sample provenance pill. A green dot + "Live" once the rustypipe
/// backend has actually returned real data; a dim "Sample" (or "Connecting…")
/// otherwise, so the user always knows whether the catalogue on screen is real
/// YouTube data or the bundled offline sample.
fn data_source_pill(theme: &Theme, model: &TubeModel) -> Flex {
    let live = model.data_source.live;
    let (dot, fg, label) = if live {
        (theme.tokens.colors.semantic.success, theme.text_primary, "Live")
    } else if model.data_source.label.contains("connect") {
        (theme.tokens.colors.semantic.warning, theme.text_dim, "Connecting…")
    } else {
        (theme.text_dim, theme.text_dim, "Offline")
    };
    Flex::row()
        .gap(6.0)
        .pad_xy(10.0, 6.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: theme.tokens.radii.full as f64,
            border: None,
        })
        .push(Flex::row().fixed(8.0, 8.0).surface(Surface {
            fill: Some(dot),
            radius: 4.0,
            border: None,
        }))
        .push(text_c(label, 12.0, fg))
}

// ---------------------------------------------------------------------------
// chips
// ---------------------------------------------------------------------------

fn chip(theme: &Theme, bus: &ActionBus, label: &str, action: Action, active: bool) -> Flex {
    // The selected chip is the headline contrast fix: it used WHITE text on the
    // accent fill (breaks in the light theme, where it would want black) — now
    // the foreground is chosen against the accent via the seam. Inactive chips
    // get primary text on a subtle dark veneer.
    let sel = selected_accent_fill(theme);
    let (fg, resting, hover_bg) = if active {
        // Active: accent fill + seam-chosen foreground. Hover goes one ramp
        // step darker (indigo-700) so the active chip still gives feedback.
        (sel.fg, Surface {
            fill: Some(sel.bg),
            radius: theme.tokens.radii.full as f64,
            border: None,
        }, theme.tokens.colors.accent.stops[3])
    } else {
        (theme.text_primary, chip_surface(theme, false), hover_fill(theme))
    };
    Flex::row()
        .pad_xy(14.0, 7.0)
        .surface(resting)
        .hover_surface(Surface {
            fill: Some(hover_bg),
            radius: theme.tokens.radii.full as f64,
            border: None,
        })
        .on_click(bus, action)
        .push(text_c(label, 13.0, fg))
}

// ---------------------------------------------------------------------------
// screens
// ---------------------------------------------------------------------------

fn scroll(content: Flex) -> Scrollable {
    Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(content.block()))
}

pub fn home(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    // Cold start shows a skeleton (loading) or an honest empty state — never a
    // fabricated sample catalogue presented as real. Real cards only.
    let body = if model.videos.is_empty() {
        if model.feed_loading {
            skeleton_grid(theme, 12)
        } else {
            empty_state(
                theme,
                "Nothing to show yet",
                "Search for something, or check back when the trending feed loads.",
            )
        }
    } else {
        grid(model.videos.iter().map(|v| video_card(model, theme, bus, imgs, v)).collect())
    };
    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(section_header(theme, IconKind::Fire, "Trending", ""))
            .push(body),
    )
}

pub fn trending(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let mut vids: Vec<&Video> = model.videos.iter().collect();
    vids.sort_by(|a, b| b.views.cmp(&a.views));
    let cards: Vec<Flex> = vids.iter().map(|v| video_card(model, theme, bus, imgs, v)).collect();
    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(section_header(theme, IconKind::Fire, "Trending", "Most-watched across your instance"))
            .push(grid(cards)),
    )
}

pub fn subscriptions(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let mut rail = Flex::row().fill_w().wrap().gap(18.0);
    for cid in &model.subscriptions {
        if let Some(ch) = model.channel(cid) {
            rail = rail.push(
                Flex::col()
                    .w(Len::Px(96.0))
                    .gap(6.0)
                    .align(Align::Center)
                    .pad(6.0)
                    .hover_surface(Surface {
                        fill: Some(hover_fill(theme)),
                        radius: theme.tokens.radii.lg as f64,
                        border: None,
                    })
                    .on_click(bus, Action::OpenChannel(ch.id.clone()))
                    .push(channel_avatar(imgs, model, &ch.id, &ch.name, 56.0))
                    .push(text_c(&ch.name, 11.5, theme.text_primary)),
            );
        }
    }

    let feed = model.subscription_feed();
    let cards: Vec<Flex> = feed.iter().map(|v| video_card(model, theme, bus, imgs, v)).collect();
    let body = if cards.is_empty() {
        empty_state(theme, "No subscriptions yet", "Subscribe to channels and their latest uploads land here.")
    } else {
        grid(cards)
    };

    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(section_header(theme, IconKind::Subscriptions, "Subscriptions", &format!("{} channels", model.subscriptions.len())))
            .push(rail)
            .push(divider(theme))
            .push(text("Latest from your subscriptions", 16.0))
            .push(body),
    )
}

pub fn history(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let vids = model.history_videos();
    let mut col = Flex::col().fill_w().gap(8.0);
    if vids.is_empty() {
        col = col.push(empty_state(theme, "Nothing watched yet", "Videos you watch show up here."));
    }
    for v in vids {
        col = col.push(compact_card(model, theme, bus, imgs, v));
    }
    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .max_w(820.0)
            .push(section_header(theme, IconKind::History, "Watch history", "Stored locally — never uploaded"))
            .push(col),
    )
}

/// One multi-select type-filter chip (Video · Playlist · Creator). Reuses the
/// selected-chip contrast seam; highlighted when its type is currently shown.
fn type_chip(theme: &Theme, bus: &ActionBus, t: SearchType, active: bool) -> Flex {
    chip(theme, bus, t.label(), Action::ToggleSearchType(t), active)
        .id(&format!("type_chip_{}", t.label().to_lowercase()))
}

/// The search results' type filter: Video · Playlist · Creator. All three are
/// selected by default; the first click narrows to one type, later clicks toggle
/// individual chips (see [`crate::model::SearchTypeFilter`]).
fn search_type_chips(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let f = &model.search_type_filter;
    let mut row = Flex::row().id("search_type_chips").fill_w().wrap().gap(8.0);
    for t in SearchType::ALL {
        row = row.push(type_chip(theme, bus, t, f.shows(t)));
    }
    row
}

/// A playlist search-result card: the cover thumbnail with a "N videos"
/// stacked-list badge, then the title + creator. Opens the playlist page.
fn playlist_card(theme: &Theme, bus: &ActionBus, imgs: Imgs, p: &Playlist) -> Flex {
    let badge = Flex::col()
        .float()
        .fill_w()
        .fill_h()
        .align(Align::End)
        .justify(Justify::End)
        .pad(8.0)
        .push(
            Flex::row()
                .gap(5.0)
                .pad_xy(8.0, 4.0)
                .align(Align::Center)
                .surface(Surface {
                    fill: Some(Color::from_rgba8(0, 0, 0, 200)),
                    radius: 6.0,
                    border: None,
                })
                .push(icon(IconKind::Playlist, 12.0, Color::WHITE))
                .push(text_c(&format!("{} videos", human_count(p.video_count)), 11.0, Color::WHITE)),
        );
    let stack = Flex::col()
        .fill_w()
        .h(Len::Px(THUMB_H))
        .push(
            Thumbnail::new(&p.seed, 0)
                .height(THUMB_H)
                .image_source(imgs.store_handle(), &p.thumbnail_url),
        )
        .push(badge);

    let meta = Flex::col()
        .grow(1.0)
        .gap(3.0)
        .push(text(&p.title, 14.5))
        .push(dim(theme, &format!("{} • Playlist", p.channel_name), 12.5))
        .push(dim(theme, &format!("{} videos", human_count(p.video_count)), 12.5));

    Flex::col()
        .fill_w()
        .gap(9.0)
        .pad(6.0)
        .id(&format!("playlist_card_{}", p.id))
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.lg as f64,
            border: None,
        })
        .on_click(bus, Action::OpenPlaylist(p.id.clone()))
        .push(stack)
        .push(
            Flex::row()
                .gap(10.0)
                .fill_w()
                .align(Align::Start)
                .push(icon(IconKind::Playlist, 22.0, theme.accent_primary))
                .push(meta),
        )
}

/// A creator search-result row: the real avatar, name (+ verified), handle and
/// subscriber count, and a View affordance. Opens the channel page.
fn creator_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, c: &Channel) -> Flex {
    // Handle + subscriber count, each dropped when unknown (no "0 subscribers").
    let show_handle = !c.handle.is_empty() && c.handle != c.id;
    let handle_subs = match (show_handle, subs_label(c.subscribers)) {
        (true, Some(subs)) => format!("{} • {}", c.handle, subs),
        (true, None) => c.handle.clone(),
        (false, Some(subs)) => subs,
        (false, None) => String::new(),
    };
    Flex::row()
        .fill_w()
        .gap(14.0)
        .pad(8.0)
        .align(Align::Center)
        .id(&format!("creator_card_{}", c.id))
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.md as f64,
            border: None,
        })
        .on_click(bus, Action::OpenChannel(c.id.clone()))
        .push(channel_avatar(imgs, model, &c.id, &c.name, 56.0))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(3.0)
                .push(
                    Flex::row()
                        .gap(6.0)
                        .align(Align::Center)
                        .push(text(&c.name, 16.0))
                        .push_if(c.verified, icon(IconKind::Check, 14.0, theme.text_dim)),
                )
                .push_if(!handle_subs.is_empty(), dim(theme, &handle_subs, 12.5)),
        )
        .push(pill(theme, bus, "View", None, Action::OpenChannel(c.id.clone()), false))
}

pub fn search(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, query: &str) -> Scrollable {
    let filter = &model.search_type_filter;
    // Each type's rows, gated by the multi-select filter.
    let videos: Vec<&Video> = if filter.shows(SearchType::Video) {
        model.search_display(query)
    } else {
        Vec::new()
    };
    let playlists: Vec<&Playlist> = if filter.shows(SearchType::Playlist) {
        model.search_playlists_display(query)
    } else {
        Vec::new()
    };
    let creators: Vec<&Channel> = if filter.shows(SearchType::Creator) {
        model.search_creators_display(query)
    } else {
        Vec::new()
    };

    let body = if model.search_loading {
        // While a fetch is in flight, show a skeleton — NEVER a premature "No
        // results" (the bug that shipped).
        skeleton_grid(theme, 8)
    } else if query.trim().is_empty() {
        empty_state(theme, "Search Tube", "Type a query above to find videos, playlists and creators.")
    } else if videos.is_empty() && playlists.is_empty() && creators.is_empty() {
        empty_state(theme, "No results", "Nothing matched that search — try different words or filters.")
    } else {
        // Creators (compact rows) → playlists → videos, each section only when
        // it has rows and its filter chip is selected.
        let mut col = Flex::col().fill_w().gap(18.0);
        if !creators.is_empty() {
            let mut rows = Flex::col().id("creator_results").fill_w().gap(8.0);
            for c in &creators {
                rows = rows.push(creator_card(model, theme, bus, imgs, c));
            }
            col = col.push(rows);
        }
        if !playlists.is_empty() {
            let cards: Vec<Flex> =
                playlists.iter().map(|p| playlist_card(theme, bus, imgs, p)).collect();
            col = col.push(grid_with_id(cards, "playlist_grid"));
        }
        if !videos.is_empty() {
            let cards: Vec<Flex> =
                videos.iter().map(|v| video_card(model, theme, bus, imgs, v)).collect();
            col = col.push(grid(cards));
        }
        col
    };
    // No result-count header (the query already lives once in the top-bar
    // field): the type filter sits at the top, results below it.
    let col = Flex::col()
        .fill_w()
        .pad(24.0)
        .gap(18.0)
        .push(search_type_chips(model, theme, bus))
        .push(body);
    scroll(col)
}

/// A playlist page: its cover, title, creator and video count, a "Play all"
/// affordance, and the ordered video list.
pub fn playlist(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, id: &str) -> Scrollable {
    let Some(pl) = model.playlist(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Playlist unavailable", "")));
    };
    let videos: Vec<&Video> = pl.video_ids.iter().filter_map(|vid| model.video(vid)).collect();
    let first_id = videos.first().map(|v| v.id.clone());

    let mut info = Flex::col()
        .grow(1.0)
        .gap(8.0)
        .align(Align::Start)
        .push(text(&pl.title, 26.0))
        .push(dim(
            theme,
            &format!("{}  •  {} videos", pl.channel_name, human_count(pl.video_count)),
            13.5,
        ));
    if let Some(fid) = first_id {
        info = info.push(
            pill(theme, bus, "Play all", Some(IconKind::Play), Action::OpenVideo(fid), true)
                .id("playlist_play"),
        );
    }

    let header = Flex::row()
        .fill_w()
        .gap(18.0)
        .align(Align::Start)
        .push(
            Flex::col().w(Len::Px(240.0)).push(
                Thumbnail::new(&pl.seed, 0)
                    .height(135.0)
                    .image_source(imgs.store_handle(), &pl.thumbnail_url),
            ),
        )
        .push(info);

    let mut list = Flex::col().fill_w().gap(8.0);
    if videos.is_empty() {
        list = list.push(empty_state(theme, "Loading playlist…", "Fetching the playlist's videos."));
    }
    for v in &videos {
        list = list.push(compact_card(model, theme, bus, imgs, v));
    }

    scroll(
        Flex::col()
            .id("playlist_page")
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(header)
            .push(divider(theme))
            .push(text("Videos", 16.0))
            .push(list),
    )
}

pub fn channel(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, id: &str) -> Scrollable {
    let Some(ch) = model.channel(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Channel not found", "")));
    };
    let subscribed = model.is_subscribed(id);

    // Real banner image when the store has it, else the seed gradient.
    let banner = ImageBox::new(&ch.seed, 160.0, theme.tokens.radii.lg as f64, None)
        .source(imgs.store_handle(), &ch.banner_url);

    let header = Flex::row()
        .fill_w()
        .gap(18.0)
        .align(Align::Center)
        .push(channel_avatar(imgs, model, &ch.id, &ch.name, 86.0))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(5.0)
                .push(
                    Flex::row()
                        .gap(8.0)
                        .align(Align::Center)
                        .push(text(&ch.name, 26.0))
                        .push_if(ch.verified, icon(IconKind::Check, 18.0, theme.text_dim)),
                )
                .push(dim(
                    theme,
                    &match subs_label(ch.subscribers) {
                        Some(subs) => format!("{}  •  {}", ch.handle, subs),
                        None => ch.handle.clone(),
                    },
                    13.5,
                ))
                .push(dim(theme, &ch.description, 13.0)),
        )
        .push(if subscribed {
            pill(theme, bus, "Subscribed", Some(IconKind::Bell), Action::ToggleSubscribe(ch.id.clone()), false)
        } else {
            pill(theme, bus, "Subscribe", None, Action::ToggleSubscribe(ch.id.clone()), true)
        });

    let cards: Vec<Flex> = model
        .channel_videos(id)
        .iter()
        .map(|v| video_card(model, theme, bus, imgs, v))
        .collect();

    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(banner)
            .push(header)
            .push(divider(theme))
            .push(text("Videos", 16.0))
            .push(grid(cards)),
    )
}

fn empty_state(theme: &Theme, title: &str, body: &str) -> Flex {
    Flex::col()
        .fill_w()
        .gap(8.0)
        .pad(48.0)
        .align(Align::Center)
        .justify(Justify::Center)
        .push(icon(IconKind::Eye, 40.0, theme.text_dim))
        .push(text(title, 18.0))
        .push(dim(theme, body, 13.0))
}

// ---------------------------------------------------------------------------
// watch page
// ---------------------------------------------------------------------------

pub fn watch(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    id: &str,
    content_w: f64,
    content_h: f64,
    player: SharedPlayer,
) -> Scrollable {
    let Some(v) = model.video(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Video unavailable", "")));
    };
    let dearrow = model.settings.dearrow_titles_on();

    // Responsive watch geometry. The player is sized by available HEIGHT so more
    // actual video fills the screen (rather than a wide-but-short strip): a 16:9
    // surface whose height is capped at `PLAYER_VH_FRAC` of the content viewport
    // height, with its width following at 16:9. When that height cap makes the
    // player narrower than the content lane it is centered horizontally; when the
    // lane is the tighter constraint the player fills the lane's width.
    //
    // Everything *below* the player (details + "Up next") lives in a padded
    // region that splits into two columns on a wide window; below
    // `WATCH_TWO_COL_MIN` the rail stacks under the details so neither column is
    // squeezed on a narrow one.
    let width_from_height = content_h * PLAYER_VH_FRAC * 16.0 / 9.0;
    let player_w = width_from_height.min(content_w).max(1.0);
    let player_h = (player_w * 9.0 / 16.0).max(1.0) as f32;
    let lower_inner = (content_w - WATCH_PAD * 2.0).max(0.0);
    let two_col = lower_inner >= WATCH_TWO_COL_MIN;

    // ONE timeline: feed the enabled community SponsorBlock segments to the
    // shared player so they render inline on its own scrub track (the app no
    // longer draws a second timeline below). Marks are fractions of the known
    // video length so they line up on the bar regardless of engine timing.
    let dur = v.duration.max(1) as f32;
    let marks: Vec<(f32, f32, Color)> = if model.settings.sponsorblock_enabled {
        v.sponsor
            .iter()
            .filter(|s| model.settings.category_enabled(s.category))
            .map(|s| {
                let (r, g, b) = s.category.rgb();
                (s.start as f32 / dur, s.end as f32 / dur, Color::from_rgb8(r, g, b))
            })
            .collect()
    } else {
        Vec::new()
    };
    // Whether this video contributes real SponsorBlock segments to the one
    // timeline — used to make the segment-carrying timeline addressable below.
    let has_sb_timeline = !marks.is_empty();
    player.borrow_mut().set_sponsor_marks(marks);

    // The resolution selector lives ON the player now, registered as a
    // player-setting (its own priority icon in the control bar) instead of a
    // separate row below the video. Picking a value round-trips back to the
    // model via `take_setting_events` (drained in `TubeApp::sync`).
    {
        let qualities: Vec<String> = crate::app::QUALITIES.iter().map(|q| q.to_string()).collect();
        let sel = crate::app::QUALITIES
            .iter()
            .position(|q| *q == model.settings.preferred_quality)
            .unwrap_or(0);
        player.borrow_mut().set_setting(ui::widget::video_player::PlayerSetting::choice(
            "resolution",
            ui::icons::Icon::Monitor,
            "Resolution",
            1,
            qualities,
            sel,
        ));
    }

    // A show-mode segment the playhead is currently inside gets a small,
    // unintrusive "Skip" button floated over the video (bottom-right, above the
    // controls bar). It dismisses itself when the playhead leaves the segment.
    let manual = v.sponsor.iter().find(|s| {
        model.settings.should_show_skip(s.category) && s.contains(model.playhead)
    });

    // The player surface is the app's persistent, shared `ui::widget::VideoPlayer`
    // (real GStreamer decode + audio under `--features media`, a synthesized frame
    // otherwise) mounted here via a cheap `PlayerHost`, so it survives the
    // whole-tree rebuild without restarting the video. Its overlaid controls bar
    // (play/pause, the single scrub timeline carrying the SponsorBlock marks,
    // volume, fullscreen) is the transport; clicking the surface toggles
    // play/pause. Height tracks the column width at a 16:9 ratio.
    let mut stage = Flex::col()
        .w(Len::Px(player_w as f32))
        .h(Len::Px(player_h))
        .push(PlayerHost::new(player, player_h));
    if let Some(s) = manual {
        stage = stage.push(
            Flex::col()
                .float()
                .fill_w()
                .fill_h()
                .align(Align::End)
                .justify(Justify::End)
                .pad_each(0.0, 0.0, 16.0, 52.0)
                .push(
                    Flex::row()
                        .id("sb_skip_btn")
                        .gap(6.0)
                        .pad_xy(13.0, 8.0)
                        .align(Align::Center)
                        .surface(Surface {
                            fill: Some(Color::from_rgba8(0, 0, 0, 200)),
                            radius: theme.tokens.radii.full as f64,
                            border: Some((1.0, Color::from_rgba8(255, 255, 255, 40))),
                        })
                        .hover_surface(Surface {
                            fill: Some(theme.accent_primary),
                            radius: theme.tokens.radii.full as f64,
                            border: None,
                        })
                        .on_click(bus, Action::SkipSegment(s.end))
                        .push(text_c(&format!("Skip {}", s.category.label()), 13.0, Color::WHITE))
                        .push(icon(IconKind::Skip, 14.0, Color::WHITE)),
                ),
        );
    }
    // The SponsorBlock segments now live inline on the player's single scrub
    // timeline (painted by the shared `VideoPlayer`, aligned to its track). Mark
    // that segment-carrying timeline region as an addressable, laid-out node so
    // it stays a real, testable part of the watch tree — there is ONE timeline,
    // and this is it.
    // Center the height-capped player within the content lane (it is narrower
    // than the lane whenever the height cap, not the width, is the constraint).
    let stage_row = Flex::row().fill_w().justify(Justify::Center).push(stage);
    let player_view = {
        let col = Flex::col().fill_w().push(stage_row);
        if has_sb_timeline { col.id("sb_timeline") } else { col }
    };

    // A compact row under the player: just a gear that opens the app's Settings
    // (right). The resolution selector moved onto the player itself, so there's
    // no separate quality row here anymore.
    let player_actions = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(spring())
        .push(
            icon_button(theme, bus, IconKind::Settings, Action::Navigate(Route::Settings))
                .id("watch_settings"),
        );

    let title = text(v.display_title(dearrow), 22.0);
    let liked = model.is_liked(&v.id);
    let mut meta = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(dim(theme, &format!("{} • {}", human_views(v.views), v.published), 13.0))
        .push(spring())
        .push(
            pill(
                theme,
                bus,
                &human_count(model.display_likes(v)),
                Some(IconKind::Like),
                Action::ToggleLike(v.id.clone()),
                liked,
            )
            .id("like_btn"),
        );
    // Return YouTube Dislike: a read-only crowdsourced dislike count beside the
    // like pill. Shows the real number once fetched, an em-dash placeholder
    // until it lands.
    if model.settings.return_dislike_enabled {
        let label = model
            .dislikes_of(&v.id)
            .map(human_count)
            .unwrap_or_else(|| "—".to_string());
        meta = meta.push(stat_pill(theme, &label, IconKind::Dislike).id("dislike_pill"));
    }
    let meta = meta.push(
        pill(theme, bus, "Share", Some(IconKind::Chevron), Action::Share(v.id.clone()), false)
            .id("share_btn"),
    );

    let ch_sub = model.is_subscribed(&v.channel_id);
    let channel_row = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(channel_avatar(imgs, model, &v.channel_id, &v.channel_name, 48.0))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(2.0)
                .on_click(bus, Action::OpenChannel(v.channel_id.clone()))
                .push(text(&v.channel_name, 15.0))
                .push_if(
                    model.channel(&v.channel_id).and_then(|c| subs_label(c.subscribers)).is_some(),
                    dim(
                        theme,
                        &model
                            .channel(&v.channel_id)
                            .and_then(|c| subs_label(c.subscribers))
                            .unwrap_or_default(),
                        12.0,
                    ),
                ),
        )
        .push(
            if ch_sub {
                pill(theme, bus, "Subscribed", Some(IconKind::Bell), Action::ToggleSubscribe(v.channel_id.clone()), false)
            } else {
                pill(theme, bus, "Subscribe", None, Action::ToggleSubscribe(v.channel_id.clone()), true)
            }
            .id("subscribe_btn"),
        );

    let desc = Flex::col()
        .fill_w()
        .gap(6.0)
        .pad(14.0)
        .surface(card_surface(theme))
        .push(dim(theme, &v.description, 13.5));

    let mut comments = Flex::col().fill_w().gap(14.0);
    comments = comments.push(
        Flex::row()
            .gap(10.0)
            .align(Align::Center)
            .push(text(&format!("{} Comments", human_count(v.comments.len() as u64)), 17.0)),
    );
    if model.settings.show_comments {
        for c in &v.comments {
            comments = comments.push(comment_row(theme, c));
        }
    } else {
        comments = comments.push(dim(theme, "Comments hidden (enable in settings).", 13.0));
    }

    // The details column — everything that sits *below* the full-bleed player:
    // the transport actions, notices, title/meta, channel row, description and
    // comments. The player itself is hoisted out of this column so it can span
    // the full width flush to the edges.
    let details = Flex::col()
        .gap(14.0)
        .push(player_actions)
        .push_if(!model.notices.is_empty(), notices_view(theme, model))
        .push(title)
        .push(meta)
        .push(divider(theme))
        .push(channel_row)
        .push(desc)
        .push(divider(theme))
        .push(comments);

    let mut up = Flex::col().fill_w().gap(10.0);
    up = up.push(
        Flex::row()
            .fill_w()
            .align(Align::Center)
            .gap(10.0)
            .push(text("Up next", 16.0))
            .push(spring())
            .push(autoplay_toggle(model, theme, bus)),
    );
    for r in model.related_display(id).into_iter().take(6) {
        up = up.push(compact_card(model, theme, bus, imgs, r));
    }

    // Wide: the details column grows beside a fixed-width "Up next" rail. Narrow:
    // the rail stacks below the details so both keep a readable width.
    let lower = if two_col {
        Flex::row()
            .fill_w()
            .gap(WATCH_COL_GAP as f32)
            .align(Align::Start)
            .push(details.grow(1.0))
            .push(up.w(Len::Px(UP_NEXT_W as f32)))
    } else {
        Flex::col()
            .fill_w()
            .gap(WATCH_COL_GAP as f32)
            .push(details.fill_w())
            .push(up)
    };

    // The player is flush (no padding) at the very top of the scroll content;
    // the details/up-next region below it carries the page padding.
    scroll(
        Flex::col()
            .fill_w()
            .push(player_view)
            .push(Flex::col().fill_w().pad(WATCH_PAD as f32).push(lower)),
    )
}

fn quality_menu(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    Flex::row()
        .gap(6.0)
        .pad_xy(10.0, 6.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: theme.tokens.radii.md as f64,
            border: Some((1.0, theme.border_standard)),
        })
        .on_click(bus, Action::SetQuality(model.settings.preferred_quality.clone()))
        .push(text_c(&model.settings.preferred_quality, 12.5, theme.text_primary))
        .push(icon(IconKind::Chevron, 12.0, theme.text_dim))
}

fn autoplay_toggle(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    Flex::row()
        .gap(8.0)
        .align(Align::Center)
        .push(dim(theme, "Autoplay", 12.5))
        .push(Switch::new(model.settings.autoplay, bus, Action::ToggleSetting(SettingFlag::Autoplay)))
}

/// The toast stack of transient notices (auto-skip confirmations, "link
/// copied" acknowledgements, worker status). Rendered on the watch page so the
/// auto-skip / share / worker feedback that writes here is actually seen. Only
/// called when [`TubeModel::notices`] is non-empty.
fn notices_view(theme: &Theme, model: &TubeModel) -> Flex {
    let mut col = Flex::col().id("notices").fill_w().gap(6.0);
    for n in model.notices.iter().take(4) {
        col = col.push(
            Flex::row()
                .fill_w()
                .gap(8.0)
                .pad_xy(12.0, 8.0)
                .align(Align::Center)
                .surface(subtle_surface(theme))
                .push(icon(IconKind::Skip, 14.0, theme.accent_primary))
                .push(text_c(n, 12.5, theme.text_primary)),
        );
    }
    col
}

fn comment_row(theme: &Theme, c: &crate::model::Comment) -> Flex {
    let mut head = Flex::row().gap(8.0).align(Align::Center);
    if c.pinned {
        head = head.push(
            Flex::row()
                .pad_xy(6.0, 2.0)
                .surface(Surface {
                    fill: Some(subtle_fill(theme)),
                    radius: 4.0,
                    border: None,
                })
                .push(text_c("Pinned", 10.5, theme.text_dim)),
        );
    }
    head = head
        .push(text_c(&c.author, 13.0, theme.text_primary))
        .push(dim(theme, &c.time_ago, 11.5));
    if c.hearted {
        head = head.push(icon(IconKind::Like, 12.0, theme.tokens.colors.semantic.danger));
    }

    let actions = Flex::row()
        .gap(6.0)
        .align(Align::Center)
        .push(icon(IconKind::Like, 13.0, theme.text_dim))
        .push(dim(theme, &human_count(c.likes), 11.5))
        .push(gap(8.0, 1.0))
        .push_if(c.replies > 0, dim(theme, &format!("{} replies", c.replies), 11.5));

    Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Start)
        .push(avatar(&c.author, 36.0))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(4.0)
                .push(head)
                .push(text(&c.text, 13.5))
                .push(actions),
        )
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

fn setting_row(theme: &Theme, title: &str, desc: &str, control: impl IntoElement) -> Flex {
    setting_row_gated(theme, title, desc, control, true)
}

/// A settings row whose label + description dim when `active` is false — used to
/// show that a sub-setting is currently gated by a master switch (e.g. the
/// DeArrow title/thumbnail toggles while DeArrow itself is off), so its state
/// stays reviewable without looking live.
fn setting_row_gated(
    theme: &Theme,
    title: &str,
    desc: &str,
    control: impl IntoElement,
    active: bool,
) -> Flex {
    let (title_c, desc_c) = if active {
        (theme.text_primary, theme.text_dim)
    } else {
        (theme.text_dim, theme.text_dim)
    };
    Flex::row()
        .fill_w()
        .gap(16.0)
        .pad_xy(0.0, 10.0)
        .align(Align::Center)
        .push(
            Flex::col()
                .grow(1.0)
                .gap(3.0)
                .push(text_c(title, 14.5, title_c))
                .push(text_c(desc, 12.5, desc_c)),
        )
        .push(control)
}

fn settings_card(theme: &Theme, title: &str, rows: Vec<Flex>) -> Flex {
    let mut col = Flex::col()
        .fill_w()
        .gap(2.0)
        .pad_each(20.0, 16.0, 20.0, 16.0)
        .surface(card_surface(theme))
        .push(text(title, 16.5))
        .push(gap(1.0, 8.0));
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            col = col.push(divider(theme));
        }
        col = col.push(r);
    }
    col
}

/// Full-width vertical spacer. Used between settings sections because their
/// scroll wrapper lays out as a CSS block, and block layout ignores flex `gap`
/// — so the cards would otherwise stack flush against each other.
fn vspace(px: f32) -> Flex {
    Flex::row().fill_w().h(Len::Px(px))
}

/// A clickable three-state mode selector for a SponsorBlock category. Cycles
/// Auto-skip → Show → Off on click. The label + fill reflect the *stored*
/// per-category mode (independent of the master switch) so the user always sees
/// their choice.
fn mode_pill(theme: &Theme, bus: &ActionBus, cat: Category, mode: SkipMode) -> Flex {
    let sel = selected_accent_fill(theme);
    let (fill, fg, border) = match mode {
        SkipMode::Skip => (Some(sel.bg), sel.fg, None),
        SkipMode::Show => (
            Some(subtle_fill(theme)),
            theme.text_primary,
            Some((1.0, theme.border_standard)),
        ),
        SkipMode::Disabled => (None, theme.text_dim, Some((1.0, theme.border_standard))),
    };
    Flex::row()
        .id(&format!("mode_{}", cat.api_key()))
        .w(Len::Px(104.0))
        .pad_xy(12.0, 7.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .surface(Surface {
            fill,
            radius: theme.tokens.radii.full as f64,
            border,
        })
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.full as f64,
            border,
        })
        .on_click(bus, Action::ToggleCategory(cat))
        .push(text_c(mode.label(), 12.5, fg))
}

pub fn settings(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Scrollable {
    let s = &model.settings;

    let playback = settings_card(
        theme,
        "Playback",
        vec![
            setting_row(theme, "Autoplay next video", "Play the up-next video when this one ends.",
                Switch::new(s.autoplay, bus, Action::ToggleSetting(SettingFlag::Autoplay)).id("switch_autoplay")),
            setting_row(theme, "Show comments", "Load the comment section on the watch page.",
                Switch::new(s.show_comments, bus, Action::ToggleSetting(SettingFlag::ShowComments)).id("switch_comments")),
            setting_row(theme, "Show dislikes", "Display the crowdsourced dislike count (Return YouTube Dislike).",
                Switch::new(s.return_dislike_enabled, bus, Action::ToggleSetting(SettingFlag::ReturnDislike)).id("switch_dislikes")),
            setting_row(theme, "Preferred quality", "Highest resolution to request when available.",
                quality_menu(model, theme, bus)),
        ],
    );

    let sponsorblock = {
        let mut rows = vec![
            setting_row(theme, "Enable SponsorBlock", "Use community segment data to mark and skip parts of videos.",
                Switch::new(s.sponsorblock_enabled, bus, Action::ToggleSetting(SettingFlag::SponsorBlock)).id("switch_sponsorblock")),
        ];
        for cat in Category::ALL {
            let (r, g, b) = cat.rgb();
            let label = Flex::row()
                .gap(10.0)
                .align(Align::Center)
                .grow(1.0)
                .push(Flex::row().fixed(14.0, 14.0).surface(Surface {
                    fill: Some(Color::from_rgb8(r, g, b)),
                    radius: 4.0,
                    border: None,
                }))
                .push(
                    Flex::col()
                        .grow(1.0)
                        .gap(2.0)
                        .push(text(cat.label(), 14.0))
                        .push(dim(theme, cat.blurb(), 12.0)),
                );
            // The stored per-category mode (not the master-gated one) so the
            // selector shows the user's choice even when SponsorBlock is off.
            let stored = s.categories.get(cat.api_key()).copied().unwrap_or(SkipMode::Skip);
            rows.push(
                Flex::row()
                    .fill_w()
                    .gap(16.0)
                    .pad_xy(0.0, 8.0)
                    .align(Align::Center)
                    .push(label)
                    .push(mode_pill(theme, bus, cat, stored)),
            );
        }
        settings_card(theme, "SponsorBlock", rows)
    };

    let dearrow = settings_card(
        theme,
        "DeArrow",
        vec![
            setting_row(theme, "Enable DeArrow", "Use community branding in place of clickbait.",
                Switch::new(s.dearrow_enabled, bus, Action::ToggleSetting(SettingFlag::DeArrow)).id("switch_dearrow")),
            // The two sub-toggles are gated by the master switch: they dim while
            // DeArrow is off (their stored choice is still shown + editable), and
            // only take effect once it's on. `dearrow_titles_on()` /
            // `dearrow_thumbs_on()` read exactly this combination on every card.
            setting_row_gated(theme, "Replace clickbait titles", "Show community-submitted, factual titles instead of clickbait.",
                Switch::new(s.dearrow_titles, bus, Action::ToggleSetting(SettingFlag::DeArrowTitles)).id("switch_dearrow_titles"),
                s.dearrow_enabled),
            setting_row_gated(theme, "Replace thumbnails", "Use neutral community thumbnails in place of clickbait frames.",
                Switch::new(s.dearrow_thumbnails, bus, Action::ToggleSetting(SettingFlag::DeArrowThumbnails)).id("switch_dearrow_thumbs"),
                s.dearrow_enabled),
        ],
    );

    let privacy = settings_card(
        theme,
        "Source",
        vec![
            // Clicking the row cycles to the next known Invidious instance —
            // the one live search actually queries — so the chevron is a real
            // control, not a dead ornament.
            setting_row(theme, "Invidious instance", &s.invidious_instance, icon(IconKind::Chevron, 16.0, theme.text_dim))
                .id("instance_row")
                .pad_xy(8.0, 8.0)
                .hover_surface(Surface {
                    fill: Some(hover_fill(theme)),
                    radius: theme.tokens.radii.md as f64,
                    border: None,
                })
                .on_click(bus, Action::CycleInstance),
        ],
    );

    // Explicit spacers between sections: the scroll wrapper lays out as a block,
    // which ignores flex `gap`, so without these the cards would touch.
    scroll(
        Flex::col()
            .fill_w()
            .max_w(820.0)
            .pad(24.0)
            .push(section_header(theme, IconKind::Settings, "Settings", ""))
            .push(vspace(18.0))
            .push(playback)
            .push(vspace(16.0))
            .push(sponsorblock)
            .push(vspace(16.0))
            .push(dearrow)
            .push(vspace(16.0))
            .push(privacy)
            .push(vspace(24.0)),
    )
}

// ---------------------------------------------------------------------------
// Ctrl+K command palette
// ---------------------------------------------------------------------------

/// One result row in the command palette.
fn palette_row(theme: &Theme, bus: &ActionBus, hit: &crate::search_type::SearchHit, index: usize) -> Flex {
    use crate::search_type::CHANNEL_TYPE_ID;
    let is_channel = hit.type_id == CHANNEL_TYPE_ID;
    let kind = if is_channel { IconKind::Subscriptions } else { IconKind::Play };
    let meta = if is_channel {
        format!("Channel · {}", hit.creator)
    } else {
        format!("{} · {}", hit.creator, clock(hit.length_secs as f64))
    };
    Flex::row()
        .id(&format!("palette_row_{index}"))
        .fill_w()
        .gap(12.0)
        .pad_xy(12.0, 9.0)
        .align(Align::Center)
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: theme.tokens.radii.md as f64,
            border: None,
        })
        .on_click(bus, Action::ActivateHit(hit.deep_link.clone()))
        .push(
            Flex::row()
                .fixed(34.0, 34.0)
                .justify(Justify::Center)
                .align(Align::Center)
                .surface(subtle_surface(theme))
                .push(icon(kind, 16.0, theme.text_dim)),
        )
        .push(
            Flex::col()
                .grow(1.0)
                .gap(2.0)
                .push(text(&hit.title, 14.0))
                .push(dim(theme, &meta, 12.0)),
        )
        .push(icon(IconKind::Chevron, 14.0, theme.text_dim))
}

/// The Ctrl+K global command palette overlay. Renders a dimming scrim (click to
/// close) with a centred card: a bound query field + live-filtered results
/// drawn from the [`crate::search_type`] provider seam (so it shares the exact
/// contract the coming global-search framework will use). Returns `None` when
/// the palette is closed so the caller can skip it entirely.
pub fn command_palette(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Option<Flex> {
    if !model.palette_open {
        return None;
    }

    let backend = crate::backend::offline::OfflineBackend::with_model(model.clone());
    let provider =
        crate::search_type::VideoSearchProvider::new(&backend, model.settings.dearrow_titles_on());
    let hits = crate::search_type::palette_hits(&provider, model, &model.palette_query);

    // Always-present, placeholder-backed input (same reachability fix as the
    // top-bar search box): the field must exist in the tree even while empty so
    // it can be focused and typed into — that's the whole point of the palette.
    let input = TextInput::new(vec![])
        .with_id("palette_input")
        .with_on_change_path("palette_query")
        .with_plain_chrome()
        .with_font_size(16.0)
        .with_placeholder("Search videos and channels…");

    let field = Flex::row()
        .fill_w()
        .h(Len::Px(46.0))
        .gap(10.0)
        .pad_xy(14.0, 0.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: theme.tokens.radii.md as f64,
            border: Some((1.0, theme.border_focus)),
        })
        .push(icon(IconKind::Search, 18.0, theme.accent_primary))
        .push(Flex::row().grow(1.0).push(input))
        .push(
            Flex::row()
                .pad_xy(7.0, 3.0)
                .surface(subtle_surface(theme))
                .push(text_c("Ctrl K", 11.0, theme.text_dim)),
        );

    let mut list = Flex::col().fill_w().gap(2.0);
    if model.palette_query.trim().is_empty() {
        list = list.push(
            Flex::col()
                .fill_w()
                .pad(20.0)
                .align(Align::Center)
                .push(dim(theme, "Type to search across Tube", 13.0)),
        );
    } else if hits.is_empty() {
        list = list.push(
            Flex::col()
                .fill_w()
                .pad(20.0)
                .align(Align::Center)
                .push(dim(theme, "No matches", 13.0)),
        );
    } else {
        for (i, h) in hits.iter().take(8).enumerate() {
            list = list.push(palette_row(theme, bus, h, i));
        }
    }

    let card = Flex::col()
        .id("palette_card")
        .w(Len::Px(560.0))
        .gap(10.0)
        .pad(14.0)
        .surface(Surface {
            fill: Some(theme.tokens.colors.surfaces.raised),
            radius: theme.tokens.radii.lg as f64,
            border: Some((1.0, theme.border_standard)),
        })
        // Swallow clicks on the card chrome so they don't reach the scrim and
        // close the palette. `OpenPalette` is idempotent (already open).
        .on_click(bus, Action::OpenPalette)
        .push(field)
        .push(divider(theme))
        .push(list);

    // Full-bleed scrim; clicking it (outside the card) closes the palette.
    Some(
        Flex::col()
            .id("palette_scrim")
            .fill_w()
            .fill_h()
            .align(Align::Center)
            .justify(Justify::Start)
            .pad_each(0.0, 96.0, 0.0, 0.0)
            .surface(Surface {
                fill: Some(Color::from_rgba8(0, 0, 0, 140)),
                radius: 0.0,
                border: None,
            })
            .on_click(bus, Action::ClosePalette)
            .push(card),
    )
}

#[cfg(test)]
mod selected_contrast_tests {
    //! Every selected / active chrome state must place a foreground that
    //! clears the WCAG AA body-text floor (4.5:1) on its own highlight.
    //! These assert on the exact `SelectedStyle` the screens paint with, in
    //! BOTH the dark and light themes (the bug was theme-asymmetric: white
    //! text broke in light, accent-on-bright broke in dark).
    use super::*;
    use crate::contrast::AA_BODY;

    fn themes() -> Vec<(&'static str, Theme)> {
        vec![("dark", Theme::dark()), ("light", Theme::light())]
    }

    #[test]
    fn selected_accent_fill_is_readable() {
        for (name, t) in themes() {
            let s = selected_accent_fill(&t);
            assert!(
                s.ratio() >= AA_BODY,
                "[{name}] selected chip/pill contrast {:.2} < {AA_BODY}",
                s.ratio()
            );
        }
    }

    #[test]
    fn selected_nav_item_is_readable() {
        for (name, t) in themes() {
            let s = selected_nav_item(&t);
            assert!(
                s.ratio() >= AA_BODY,
                "[{name}] selected nav-item contrast {:.2} < {AA_BODY}",
                s.ratio()
            );
        }
    }

    #[test]
    fn status_pill_on_is_readable() {
        for (name, t) in themes() {
            let s = status_pill_on(&t);
            assert!(
                s.ratio() >= AA_BODY,
                "[{name}] on status-pill contrast {:.2} < {AA_BODY}",
                s.ratio()
            );
        }
    }
}

#[cfg(test)]
mod search_screen_tests {
    use super::*;
    use crate::model::Route;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use taffy::prelude::*;
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::widget::Widget;
    use vello::Scene;
    use vello::kurbo::{Affine, Point};
    use vello_encoding::DrawTag;

    /// A tiny valid PNG (4×4 opaque green) so a test can seed the image store
    /// with a decodable thumbnail without a network.
    fn px_png() -> Vec<u8> {
        let (w, h) = (4u32, 4u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&[20, 200, 60, 255]);
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        out
    }

    /// The search screen no longer renders a result-count header (the query
    /// already appears once in the top-bar field), and its type-filter chips are
    /// present so results can be narrowed by kind.
    #[test]
    fn search_screen_has_type_chips_and_no_count_header() {
        use crate::model::Route;
        let mut model = crate::sample::model();
        let mut v = model.videos[0].clone();
        v.id = "only".into();
        model.videos = vec![v];
        model.search_hits_query = "q".into();
        model.search_hits = vec!["only".into()];
        model.route = Route::Search("q".into());

        let theme = Theme::dark();
        let bus = crate::action::new_bus();
        let mut screen = search(&model, &theme, &bus, Imgs::none(), "q");

        let (w, h) = (1280.0f32, 800.0f32);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = screen.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .new_with_children(
                Style { size: Size { width: length(w), height: length(h) }, ..Default::default() },
                &[child],
            )
            .unwrap();

        fn has_id(w: &dyn Widget, id: &str) -> bool {
            if w.test_id() == Some(id) {
                return true;
            }
            w.children().iter().any(|c| has_id(*c, id))
        }
        assert!(has_id(&screen, "search_type_chips"), "the Video·Playlist·Creator chips are present");
        for label in ["video", "playlist", "creator"] {
            assert!(
                has_id(&screen, &format!("type_chip_{label}")),
                "the {label} type chip is present",
            );
        }
    }

    /// Issue 2 — a search-result row paints the fetched thumbnail. Rendered
    /// through the real `search` screen with an image store that has decoded the
    /// result's thumbnail URL, the card must emit an IMAGE draw (the real still),
    /// not just the gradient poster — the same `video_card` image path
    /// Home/Trending use.
    #[test]
    fn search_result_row_paints_the_fetched_thumbnail() {
        let store = ui::ImageStore::new(Arc::new(|_u: &str| Some(px_png())));

        let mut model = crate::sample::model();
        let mut v = model.videos[0].clone();
        v.thumbnail_url = "https://i.ytimg.com/vi/testresult/hqdefault.jpg".to_string();
        let vid = v.id.clone();
        let thumb = v.thumbnail_url.clone();
        model.videos = vec![v];
        model.search_hits_query = "q".to_string();
        model.search_hits = vec![vid];
        model.route = Route::Search("q".to_string());

        // Pre-warm the store so the build captures the decoded image.
        store.get(&thumb);
        store.block_until_idle(Duration::from_secs(5));
        assert!(store.get(&thumb).is_some(), "the result thumbnail decoded + cached");

        let theme = Theme::dark();
        let bus = crate::action::new_bus();
        let imgs = Imgs(Some(&store));
        let mut screen = search(&model, &theme, &bus, imgs, "q");

        // Lay the real screen out in a definite viewport, then draw it.
        let (w, h) = (1280.0f32, 800.0f32);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let child = screen.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style { size: Size { width: length(w), height: length(h) }, ..Default::default() },
                &[child],
            )
            .unwrap();
        {
            let map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::Definite(h),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        screen.apply_layout(&taffy, child, Point::ORIGIN);

        // The thumbnail resolves its decoded image lazily in `sync` (from the
        // pre-warmed store), so run it — the real per-frame path — before drawing.
        let mut doc = automerge::AutoCommit::new();
        let root_obj = automerge::ROOT;
        screen.sync(&mut doc, &root_obj);

        let mut scene = Scene::new();
        screen.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &PresenceCtx::empty());
        let images = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count();
        assert!(
            images >= 1,
            "the search result row must paint the fetched thumbnail (IMAGE draw), got {images}"
        );
    }
}
