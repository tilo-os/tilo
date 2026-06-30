//! Screen composition. Each `fn` here takes the immutable [`TubeModel`],
//! the live [`Theme`] and the [`ActionBus`] and returns a widget subtree
//! built entirely from `ui` widgets + Tube's app-local primitives. There is
//! no per-screen state: navigation rebuilds the whole tree from the model,
//! so these functions are pure views.

use ui::theme::Theme;
use ui::widget::{Avatar, ScrollAxis, Scrollable, TextInput};
use vello::peniko::Color;

use crate::action::{Action, ActionBus, SettingFlag};
use crate::model::{Category, Route, TubeModel, Video};
use crate::util::{clock, human_count, human_views};
use crate::widgets::{
    card_surface, chip_surface, dim, divider, gap, spring, text, text_c, Align, Flex, Icon,
    IconKind, IntoElement, Justify, Len, SeekBar, Surface, Switch, Thumbnail,
};

const CARD_W: f32 = 300.0;
const THUMB_H: f32 = 169.0;

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
    let (bg, fg, border) = if filled {
        (Some(theme.accent_primary), Color::WHITE, None)
    } else {
        (
            Some(theme.tokens.colors.neutral.stops[7]),
            theme.text_primary,
            Some((1.0, theme.border_standard)),
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
            fill: Some(if filled {
                theme.accent_hover
            } else {
                theme.tokens.colors.neutral.stops[6]
            }),
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
        .surface(Surface {
            fill: Some(theme.tokens.colors.neutral.stops[8]),
            radius: theme.tokens.radii.full as f64,
            border: None,
        })
        .hover_surface(Surface {
            fill: Some(theme.tokens.colors.neutral.stops[6]),
            radius: theme.tokens.radii.full as f64,
            border: None,
        })
        .on_click(bus, action)
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

fn video_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, v: &Video) -> Flex {
    let dearrow = model.settings.dearrow_enabled;
    let active_dearrow = v.dearrow_active(dearrow);
    let progress = model.history.first().filter(|h| **h == v.id).map(|_| 0.35);
    let mut thumb = Thumbnail::new(&v.seed, v.duration)
        .height(THUMB_H)
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
            fill: Some(theme.tokens.colors.neutral.stops[8]),
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
                .push(avatar(&v.channel_name, 36.0))
                .push(meta),
        )
}

/// Compact horizontal card for the "Up next" rail + history rows.
fn compact_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, v: &Video) -> Flex {
    let dearrow = model.settings.dearrow_enabled;
    Flex::row()
        .fill_w()
        .gap(10.0)
        .pad(6.0)
        .align(Align::Start)
        .hover_surface(Surface {
            fill: Some(theme.tokens.colors.neutral.stops[8]),
            radius: theme.tokens.radii.md as f64,
            border: None,
        })
        .on_click(bus, Action::OpenVideo(v.id.clone()))
        .push(
            Flex::col()
                .w(Len::Px(160.0))
                .push(Thumbnail::new(&v.seed, v.duration).height(90.0).live(v.live)),
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
    let fg = if active { theme.accent_primary } else { theme.text_dim };
    let surface = if active {
        Surface {
            fill: Some(theme.tokens.colors.neutral.stops[8]),
            radius: theme.tokens.radii.lg as f64,
            border: None,
        }
    } else {
        Surface { fill: None, radius: theme.tokens.radii.lg as f64, border: None }
    };
    Flex::col()
        .fill_w()
        .gap(4.0)
        .pad_xy(4.0, 8.0)
        .align(Align::Center)
        .justify(Justify::Center)
        .surface(surface)
        .hover_surface(Surface {
            fill: Some(theme.tokens.colors.neutral.stops[8]),
            radius: theme.tokens.radii.lg as f64,
            border: None,
        })
        .on_click(bus, Action::Navigate(route))
        .push(icon(kind, 22.0, fg))
        .push(text_c(label, 11.0, fg))
}

pub fn nav_rail(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let sec = model.route.nav_section();
    Flex::col()
        .w(Len::Px(86.0))
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
    // document (survives navigation rebuilds) and would sync to peers.
    let search_input = TextInput::new(vec![])
        .with_id("search_input")
        .with_on_change_path("search_query")
        .with_plain_chrome()
        .with_font_size(15.0);

    let search = Flex::row()
        .grow(1.0)
        .max_w(560.0)
        .h(Len::Px(42.0))
        .gap(8.0)
        .pad_xy(14.0, 0.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(theme.tokens.colors.neutral.stops[8]),
            radius: theme.tokens.radii.full as f64,
            border: Some((1.0, theme.border_standard)),
        })
        .push(icon(IconKind::Search, 18.0, theme.text_dim))
        .push(
            Flex::row()
                .grow(1.0)
                .push_if(
                    model.search_query.is_empty(),
                    dim(theme, "Search privacy-respecting videos", 14.0),
                )
                .push_if(!model.search_query.is_empty(), search_input),
        )
        .push(icon_button(theme, bus, IconKind::Search, Action::SubmitSearch));

    let status = Flex::row()
        .gap(8.0)
        .align(Align::Center)
        .push(status_pill(theme, IconKind::Shield, "SponsorBlock", model.settings.sponsorblock_enabled))
        .push(status_pill(theme, IconKind::Sparkle, "DeArrow", model.settings.dearrow_enabled));

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
        .push(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .push(text_c("Tube", 22.0, theme.text_primary))
                .push(
                    Flex::row()
                        .pad_xy(7.0, 3.0)
                        .surface(Surface {
                            fill: Some(theme.tokens.colors.semantic.success.with_alpha(0.18)),
                            radius: theme.tokens.radii.full as f64,
                            border: None,
                        })
                        .push(text_c("no ads", 11.0, theme.tokens.colors.semantic.success)),
                ),
        )
        .push(search)
        .push(status)
        .push(avatar("You", 36.0))
}

fn status_pill(theme: &Theme, kind: IconKind, label: &str, on: bool) -> Flex {
    let (fg, bg) = if on {
        (theme.accent_primary, theme.tokens.colors.accent.stops[1].with_alpha(0.4))
    } else {
        (theme.text_dim, theme.tokens.colors.neutral.stops[8])
    };
    Flex::row()
        .gap(6.0)
        .pad_xy(10.0, 6.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(bg),
            radius: theme.tokens.radii.full as f64,
            border: None,
        })
        .push(icon(kind, 14.0, fg))
        .push(text_c(label, 12.0, fg))
}

// ---------------------------------------------------------------------------
// chips
// ---------------------------------------------------------------------------

fn chip(theme: &Theme, bus: &ActionBus, label: &str, action: Action, active: bool) -> Flex {
    let fg = if active { Color::WHITE } else { theme.text_primary };
    Flex::row()
        .pad_xy(14.0, 7.0)
        .surface(chip_surface(theme, active))
        .hover_surface(Surface {
            fill: Some(if active {
                theme.accent_hover
            } else {
                theme.tokens.colors.neutral.stops[6]
            }),
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

pub fn home(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Scrollable {
    let cards: Vec<Flex> = model.videos.iter().map(|v| video_card(model, theme, bus, v)).collect();
    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(chip_bar(model, theme, bus))
            .push(section_header(theme, IconKind::Fire, "Trending now", "Reordered by you — no algorithm tricks"))
            .push(grid(cards)),
    )
}

pub fn trending(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Scrollable {
    let mut vids: Vec<&Video> = model.videos.iter().collect();
    vids.sort_by(|a, b| b.views.cmp(&a.views));
    let cards: Vec<Flex> = vids.iter().map(|v| video_card(model, theme, bus, v)).collect();
    scroll(
        Flex::col()
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .push(section_header(theme, IconKind::Fire, "Trending", "Most-watched across your instance"))
            .push(grid(cards)),
    )
}

pub fn subscriptions(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Scrollable {
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
                        fill: Some(theme.tokens.colors.neutral.stops[8]),
                        radius: theme.tokens.radii.lg as f64,
                        border: None,
                    })
                    .on_click(bus, Action::OpenChannel(ch.id.clone()))
                    .push(avatar(&ch.name, 56.0))
                    .push(text_c(&ch.name, 11.5, theme.text_primary)),
            );
        }
    }

    let feed = model.subscription_feed();
    let cards: Vec<Flex> = feed.iter().map(|v| video_card(model, theme, bus, v)).collect();
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

pub fn history(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Scrollable {
    let vids = model.history_videos();
    let mut col = Flex::col().fill_w().gap(8.0);
    if vids.is_empty() {
        col = col.push(empty_state(theme, "Nothing watched yet", "Videos you watch show up here."));
    }
    for v in vids {
        col = col.push(compact_card(model, theme, bus, v));
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

pub fn search(model: &TubeModel, theme: &Theme, bus: &ActionBus, query: &str) -> Scrollable {
    let results = model.search(query);
    let cards: Vec<Flex> = results.iter().map(|v| video_card(model, theme, bus, v)).collect();
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

pub fn channel(model: &TubeModel, theme: &Theme, bus: &ActionBus, id: &str) -> Scrollable {
    let Some(ch) = model.channel(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Channel not found", "")));
    };
    let subscribed = model.is_subscribed(id);
    let (g0, _g1) = crate::util::seed_gradient(&ch.seed);

    let banner = Flex::row().fill_w().h(Len::Px(120.0)).surface(Surface {
        fill: Some(g0),
        radius: theme.tokens.radii.lg as f64,
        border: None,
    });

    let header = Flex::row()
        .fill_w()
        .gap(18.0)
        .align(Align::Center)
        .push(avatar(&ch.name, 86.0))
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
        .map(|v| video_card(model, theme, bus, v))
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

pub fn watch(model: &TubeModel, theme: &Theme, bus: &ActionBus, id: &str) -> Scrollable {
    let Some(v) = model.video(id) else {
        return scroll(Flex::col().pad(24.0).push(empty_state(theme, "Video unavailable", "")));
    };
    let dearrow = model.settings.dearrow_enabled;

    // Player surface (poster). Real H.264 playback feeds a ui::VideoPlayer
    // here via the live worker — see playback.rs.
    let player = Flex::col()
        .fill_w()
        .push(Thumbnail::new(&v.seed, v.duration).height(430.0).hero().live(v.live));

    let enabled: Vec<bool> = v
        .sponsor
        .iter()
        .map(|s| model.settings.category_enabled(s.category))
        .collect();
    let seek = SeekBar::new(bus, v.duration as f64, model.playhead, v.sponsor.clone(), enabled);
    let transport = Flex::row()
        .fill_w()
        .gap(14.0)
        .align(Align::Center)
        .push(icon_button(
            theme,
            bus,
            if model.playing { IconKind::Pause } else { IconKind::Play },
            Action::TogglePlay,
        ))
        .push(text_c(
            &format!("{} / {}", clock(model.playhead), clock(v.duration as f64)),
            13.0,
            theme.text_dim,
        ))
        .push(spring())
        .push(quality_menu(model, theme, bus))
        .push(icon(IconKind::Settings, 18.0, theme.text_dim));

    let title = text(v.display_title(dearrow), 22.0);
    let meta = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(dim(theme, &format!("{} • {}", human_views(v.views), v.published), 13.0))
        .push(spring())
        .push(pill(theme, bus, &human_count(v.likes), Some(IconKind::Like), Action::Like, false))
        .push(pill(theme, bus, "Share", Some(IconKind::Chevron), Action::Like, false));

    let ch_sub = model.is_subscribed(&v.channel_id);
    let channel_row = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(avatar(&v.channel_name, 48.0))
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
        .push(if ch_sub {
            pill(theme, bus, "Subscribed", Some(IconKind::Bell), Action::ToggleSubscribe(v.channel_id.clone()), false)
        } else {
            pill(theme, bus, "Subscribe", None, Action::ToggleSubscribe(v.channel_id.clone()), true)
        });

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
        .grow(1.0)
        .gap(14.0)
        .push(player)
        .push(Flex::col().fill_w().gap(8.0).push(seek).push(transport))
        .push(title)
        .push(meta)
        .push(divider(theme))
        .push(channel_row)
        .push(sb_card)
        .push(desc)
        .push(divider(theme))
        .push(comments);

    let mut up = Flex::col().w(Len::Px(360.0)).gap(10.0);
    up = up.push(
        Flex::row()
            .fill_w()
            .align(Align::Center)
            .gap(10.0)
            .push(text("Up next", 16.0))
            .push(spring())
            .push(autoplay_toggle(model, theme, bus)),
    );
    for r in model.related(id).into_iter().take(6) {
        up = up.push(compact_card(model, theme, bus, r));
    }

    scroll(
        Flex::col().fill_w().pad(20.0).push(
            Flex::row()
                .fill_w()
                .gap(24.0)
                .align(Align::Start)
                .push(left)
                .push(up),
        ),
    )
}

fn quality_menu(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    Flex::row()
        .gap(6.0)
        .pad_xy(10.0, 6.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(theme.tokens.colors.neutral.stops[8]),
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
                        fill: Some(theme.tokens.colors.neutral.stops[8]),
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

fn comment_row(theme: &Theme, c: &crate::model::Comment) -> Flex {
    let mut head = Flex::row().gap(8.0).align(Align::Center);
    if c.pinned {
        head = head.push(
            Flex::row()
                .pad_xy(6.0, 2.0)
                .surface(Surface {
                    fill: Some(theme.tokens.colors.neutral.stops[8]),
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
                Switch::new(s.autoplay, bus, Action::ToggleSetting(SettingFlag::Autoplay))),
            setting_row(theme, "Show comments", "Load the comment section on the watch page.",
                Switch::new(s.show_comments, bus, Action::ToggleSetting(SettingFlag::ShowComments))),
            setting_row(theme, "Preferred quality", "Highest resolution to request when available.",
                quality_menu(model, theme, bus)),
        ],
    );

    let sponsorblock = {
        let mut rows = vec![
            setting_row(theme, "Enable SponsorBlock", "Use community segment data to mark and skip parts of videos.",
                Switch::new(s.sponsorblock_enabled, bus, Action::ToggleSetting(SettingFlag::SponsorBlock))),
            setting_row(theme, "Auto-skip", "Skip enabled categories automatically instead of just marking them.",
                Switch::new(s.auto_skip, bus, Action::ToggleSetting(SettingFlag::AutoSkip))),
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
                Switch::new(s.dearrow_enabled, bus, Action::ToggleSetting(SettingFlag::DeArrow))),
            setting_row(theme, "Replace thumbnails", "Use neutral community thumbnails in place of clickbait frames.",
                Switch::new(s.dearrow_thumbnails, bus, Action::ToggleSetting(SettingFlag::DeArrowThumbnails))),
        ],
    );

    let privacy = settings_card(
        theme,
        "Privacy & source",
        vec![
            setting_row(theme, "Invidious instance", &s.invidious_instance, icon(IconKind::Chevron, 16.0, theme.text_dim)),
            setting_row(theme, "No ads, no tracking", "Tube never loads Google's player, cookies or ad SDKs.",
                icon(IconKind::Shield, 18.0, theme.tokens.colors.semantic.success)),
        ],
    );

    scroll(
        Flex::col()
            .fill_w()
            .max_w(820.0)
            .pad(24.0)
            .gap(18.0)
            .push(section_header(theme, IconKind::Settings, "Settings", "Stored in the document CRDT"))
            .push(playback)
            .push(sponsorblock)
            .push(dearrow)
            .push(privacy),
    )
}
