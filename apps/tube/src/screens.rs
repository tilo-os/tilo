//! Screen composition. Each `fn` here takes the immutable [`TubeModel`],
//! the live [`Theme`] and the [`ActionBus`] and returns a widget subtree
//! built entirely from `ui` widgets + Tube's app-local primitives. There is
//! no per-screen state: navigation rebuilds the whole tree from the model,
//! so these functions are pure views.

use ui::theme::Theme;
use ui::widget::{Avatar, ScrollAxis, Scrollable, TextInput};
use vello::peniko::Color;

use crate::action::{Action, ActionBus, SettingFlag};
use crate::images::Imgs;
use crate::model::{Category, Route, TubeModel, Video};
use crate::util::{clock, human_count, human_views};
use crate::widgets::{
    card_surface, chip_surface, dim, divider, gap, hover_fill, hover_pill_surface, spring,
    subtle_fill, subtle_surface, text, text_c, Align, Flex, Icon, IconKind, ImageBox, IntoElement,
    Justify, Len, PlayerHost, SharedPlayer, Surface, Switch, Thumbnail,
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
/// Cap on the 16:9 player height so it never dominates a tall, wide window.
const PLAYER_MAX_H: f64 = 480.0;

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

fn initials(name: &str) -> String {
    let mut out = String::new();
    for word in name.split_whitespace().take(2) {
        if let Some(c) = word.chars().next() {
            out.push(c.to_ascii_uppercase());
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

fn avatar(name: &str, size: f32) -> Avatar {
    Avatar::from_initials(&initials(name), size)
}

/// A channel avatar that paints the real decoded avatar image when the store
/// has it, else the deterministic initials chip. Looks the URL up on the
/// channel record so any screen can render a real uploader photo.
fn channel_avatar(imgs: Imgs, model: &TubeModel, channel_id: &str, name: &str, size: f32) -> Avatar {
    let url = model.channel(channel_id).map(|c| c.avatar_url.as_str()).unwrap_or("");
    match imgs.get(url) {
        Some(img) => Avatar::new(img, size),
        None => Avatar::from_initials(&initials(name), size),
    }
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
    let dearrow = model.settings.dearrow_enabled;
    let active_dearrow = v.dearrow_active(dearrow);
    let progress = model.history.first().filter(|h| **h == v.id).map(|_| 0.35);
    let mut thumb = Thumbnail::new(&v.seed, v.duration)
        .height(THUMB_H)
        .image(imgs.get(model.display_thumbnail_url(v)))
        .dearrow(active_dearrow && model.settings.dearrow_thumbnails)
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

    Flex::col()
        .w(Len::Px(CARD_W))
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
    let dearrow = model.settings.dearrow_enabled;
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
                    .image(imgs.get(model.display_thumbnail_url(v)))
                    .live(v.live),
            ),
        )
        .push(
            Flex::col()
                .grow(1.0)
                .gap(4.0)
                .push(text(v.display_title(dearrow), 13.0))
                .push(dim(theme, &v.channel_name, 12.0))
                .push(dim(
                    theme,
                    format!("{} • {}", human_count(v.views), v.published),
                    11.5,
                )),
        )
}

fn grid(cards: Vec<Flex>) -> Flex {
    let mut row = Flex::row().fill_w().wrap().gap(18.0).align(Align::Start);
    for c in cards {
        row = row.push(c);
    }
    row
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

pub fn top_bar(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
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

    let search = Flex::row()
        .grow(1.0)
        .max_w(560.0)
        .h(Len::Px(42.0))
        .gap(8.0)
        .pad_xy(14.0, 0.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: theme.tokens.radii.full as f64,
            border: Some((1.0, theme.border_standard)),
        })
        .push(icon(IconKind::Search, 18.0, theme.text_dim))
        .push(Flex::row().grow(1.0).push(search_input))
        .push(icon_button(theme, bus, IconKind::Search, Action::SubmitSearch));

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
        .push(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .push(text_c("Tube", 22.0, theme.text_primary)),
        )
        .push(search)
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

fn chip_bar(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let mut row = Flex::row().fill_w().wrap().gap(8.0);
    let on_home = matches!(model.route, Route::Home);
    row = row.push(chip(theme, bus, "All", Action::Navigate(Route::Home), on_home));
    for cat in model.categories() {
        let active = matches!(&model.route, Route::Search(q) if *q == cat);
        row = row.push(chip(theme, bus, &cat, Action::Navigate(Route::Search(cat.clone())), active));
    }
    row
}

// ---------------------------------------------------------------------------
// screens
// ---------------------------------------------------------------------------

fn scroll(content: Flex) -> Scrollable {
    Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(content.block()))
}

pub fn home(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let cards: Vec<Flex> = model.videos.iter().map(|v| video_card(model, theme, bus, imgs, v)).collect();
    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(chip_bar(model, theme, bus))
            .push(section_header(theme, IconKind::Fire, "Trending", ""))
            .push(grid(cards)),
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

pub fn search(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, query: &str) -> Scrollable {
    let results = model.search_display(query);
    let cards: Vec<Flex> = results.iter().map(|v| video_card(model, theme, bus, imgs, v)).collect();
    let header = format!("{} results", cards.len());
    let col = Flex::col()
        .fill_w()
        .pad(24.0)
        .gap(18.0)
        .push(
            Flex::row()
                .fill_w()
                .gap(10.0)
                .align(Align::Center)
                .push(icon(IconKind::Search, 20.0, theme.accent_primary))
                .push(text(if query.is_empty() { "All videos" } else { query }, 21.0))
                .push(spring())
                .push(dim(theme, &header, 13.0)),
        )
        .push(chip_bar(model, theme, bus))
        .push(if cards.is_empty() {
            empty_state(theme, "No matches", "Try a different search.")
        } else {
            grid(cards)
        });
    scroll(col)
}

pub fn channel(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, id: &str) -> Scrollable {
    let Some(ch) = model.channel(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Channel not found", "")));
    };
    let subscribed = model.is_subscribed(id);

    // Real banner image when the store has it, else the seed gradient.
    let banner = ImageBox::new(
        &ch.seed,
        160.0,
        theme.tokens.radii.lg as f64,
        imgs.get(&ch.banner_url),
    );

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
                .push(dim(theme, &format!("{}  •  {} subscribers", ch.handle, human_count(ch.subscribers)), 13.5))
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
    player: SharedPlayer,
) -> Scrollable {
    let Some(v) = model.video(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Video unavailable", "")));
    };
    let dearrow = model.settings.dearrow_enabled;

    // Responsive watch geometry. Below `WATCH_TWO_COL_MIN` the "Up next" rail
    // stacks under the player instead of sitting beside it, so neither column
    // is squeezed on a narrow window. The player is a 16:9 surface derived from
    // its column's width and capped so it never dominates a tall window.
    let inner = (content_w - WATCH_PAD * 2.0).max(0.0);
    let two_col = inner >= WATCH_TWO_COL_MIN;
    let player_col_w = if two_col {
        (inner - WATCH_COL_GAP - UP_NEXT_W).max(0.0)
    } else {
        inner
    };
    let player_h = ((player_col_w * 9.0 / 16.0).min(PLAYER_MAX_H)).max(1.0) as f32;

    // The player surface is the app's persistent, shared `ui::widget::VideoPlayer`
    // (real GStreamer decode + audio under `--features media`, a synthesized frame
    // otherwise) mounted here via a cheap `PlayerHost`, so it survives the
    // whole-tree rebuild without restarting the video. Its overlaid controls bar
    // (play/pause, scrub, volume, fullscreen) is the transport. Height tracks the
    // column width at a 16:9 ratio.
    let player_view = Flex::col()
        .fill_w()
        .push(PlayerHost::new(player, player_h));

    // A compact row under the player for app-level playback affordances that the
    // shared player doesn't own: the preferred-quality selector and a gear that
    // opens Settings (playback / SponsorBlock / DeArrow / source).
    let player_actions = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(quality_menu(model, theme, bus))
        .push(spring())
        .push(
            icon_button(theme, bus, IconKind::Settings, Action::Navigate(Route::Settings))
                .id("watch_settings"),
        );

    let title = text(v.display_title(dearrow), 22.0);
    let liked = model.is_liked(&v.id);
    let meta = Flex::row()
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
        )
        .push(
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
                .push(dim(
                    theme,
                    &model
                        .channel(&v.channel_id)
                        .map(|c| format!("{} subscribers", human_count(c.subscribers)))
                        .unwrap_or_default(),
                    12.0,
                )),
        )
        .push(
            if ch_sub {
                pill(theme, bus, "Subscribed", Some(IconKind::Bell), Action::ToggleSubscribe(v.channel_id.clone()), false)
            } else {
                pill(theme, bus, "Subscribe", None, Action::ToggleSubscribe(v.channel_id.clone()), true)
            }
            .id("subscribe_btn"),
        );

    let sb_card = sponsor_summary(model, theme, bus, v);

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

    let left = Flex::col()
        .gap(14.0)
        .push(player_view)
        .push(player_actions)
        .push_if(!model.notices.is_empty(), notices_view(theme, model))
        .push(title)
        .push(meta)
        .push(divider(theme))
        .push(channel_row)
        .push(sb_card)
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

    // Wide: player column grows beside a fixed-width "Up next" rail. Narrow:
    // the rail stacks below the player so both keep a readable width.
    let body = if two_col {
        Flex::row()
            .fill_w()
            .gap(WATCH_COL_GAP as f32)
            .align(Align::Start)
            .push(left.grow(1.0))
            .push(up.w(Len::Px(UP_NEXT_W as f32)))
    } else {
        Flex::col()
            .fill_w()
            .gap(WATCH_COL_GAP as f32)
            .push(left.fill_w())
            .push(up)
    };

    scroll(Flex::col().fill_w().pad(WATCH_PAD as f32).push(body))
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

fn sponsor_summary(model: &TubeModel, theme: &Theme, bus: &ActionBus, v: &Video) -> Flex {
    let mut col = Flex::col().fill_w().gap(10.0).pad(14.0).surface(card_surface(theme));
    col = col.push(
        Flex::row()
            .fill_w()
            .gap(8.0)
            .align(Align::Center)
            .push(icon(IconKind::Shield, 18.0, theme.accent_primary))
            .push(text("SponsorBlock", 15.0))
            .push(spring())
            .push(dim(
                theme,
                if model.settings.auto_skip { "Auto-skip on" } else { "Manual" },
                12.0,
            ))
            .push(Switch::new(
                model.settings.sponsorblock_enabled,
                bus,
                Action::ToggleSetting(SettingFlag::SponsorBlock),
            )),
    );
    if v.sponsor.is_empty() {
        col = col.push(dim(theme, "No community segments for this video yet.", 12.5));
    } else {
        let mut legend = Flex::row().fill_w().wrap().gap(8.0);
        for s in &v.sponsor {
            let (r, g, b) = s.category.rgb();
            let on = model.settings.category_enabled(s.category);
            legend = legend.push(
                Flex::row()
                    .gap(6.0)
                    .pad_xy(9.0, 5.0)
                    .align(Align::Center)
                    .surface(Surface {
                        fill: Some(subtle_fill(theme)),
                        radius: theme.tokens.radii.full as f64,
                        border: None,
                    })
                    .on_click(bus, Action::ToggleCategory(s.category))
                    .push(Flex::row().fixed(10.0, 10.0).surface(Surface {
                        fill: Some(Color::from_rgba8(r, g, b, if on { 255 } else { 90 })),
                        radius: 5.0,
                        border: None,
                    }))
                    .push(text_c(
                        &format!("{} · {}", s.category.label(), clock(s.start)),
                        12.0,
                        if on { theme.text_primary } else { theme.text_dim },
                    )),
            );
        }
        col = col.push(legend);
    }
    col
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
    Flex::row()
        .fill_w()
        .gap(16.0)
        .pad_xy(0.0, 8.0)
        .align(Align::Center)
        .push(
            Flex::col()
                .grow(1.0)
                .gap(2.0)
                .push(text(title, 14.5))
                .push(dim(theme, desc, 12.5)),
        )
        .push(control)
}

fn settings_card(theme: &Theme, title: &str, rows: Vec<Flex>) -> Flex {
    let mut col = Flex::col()
        .fill_w()
        .gap(2.0)
        .pad(18.0)
        .surface(card_surface(theme))
        .push(text(title, 16.0))
        .push(gap(1.0, 6.0));
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            col = col.push(divider(theme));
        }
        col = col.push(r);
    }
    col
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
            setting_row(theme, "Preferred quality", "Highest resolution to request when available.",
                quality_menu(model, theme, bus)),
        ],
    );

    let sponsorblock = {
        let mut rows = vec![
            setting_row(theme, "Enable SponsorBlock", "Use community segment data to mark and skip parts of videos.",
                Switch::new(s.sponsorblock_enabled, bus, Action::ToggleSetting(SettingFlag::SponsorBlock)).id("switch_sponsorblock")),
            setting_row(theme, "Auto-skip", "Skip enabled categories automatically instead of just marking them.",
                Switch::new(s.auto_skip, bus, Action::ToggleSetting(SettingFlag::AutoSkip)).id("switch_autoskip")),
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
            rows.push(
                Flex::row()
                    .fill_w()
                    .gap(16.0)
                    .pad_xy(0.0, 8.0)
                    .align(Align::Center)
                    .push(label)
                    .push(Switch::new(
                        *s.categories.get(cat.api_key()).unwrap_or(&true),
                        bus,
                        Action::ToggleCategory(cat),
                    )),
            );
        }
        settings_card(theme, "SponsorBlock", rows)
    };

    let dearrow = settings_card(
        theme,
        "DeArrow",
        vec![
            setting_row(theme, "Replace clickbait titles", "Show community-submitted, factual titles instead of clickbait.",
                Switch::new(s.dearrow_enabled, bus, Action::ToggleSetting(SettingFlag::DeArrow)).id("switch_dearrow")),
            setting_row(theme, "Replace thumbnails", "Use neutral community thumbnails in place of clickbait frames.",
                Switch::new(s.dearrow_thumbnails, bus, Action::ToggleSetting(SettingFlag::DeArrowThumbnails)).id("switch_dearrow_thumbs")),
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

    scroll(
        Flex::col()
            .fill_w()
            .max_w(820.0)
            .pad(24.0)
            .gap(18.0)
            .push(section_header(theme, IconKind::Settings, "Settings", ""))
            .push(playback)
            .push(sponsorblock)
            .push(dearrow)
            .push(privacy),
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
        crate::search_type::VideoSearchProvider::new(&backend, model.settings.dearrow_enabled);
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
