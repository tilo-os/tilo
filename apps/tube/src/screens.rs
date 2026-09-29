//! Screen composition. Each `fn` here takes the immutable [`TubeModel`],
//! the live [`Theme`] and the [`ActionBus`] and returns a widget subtree
//! built entirely from `ui` widgets + Tube's app-local primitives. There is
//! no per-screen state: navigation rebuilds the whole tree from the model,
//! so these functions are pure views.

use std::sync::{Arc, Mutex};
use ui::icons::Icon;
use ui::theme::Theme;
use ui::time::UNIX_EPOCH;
use ui::widget::{IconWidget, ScrollAxis, Scrollable, TextInput};
use vello::peniko::Color;

use crate::action::{Action, ActionBus, SettingFlag};
use crate::images::Imgs;
use crate::model::{Category, Channel, Playlist, Route, SearchType, SkipMode, TubeModel, Video};
use crate::util::{clock, human_count, human_views};
use crate::widgets::{
    Align, ChannelAvatar, Flex, ImageBox, IntoElement, Justify, LargeVideoMeta, Len, PlayerHost,
    SharedPlayer, Surface, Switch, Thumbnail, card_surface, chip_surface, dim, divider, divider_on,
    gap, hover_fill, hover_pill_surface, spring, subtle_edge, subtle_fill, subtle_surface, text,
    text_c,
};

use crate::contrast::SelectedStyle;

/// Gap and padding values on the 4 px scale read `theme.tokens.spacing`
/// (`s1` 4, `s2` 8, `s3` 12, `s4` 16, `s6` 24, `s12` 48) and control sizes on
/// a HIG metric read `theme.tokens.metrics`; the values below are the
/// authored off-scale ones this app keeps (judged by outcome in review, see
/// `docs/superpowers/audits/2026-09-16-ui-alignment-review/a8-media.md` T3).
/// Pill vertical padding (14 px label → 40 px capsule with its 22 px line).
const PILL_PAD_Y: f32 = 9.0;
/// Video-card body and avatar/text gaps.
const CARD_BODY_GAP: f32 = 9.0;
const CARD_AVATAR_GAP: f32 = 10.0;
/// Card grid gutter; the section heading's icon/title gap.
const GRID_GAP: f32 = 18.0;
const HEADING_GAP: f32 = 10.0;
/// Chip and status-pill insets.
const CHIP_PAD_X: f32 = 14.0;
const CHIP_PAD_Y: f32 = 7.0;
const STATUS_PILL_PAD_X: f32 = 10.0;
const STATUS_PILL_PAD_Y: f32 = 6.0;
/// Top bar: 66 px bar with a 20 px side inset holding a 42 px search field
/// (lens/field gap 10).
const TOP_BAR_H: f32 = 66.0;
const TOP_BAR_PAD_X: f32 = 20.0;
const SEARCH_FIELD_H: f32 = 42.0;
const SEARCH_FIELD_GAP: f32 = 10.0;
const CARD_W: f32 = 300.0;
const THUMB_H: f32 = 169.0;
/// Inner padding of a grid card (thumbnail and meta inset from the hover
/// surface). Section headings share it so heading and card content align.
const CARD_PAD: f32 = 6.0;

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

/// "On" status pill (`SponsorBlock` / `DeArrow` indicators in the top bar). The
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
fn channel_avatar(
    imgs: Imgs,
    model: &TubeModel,
    channel_id: &str,
    name: &str,
    size: f32,
) -> ChannelAvatar {
    let url = model
        .channel(channel_id)
        .map(|c| c.avatar_url.clone())
        .unwrap_or_default();
    // Resolve the photo lazily from the store (folds in on a redraw, no
    // structural rebuild); shows the initials chip until it decodes.
    ChannelAvatar::new(imgs.store_handle(), &url, name, size)
}

fn icon(kind: Icon, size: f32, color: Color) -> IconWidget {
    IconWidget::new(kind, size).with_color(color)
}

/// A pill button: optional leading icon + label, filled (accent) or
/// outlined, that pushes `action` on click.
fn pill(
    theme: &Theme,
    bus: &ActionBus,
    label: &str,
    leading: Option<Icon>,
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
            Some((1.0, subtle_edge(theme))),
            hover_fill(theme),
        )
    };
    let sp = &theme.tokens.spacing;
    let mut row = Flex::row()
        .gap(sp.s2)
        .pad_xy(sp.s4, PILL_PAD_Y)
        .align(Align::Center);
    if let Some(k) = leading {
        row = row.push(icon(k, 16.0, fg));
    }
    row.push(text_c(label, 14.0, fg))
        .surface(Surface {
            fill: bg,
            radius: f64::from(theme.tokens.radii.full),
            border,
        })
        .hover_surface(Surface {
            fill: Some(hover_bg),
            radius: f64::from(theme.tokens.radii.full),
            border,
        })
        .on_click(bus, action)
        .accessible_name(label)
}

/// A non-interactive stat pill (leading icon + label) — the outlined-pill look
/// without a click handler. Used for the read-only dislike count.
fn stat_pill(theme: &Theme, label: &str, leading: Icon) -> Flex {
    let sp = &theme.tokens.spacing;
    Flex::row()
        .gap(sp.s2)
        .pad_xy(sp.s4, PILL_PAD_Y)
        .align(Align::Center)
        .surface(subtle_surface(theme))
        .push(icon(leading, 16.0, theme.text_primary))
        .push(text_c(label, 14.0, theme.text_primary))
}

/// A small icon-only round button.
fn icon_button(theme: &Theme, bus: &ActionBus, kind: Icon, action: Action) -> Flex {
    let label = action.accessible_label();
    let m = &theme.tokens.metrics;
    Flex::row()
        .fixed(m.bar_height, m.bar_height)
        .justify(Justify::Center)
        .align(Align::Center)
        .push(icon(kind, m.icon_md, theme.text_primary))
        .surface(subtle_surface(theme))
        .hover_surface(hover_pill_surface(theme))
        .on_click(bus, action)
        .accessible_name(label)
}

/// The hamburger in the top bar. Toggles the navigation rail — a no-op-looking
/// affordance on a wide window (the rail is already docked) that becomes the
/// way to summon the rail once it has collapsed into an overlay flap on a
/// narrow one.
fn menu_button(theme: &Theme) -> Flex {
    let m = &theme.tokens.metrics;
    let button = Flex::row()
        .id("nav_toggle")
        .fixed(m.bar_height, m.bar_height)
        .justify(Justify::Center)
        .align(Align::Center)
        .push(icon(Icon::Menu, m.icon_md, theme.text_primary))
        .surface(Surface {
            fill: None,
            radius: f64::from(theme.tokens.radii.full),
            border: None,
        })
        .hover_surface(hover_pill_surface(theme))
        .accessible_name("Show or hide navigation");
    if let Some(binding) = crate::actions::sidebar_binding() {
        button.on_action(binding)
    } else {
        button
    }
}

fn section_header(theme: &Theme, kind: Icon, title: &str, subtitle: &str) -> Flex {
    let title_row = Flex::row()
        .fill_w()
        // Cards inset their content by `CARD_PAD` inside the grid; the heading
        // row insets the same so its icon and trailing label sit on the card
        // content's left and right edges instead of 6 px outside them.
        .pad_xy(CARD_PAD, 0.0)
        .gap(HEADING_GAP)
        .align(Align::Center)
        .push(icon(kind, 22.0, theme.accent_primary))
        .push(text(title, 21.0));
    // Large type must reflow rather than leave a right-aligned subtitle to
    // collide with the heading. The subtitle remains present and readable;
    // it simply takes its own line, as it would on a compact window.
    if theme.text_scale >= 1.5 {
        // The secondary phrase is useful at ordinary density but repeats the
        // selected source/filter and has no action. At large text we preserve
        // the primary landmark and reclaim its row for actual results instead
        // of producing a narrow, three-line fragment above the first card.
        Flex::col().fill_w().push(title_row).push(gap(1.0, 38.0))
    } else {
        title_row.push(spring()).push(dim(theme, subtitle, 13.0))
    }
}

// ---------------------------------------------------------------------------
// cards
// ---------------------------------------------------------------------------

fn video_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, v: &Video) -> Flex {
    let dearrow = model.settings.dearrow_titles_on();
    let progress = model.history.first().filter(|h| **h == v.id).map(|_| 0.35);
    let mut thumb = Thumbnail::new(&v.seed, v.duration)
        .height(THUMB_H)
        .image_source(imgs.store_handle(), model.display_thumbnail_url(v))
        .live(v.live);
    if let Some(p) = progress {
        thumb = thumb.progress(p);
    }

    let high_text = theme.text_scale >= 1.5;
    let title = text(v.display_title(dearrow), 14.5);
    let stats_label = format!("{} • {}", human_views(v.views), v.published);
    let details = if high_text {
        // The leaf measures and paints all three scaled lanes together. This
        // makes its bottom edge a real typography result, rather than a set of
        // guessed spacers which can collide with the next card at 2×.
        Flex::col().fill_w().push(LargeVideoMeta::new(
            v.display_title(dearrow),
            &v.channel_name,
            &stats_label,
        ))
    } else {
        let channel = Flex::row()
            .fill_w()
            .gap(5.0)
            .align(Align::Center)
            .push(
                Flex::row()
                    .grow(1.0)
                    .push(dim(theme, &v.channel_name, 12.5)),
            )
            .push_if(
                model.channel(&v.channel_id).is_some_and(|c| c.verified),
                icon(Icon::Check, 12.0, theme.text_dim),
            );
        let stats = dim(theme, stats_label, 12.5);
        let meta = Flex::col()
            .gap(3.0)
            .grow(1.0)
            .push(title)
            .push(channel)
            .push(stats);
        Flex::row()
            .gap(10.0)
            .fill_w()
            .align(Align::Start)
            .push(channel_avatar(
                imgs,
                model,
                &v.channel_id,
                &v.channel_name,
                36.0,
            ))
            .push(meta)
    };
    // No fixed width: as a responsive-grid item the card stretches to fill its
    // column (which the grid sizes between CARD_W and its natural max).
    Flex::col()
        .id(&format!("video_card_{}", v.id))
        .gap(CARD_BODY_GAP)
        .pad(CARD_PAD)
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: f64::from(theme.tokens.radii.lg),
            border: None,
        })
        .on_click(bus, Action::OpenVideo(v.id.clone()))
        .accessible_name(format!("Open video: {}", v.display_title(dearrow)))
        .push(thumb)
        .push(details)
}

/// Compact horizontal card for the "Up next" rail + history rows.
fn compact_card(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs, v: &Video) -> Flex {
    let dearrow = model.settings.dearrow_titles_on();
    Flex::row()
        .fill_w()
        .gap(CARD_AVATAR_GAP)
        .pad(CARD_PAD)
        .align(Align::Start)
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: f64::from(theme.tokens.radii.md),
            border: None,
        })
        .on_click(bus, Action::OpenVideo(v.id.clone()))
        .accessible_name(format!("Open video: {}", v.display_title(dearrow)))
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
                .gap(theme.tokens.spacing.s1)
                .push(text(v.display_title(dearrow), 13.0))
                .push(dim(theme, &v.channel_name, 12.0))
                // Join only the parts we actually have, so a playlist entry with no
                // view count / date never shows a dangling "0 • " or a trailing "•".
                .push_if(
                    !views_date_line(v.views, &v.published).is_empty(),
                    dim(theme, views_date_line(v.views, &v.published), 11.5),
                ),
        )
}

/// A resource-addressed save/remove control. The shared queue is intentionally
/// not optimistically mutated here: its server-preferred action either commits
/// through the collaboration host and comes back in the document projection,
/// or remains unchanged. That keeps the visible state honest when offline.
fn queue_control(model: &TubeModel, theme: &Theme, video_id: &str) -> Flex {
    let queued = model.is_queued(video_id);
    let label = if queued { "In queue" } else { "Add to queue" };
    let sel = selected_accent_fill(theme);
    let surface = if queued {
        Surface {
            fill: Some(sel.bg),
            radius: f64::from(theme.tokens.radii.full),
            border: None,
        }
    } else {
        Surface {
            fill: Some(subtle_fill(theme)),
            radius: f64::from(theme.tokens.radii.full),
            border: Some((1.0, theme.border_standard)),
        }
    };
    let fg = if queued { sel.fg } else { theme.text_primary };
    let control = Flex::row()
        .id(&format!("queue_btn_{video_id}"))
        .gap(6.0)
        .pad_xy(12.0, 8.0)
        .align(Align::Center)
        .surface(surface)
        .hover_surface(Surface {
            fill: Some(if queued {
                theme.tokens.colors.accent.stops[3]
            } else {
                hover_fill(theme)
            }),
            radius: f64::from(theme.tokens.radii.full),
            border: surface.border,
        })
        .accessible_name(format!("{label}: shared watch queue"))
        .push(icon(Icon::ListVideo, 15.0, fg))
        .push(text_c(label, 12.5, fg));
    if let Some(binding) = crate::actions::queue_binding(model, video_id) {
        control.on_action(binding)
    } else {
        control
    }
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

fn grid(theme: &Theme, cards: Vec<Flex>) -> Flex {
    grid_with_id(theme, cards, "video_grid")
}

fn grid_with_id(theme: &Theme, cards: Vec<Flex>, id: &str) -> Flex {
    // At 2× text a card's title and metadata are intentionally allowed to
    // occupy several lines. A vertical feed is more readable than squeezing
    // four visual cards into a desktop-width row; ordinary text keeps the
    // efficient responsive grid below.
    if theme.text_scale >= 1.5 {
        let mut col = Flex::col().id(id).fill_w().gap(24.0);
        for card in cards {
            col = col.push(card);
        }
        return col;
    }
    // Responsive grid: columns are ≥ `CARD_W` and flex to fill the row, so the
    // cards scale with the window width up to their natural max and only then
    // wrap to add a column — the row is always filled edge to edge (no right
    // gutter). `align(Start)` top-aligns cards of differing heights per row.
    let mut row = Flex::row()
        .id(id)
        .fill_w()
        .grid(CARD_W)
        .gap(GRID_GAP)
        .align(Align::Start);
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
        .gap(CARD_BODY_GAP)
        .pad(CARD_PAD)
        .push(skeleton_block(
            theme,
            Len::Fill,
            THUMB_H,
            f64::from(theme.tokens.radii.lg),
        ))
        .push(
            Flex::row()
                .gap(CARD_AVATAR_GAP)
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
    grid(theme, (0..n).map(|_| skeleton_card(theme)).collect())
}

// ---------------------------------------------------------------------------
// chrome: nav rail + top bar
// ---------------------------------------------------------------------------

fn nav_item(
    theme: &Theme,
    kind: Icon,
    label: &str,
    route: &Route,
    active: bool,
    expanded: bool,
) -> Flex {
    // Active item: a solid accent fill with a foreground chosen to contrast it
    // (so the selected destination reads clearly), instead of accent-coloured
    // glyphs floating on a near-white slate-200 chip (3.6:1, unreadable).
    let (fg, surface) = if active {
        let sel = selected_nav_item(theme);
        (
            sel.fg,
            Surface {
                fill: Some(sel.bg),
                radius: f64::from(theme.tokens.radii.lg),
                border: None,
            },
        )
    } else {
        (
            theme.text_dim,
            Surface {
                fill: None,
                radius: f64::from(theme.tokens.radii.lg),
                border: None,
            },
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
            radius: f64::from(theme.tokens.radii.lg),
            border: None,
        }
    };
    // Stable, addressable id so headless click-tests can target a nav item by
    // name (e.g. "nav_home"). Derived from the label, lowercased.
    let id = format!("nav_{}", label.to_lowercase());
    let item = if expanded {
        Flex::row()
            .id(&id)
            .fill_w()
            .gap(12.0)
            .pad_xy(14.0, 12.0)
            .align(Align::Center)
            .surface(surface)
            .hover_surface(hover)
            .accessible_name(label)
            .push(icon(kind, 21.0, fg))
            .push(text_c(label, 14.0, fg))
    } else {
        Flex::col()
            .id(&id)
            .fill_w()
            .gap(4.0)
            .pad_xy(4.0, 8.0)
            .align(Align::Center)
            .justify(Justify::Center)
            .surface(surface)
            .hover_surface(hover)
            .accessible_name(label)
            .push(icon(kind, 22.0, fg))
            .push(text_c(label, 11.0, fg))
    };
    if let Some(binding) = crate::actions::navigation_binding(route) {
        item.on_action(binding)
    } else {
        item
    }
}

pub fn nav_rail(model: &TubeModel, theme: &Theme, _bus: &ActionBus, width: f64) -> Flex {
    let sec = model.route.nav_section();
    let expanded = width > NAV_RAIL_W;
    Flex::col()
        .w(Len::Px(width as f32))
        .fill_h()
        .pad_xy(8.0, 14.0)
        .gap(8.0)
        .align(Align::Center)
        // Bar-role chrome (the rail): base fill, one derived hairline.
        .surface(Surface {
            fill: Some(theme.tokens.colors.surfaces.base),
            radius: 0.0,
            border: Some((1.0, ui::chrome::chrome_hairline(theme))),
        })
        .push(
            Flex::col()
                .fixed(44.0, 44.0)
                .align(Align::Center)
                .justify(Justify::Center)
                .surface(Surface {
                    fill: Some(theme.accent_primary),
                    radius: f64::from(theme.tokens.radii.lg),
                    border: None,
                })
                .push(icon(Icon::Play, 22.0, Color::WHITE)),
        )
        .push_if(expanded, text_c("Tube", 16.0, theme.text_primary))
        .push(gap(1.0, 6.0))
        .push(nav_item(
            theme,
            Icon::Home,
            "Home",
            &Route::Home,
            sec == "home",
            expanded,
        ))
        .push(nav_item(
            theme,
            Icon::Flame,
            "Trending",
            &Route::Trending,
            sec == "trending",
            expanded,
        ))
        .push(nav_item(
            theme,
            Icon::GalleryVerticalEnd,
            "Subs",
            &Route::Subscriptions,
            sec == "subs",
            expanded,
        ))
        .push(nav_item(
            theme,
            Icon::ListVideo,
            "Queue",
            &Route::Queue,
            sec == "queue",
            expanded,
        ))
        .push(nav_item(
            theme,
            Icon::History,
            "History",
            &Route::History,
            sec == "history",
            expanded,
        ))
        .push(spring())
        .push(nav_item(
            theme,
            Icon::Settings,
            "Settings",
            &Route::Settings,
            sec == "settings",
            expanded,
        ))
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
        .push(icon(Icon::ArrowLeft, 20.0, theme.text_primary))
        .surface(Surface {
            fill: None,
            radius: f64::from(theme.tokens.radii.full),
            border: None,
        })
        .hover_surface(hover_pill_surface(theme))
        .on_click(bus, Action::Back)
        .accessible_name("Go back")
}

pub fn top_bar(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    can_back: bool,
    search_query: &ui::LocalState<String>,
    compact: bool,
) -> Flex {
    // The search field uses the toolkit's process-local TextInput binding. It
    // survives navigation rebuilds in this app instance but never enters the
    // document or syncs to peers. It is
    // ALWAYS present — its own placeholder draws the "Search" hint while empty —
    // so the user can click into it and type from a cold start. (An earlier
    // build swapped the input for a static label while empty, which meant the
    // input was absent from the tree until a query already existed, so there was
    // nothing to focus and free-text search was unreachable.)
    let search_input = TextInput::from_state(search_query)
        .with_id("search_input")
        .with_kind(ui::widget::TextInputKind::Search)
        .with_plain_chrome()
        .with_font_size(15.0)
        .with_placeholder("Search");

    // A single search lens (leading) — Enter submits, so there is no second
    // trailing lens/button cluttering the field.
    let sp = &theme.tokens.spacing;
    let search_field = Flex::row()
        .grow(1.0)
        .max_w(560.0)
        .h(Len::Px(SEARCH_FIELD_H))
        .gap(SEARCH_FIELD_GAP)
        .pad_xy(sp.s4, 0.0)
        .align(Align::Center)
        .surface(subtle_surface(theme))
        .push(icon(Icon::Search, 18.0, theme.text_dim))
        .push(Flex::row().grow(1.0).push(search_input));
    let search = if compact {
        icon_button(theme, bus, Icon::Search, Action::OpenPalette).id("compact_search")
    } else {
        search_field
    };

    // Only surface the data-source indicator when we're NOT on live data
    // (connecting or offline) — in the normal live case the top bar stays clean.
    // SponsorBlock / DeArrow are settings, not top-bar toggles; they live in Settings.
    let status = Flex::row().gap(sp.s2).align(Align::Center).push_if(
        !compact && !model.data_source.live,
        data_source_pill(theme, model),
    );

    Flex::row()
        .fill_w()
        .h(Len::Px(TOP_BAR_H))
        .gap(sp.s4)
        .pad_xy(TOP_BAR_PAD_X, 0.0)
        .align(Align::Center)
        // Bar-role chrome (the top bar): base fill, one derived hairline.
        .surface(Surface {
            fill: Some(theme.tokens.colors.surfaces.base),
            radius: 0.0,
            border: Some((1.0, ui::chrome::chrome_hairline(theme))),
        })
        .push(menu_button(theme))
        .push_if(can_back, back_button(theme, bus))
        // The OS owns the window title/app-name — no "Tube" wordmark here.
        .push(search)
        // Push the status + account avatar hard to the right edge for a
        // balanced bar (search sits centre-left, account is top-right).
        .push(spring())
        .push(status)
        .push(avatar("You", 36.0))
}

/// Live-vs-sample provenance pill. A green dot + "Live" once the rustypipe
/// backend has returned data; a dim "Sample" (or "Connecting…") otherwise,
/// so the user can distinguish `YouTube` data from the bundled offline sample.
fn data_source_pill(theme: &Theme, model: &TubeModel) -> Flex {
    let live = model.data_source.live;
    let (dot, fg, label) = if live {
        (
            theme.tokens.colors.semantic.success,
            theme.text_primary,
            "Live",
        )
    } else if model.data_source.label.contains("connect") {
        (
            theme.tokens.colors.semantic.warning,
            theme.text_dim,
            "Connecting…",
        )
    } else {
        (theme.text_dim, theme.text_dim, "Offline")
    };
    Flex::row()
        .gap(theme.tokens.metrics.icon_label_gap)
        .pad_xy(STATUS_PILL_PAD_X, STATUS_PILL_PAD_Y)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: f64::from(theme.tokens.radii.full),
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
        (
            sel.fg,
            Surface {
                fill: Some(sel.bg),
                radius: f64::from(theme.tokens.radii.full),
                border: None,
            },
            theme.tokens.colors.accent.stops[3],
        )
    } else {
        (
            theme.text_primary,
            chip_surface(theme, false),
            hover_fill(theme),
        )
    };
    Flex::row()
        .pad_xy(CHIP_PAD_X, CHIP_PAD_Y)
        .surface(resting)
        .hover_surface(Surface {
            fill: Some(hover_bg),
            radius: f64::from(theme.tokens.radii.full),
            border: None,
        })
        .on_click(bus, action)
        .accessible_name(label)
        .push(text_c(label, 13.0, fg))
}

// ---------------------------------------------------------------------------
// screens
// ---------------------------------------------------------------------------

fn scroll(content: Flex) -> Scrollable {
    Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(content.block()))
}

pub fn home(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    // Cold start shows a skeleton (loading) or an empty state. Cards are added
    // when catalogue data arrives.
    let body = if model.videos.is_empty() {
        if model.feed_loading {
            skeleton_grid(theme, 12)
        } else if let Some(error) = &model.feed_error {
            empty_state(theme, "Home refresh delayed", error)
        } else if model.data_source.label.to_lowercase().contains("offline") {
            empty_state(
                theme,
                "You're offline",
                "Reconnect, then search or return Home to load videos. Your subscriptions and watch history remain available.",
            )
        } else {
            empty_state(
                theme,
                "Nothing to show yet",
                "Search for something, or check back when the trending feed loads.",
            )
        }
    } else {
        grid(
            theme,
            model
                .videos
                .iter()
                .map(|v| video_card(model, theme, bus, imgs, v))
                .collect(),
        )
    };
    scroll(
        Flex::col()
            .fill_w()
            .pad(theme.tokens.spacing.s6)
            .gap(GRID_GAP)
            .push(section_header(
                theme,
                Icon::Home,
                "Home",
                "Recent and popular",
            ))
            .push(body),
    )
}

pub fn trending(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let mut vids: Vec<&Video> = model.videos.iter().collect();
    vids.sort_by_key(|v| std::cmp::Reverse(v.views));
    let cards: Vec<Flex> = vids
        .iter()
        .map(|v| video_card(model, theme, bus, imgs, v))
        .collect();
    scroll(
        Flex::col()
            .fill_w()
            .pad(theme.tokens.spacing.s6)
            .gap(GRID_GAP)
            .push(section_header(
                theme,
                Icon::Flame,
                "Trending",
                "Most-watched right now",
            ))
            .push(grid(theme, cards)),
    )
}

pub fn subscriptions(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let mut rail = Flex::row().fill_w().wrap().gap(GRID_GAP);
    for cid in &model.subscriptions {
        if let Some(ch) = model.channel(cid) {
            rail = rail.push(
                Flex::col()
                    .w(Len::Px(96.0))
                    .gap(6.0)
                    .align(Align::Center)
                    .pad(CARD_PAD)
                    .hover_surface(Surface {
                        fill: Some(hover_fill(theme)),
                        radius: f64::from(theme.tokens.radii.lg),
                        border: None,
                    })
                    .on_click(bus, Action::OpenChannel(ch.id.clone()))
                    .accessible_name(format!("Open creator: {}", ch.name))
                    .push(channel_avatar(imgs, model, &ch.id, &ch.name, 56.0))
                    .push(text_c(&ch.name, 11.5, theme.text_primary)),
            );
        }
    }

    let feed = model.subscription_feed();
    let cards: Vec<Flex> = feed
        .iter()
        .map(|v| video_card(model, theme, bus, imgs, v))
        .collect();
    let body = if cards.is_empty() {
        empty_state(
            theme,
            "No subscriptions yet",
            "Subscribe to channels and their latest uploads land here.",
        )
    } else {
        grid(theme, cards)
    };

    scroll(
        Flex::col()
            .fill_w()
            .pad(theme.tokens.spacing.s6)
            .gap(GRID_GAP)
            .push(section_header(
                theme,
                Icon::GalleryVerticalEnd,
                "Subscriptions",
                &format!("{} channels", model.subscriptions.len()),
            ))
            .push(rail)
            .push(divider(theme))
            .push(text("Latest from your subscriptions", 16.0))
            .push(body),
    )
}

pub fn history(model: &TubeModel, theme: &Theme, bus: &ActionBus, imgs: Imgs) -> Scrollable {
    let vids = model.history_videos();
    let mut col = Flex::col().fill_w().gap(theme.tokens.spacing.s2);
    if vids.is_empty() {
        col = col.push(empty_state(
            theme,
            "Nothing watched yet",
            "Videos you watch show up here.",
        ));
    }
    for v in vids {
        col = col.push(compact_card(model, theme, bus, imgs, v));
    }
    if let Some(recovery) = &model.history_recovery_notice {
        col = col.push(empty_state(theme, "History recovery needed", recovery));
    }
    scroll(
        Flex::col()
            .fill_w()
            .pad(theme.tokens.spacing.s6)
            .gap(GRID_GAP)
            .max_w(820.0)
            .push(
                Flex::row()
                    .fill_w()
                    .gap(12.0)
                    .align(Align::Center)
                    .push(Flex::row().grow(1.0).push(section_header(
                        theme,
                        Icon::History,
                        "Watch history",
                        "Private to this profile — syncs across your devices",
                    )))
                    .push_if(
                        !model.history.is_empty(),
                        pill(
                            theme,
                            bus,
                            "Clear history",
                            Some(Icon::Trash),
                            Action::ClearHistory,
                            false,
                        ),
                    ),
            )
            .push(col),
    )
}

/// The collaborative queue is membership-based rather than a shared ordered
/// playlist. Say that plainly in the view: people can save the same videos,
/// while each device still controls what plays next and in which order.
pub fn queue(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    available_width: f64,
) -> Scrollable {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a desktop viewport width is safely representable as f32"
    )]
    let content_width = (available_width - 64.0).clamp(1.0, 920.0) as f32;
    let videos: Vec<&Video> = model
        .queue
        .iter()
        .filter_map(|id| model.video(id))
        .collect();
    let unavailable = model.queue.len().saturating_sub(videos.len());
    let mut entries = Flex::col().fill_w().gap(10.0);
    if videos.is_empty() {
        entries = entries.push(queue_empty_state(theme, content_width));
    }
    for video in videos {
        entries = entries.push(
            // Keep the queue action on its own line: at a narrow window or
            // 200% text the compact card retains a useful title lane instead
            // of sacrificing it to a fixed-width secondary button.
            Flex::col()
                .fill_w()
                .gap(8.0)
                .push(compact_card(model, theme, bus, imgs, video))
                .push(queue_control(model, theme, &video.id)),
        );
    }
    if unavailable > 0 {
        entries = entries.push(dim(
            theme,
            format!("{unavailable} queued item(s) need metadata before they can be shown."),
            12.5,
        ));
    }
    scroll(
        Flex::col()
            .id("queue_page")
            .fill_w()
            .pad(24.0)
            .gap(18.0)
            .max_w(920.0)
            .push(queue_heading(theme, model.queue.len(), content_width))
            .push(entries),
    )
}

/// Queue-specific large-text heading. Generic one-line chrome is fine at the
/// normal scale, but at 200% it can become a multi-line `Text` inside a flex
/// row. Splitting the compact, stable phrases into independently measured
/// lines makes their vertical geometry explicit instead of letting glyphs
/// paint below their one-line allocation.
fn queue_heading(theme: &Theme, count: usize, content_width: f32) -> Flex {
    if theme.text_scale >= 1.5 {
        return Flex::col()
            .id("queue_heading")
            .w(Len::Px(content_width))
            .gap(8.0)
            .push(
                Flex::row()
                    .gap(10.0)
                    .align(Align::Center)
                    .push(icon(Icon::ListVideo, 22.0, theme.accent_primary))
                    .push(text("Shared queue", 21.0).with_id("queue_heading_title")),
            )
            .push(
                Flex::row().fill_w().push(
                    dim(theme, format!("{count} saved"), 13.0).with_id("queue_heading_count"),
                ),
            )
            .push(gap(1.0, 8.0))
            .push(Flex::row().fill_w().push(
                dim(theme, "Membership is shared.", 13.0).with_id("queue_heading_membership"),
            ))
            .push(Flex::row().fill_w().push(
                dim(theme, "Playback order stays local.", 13.0).with_id("queue_heading_order"),
            ));
    }
    Flex::col()
        .id("queue_heading")
        .fill_w()
        .gap(8.0)
        .push(section_header(
            theme,
            Icon::ListVideo,
            "Shared queue",
            &format!("{count} saved video{}", if count == 1 { "" } else { "s" }),
        ))
        .push(dim(
            theme,
            "Shared membership; each person controls local playback order.",
            13.0,
        ))
}

/// An explicit large-text empty state for the queue. Its short individual
/// lines are deliberately all independently laid out; the regular generic
/// empty state is retained at normal scale where its compact prose has room.
fn queue_empty_state(theme: &Theme, content_width: f32) -> Flex {
    if theme.text_scale >= 1.5 {
        return Flex::col()
            .id("queue_empty_state")
            .w(Len::Px(content_width))
            .gap(12.0)
            .pad(48.0)
            .align(Align::Center)
            .justify(Justify::Center)
            .push(icon(Icon::Eye, 40.0, theme.text_dim))
            .push(
                Flex::row()
                    .fill_w()
                    .justify(Justify::Center)
                    .push(text("Queue empty", 18.0).with_id("queue_empty_title")),
            )
            .push(Flex::row().fill_w().justify(Justify::Center).push(
                dim(theme, "Add from a video's watch page.", 13.0).with_id("queue_empty_add"),
            ))
            .push(Flex::row().fill_w().justify(Justify::Center).push(
                dim(theme, "Sync needs a collaboration host.", 13.0).with_id("queue_empty_sync"),
            ));
    }
    empty_state(
        theme,
        "Your shared queue is empty",
        "Add a video from its watch page. Queue membership syncs with collaborators when a collaboration host is available.",
    )
    .id("queue_empty_state")
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
    let mut row = Flex::row()
        .id("search_type_chips")
        .fill_w()
        .wrap()
        .gap(theme.tokens.spacing.s2);
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
                .push(icon(Icon::ListVideo, 12.0, Color::WHITE))
                .push(text_c(
                    format!("{} videos", human_count(p.video_count)),
                    11.0,
                    Color::WHITE,
                )),
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
        .push(dim(theme, format!("{} • Playlist", p.channel_name), 12.5))
        .push(dim(
            theme,
            format!("{} videos", human_count(p.video_count)),
            12.5,
        ));

    Flex::col()
        .fill_w()
        .gap(9.0)
        .pad(CARD_PAD)
        .id(&format!("playlist_card_{}", p.id))
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: f64::from(theme.tokens.radii.lg),
            border: None,
        })
        .on_click(bus, Action::OpenPlaylist(p.id.clone()))
        .accessible_name(format!("Open playlist: {}", p.title))
        .push(stack)
        .push(
            Flex::row()
                .gap(10.0)
                .fill_w()
                .align(Align::Start)
                .push(icon(Icon::ListVideo, 22.0, theme.accent_primary))
                .push(meta),
        )
}

/// A creator search-result row: the real avatar, name (+ verified), handle and
/// subscriber count, and a View affordance. Opens the channel page.
fn creator_card(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    c: &Channel,
) -> Flex {
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
            radius: f64::from(theme.tokens.radii.md),
            border: None,
        })
        .on_click(bus, Action::OpenChannel(c.id.clone()))
        .accessible_name(format!("Open creator: {}", c.name))
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
                        .push_if(c.verified, icon(Icon::Check, 14.0, theme.text_dim)),
                )
                .push_if(!handle_subs.is_empty(), dim(theme, &handle_subs, 12.5)),
        )
        .push(pill(
            theme,
            bus,
            "View",
            None,
            Action::OpenChannel(c.id.clone()),
            false,
        ))
}

pub fn search(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    query: &str,
) -> Scrollable {
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
    let visible_results = videos.len() + playlists.len() + creators.len();

    let body = if model.search_loading {
        // While a fetch is in flight, show a skeleton — NEVER a premature "No
        // results".
        skeleton_grid(theme, 8)
    } else if let Some(error) = &model.search_error {
        empty_state(theme, "Search delayed", error)
    } else if query.trim().is_empty() {
        empty_state(
            theme,
            "Search Tube",
            "Type a query above to find videos, playlists and creators.",
        )
    } else if videos.is_empty() && playlists.is_empty() && creators.is_empty() {
        empty_state(
            theme,
            "No results",
            "Nothing matched that search — try different words or filters.",
        )
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
            let cards: Vec<Flex> = playlists
                .iter()
                .map(|p| playlist_card(theme, bus, imgs, p))
                .collect();
            col = col.push(grid_with_id(theme, cards, "playlist_grid"));
        }
        if !videos.is_empty() {
            if visible_results == 1 {
                // A lone desktop result should read as an intentionally focused
                // outcome, not a lost card in a kilometre-wide grid. Constrain
                // and center the same honest card; narrow layouts retain the
                // full responsive grid once more than one result exists.
                if let Some(video) = videos.first() {
                    col = col.push(
                        Flex::row()
                            .id("sparse_video_result")
                            .fill_w()
                            .justify(Justify::Center)
                            .push(
                                Flex::col()
                                    .w(Len::Px(440.0))
                                    .push(video_card(model, theme, bus, imgs, video)),
                            ),
                    );
                }
            } else {
                let cards: Vec<Flex> = videos
                    .iter()
                    .map(|v| video_card(model, theme, bus, imgs, v))
                    .collect();
                col = col.push(grid(theme, cards));
            }
        }
        col
    };
    // Retain compact result context so a sparse wide result set is not an
    // unlabelled card floating in an otherwise empty lane. The top bar already
    // presents the active query, so do not repeat it here.
    let summary = (!model.search_loading
        && model.search_error.is_none()
        && !query.trim().is_empty()
        && visible_results > 0)
        .then(|| {
            let noun = if visible_results == 1 {
                "result"
            } else {
                "results"
            };
            Flex::col()
                .id("search_results_summary")
                .gap(3.0)
                .push(text("Search results", 20.0))
                .push(dim(theme, format!("{visible_results} {noun} shown"), 13.0))
        });
    let col = Flex::col()
        .fill_w()
        .pad(24.0)
        .gap(18.0)
        .push(search_type_chips(model, theme, bus))
        .push_if(summary.is_some(), summary.unwrap_or_else(Flex::col))
        .push(body);
    scroll(col)
}

/// A playlist page: its cover, title, creator and video count, a "Play all"
/// affordance, and the ordered video list.
pub fn playlist(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    id: &str,
) -> Scrollable {
    let Some(pl) = model.playlist(id) else {
        return scroll(
            Flex::col()
                .pad(24.0)
                .push(empty_state(theme, "Playlist unavailable", "")),
        );
    };
    let videos: Vec<&Video> = pl
        .video_ids
        .iter()
        .filter_map(|vid| model.video(vid))
        .collect();
    let first_id = videos.first().map(|v| v.id.clone());

    let mut info = Flex::col()
        .grow(1.0)
        .gap(8.0)
        .align(Align::Start)
        .push(text(&pl.title, 26.0))
        .push(dim(
            theme,
            format!(
                "{}  •  {} videos",
                pl.channel_name,
                human_count(pl.video_count)
            ),
            13.5,
        ));
    if let Some(fid) = first_id {
        info = info.push(
            pill(
                theme,
                bus,
                "Play all",
                Some(Icon::Play),
                Action::OpenVideo(fid),
                true,
            )
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
        list = list.push(empty_state(
            theme,
            "Loading playlist…",
            "Fetching the playlist's videos.",
        ));
    }
    for v in &videos {
        list = list.push(compact_card(model, theme, bus, imgs, v));
    }

    scroll(
        Flex::col()
            .id("playlist_page")
            .fill_w()
            .pad(theme.tokens.spacing.s6)
            .gap(GRID_GAP)
            .push(header)
            .push(divider(theme))
            .push(text("Videos", 16.0))
            .push(list),
    )
}

pub fn channel(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    id: &str,
) -> Scrollable {
    let Some(ch) = model.channel(id) else {
        return scroll(
            Flex::col()
                .pad(24.0)
                .push(empty_state(theme, "Channel not found", "")),
        );
    };
    let subscribed = model.is_subscribed(id);

    // Real banner image when the store has it, else the seed gradient.
    let banner = ImageBox::new(&ch.seed, 160.0, f64::from(theme.tokens.radii.lg), None)
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
                        .push_if(ch.verified, icon(Icon::Check, 18.0, theme.text_dim)),
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
            pill(
                theme,
                bus,
                "Subscribed",
                Some(Icon::Bell),
                Action::ToggleSubscribe(ch.id.clone()),
                false,
            )
        } else {
            pill(
                theme,
                bus,
                "Subscribe",
                None,
                Action::ToggleSubscribe(ch.id.clone()),
                true,
            )
        });

    let cards: Vec<Flex> = model
        .channel_videos(id)
        .iter()
        .map(|v| video_card(model, theme, bus, imgs, v))
        .collect();

    scroll(
        Flex::col()
            .fill_w()
            .pad(theme.tokens.spacing.s6)
            .gap(GRID_GAP)
            .push(banner)
            .push(header)
            .push(divider(theme))
            .push(text("Videos", 16.0))
            .push(grid(theme, cards)),
    )
}

fn empty_state(theme: &Theme, title: &str, body: &str) -> Flex {
    Flex::col()
        .fill_w()
        .gap(8.0)
        .pad(48.0)
        .align(Align::Center)
        .justify(Justify::Center)
        .push(icon(Icon::Eye, 40.0, theme.text_dim))
        .push(text(title, 18.0))
        .push(dim(theme, body, 13.0))
}

// ---------------------------------------------------------------------------
// watch page
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "a single linear layout build over the watch page's fixed regions (player, \
              title/actions, tabs, comments); splitting it would only relocate the line \
              count, not the amount a reader must hold in mind"
)]
pub fn watch(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    imgs: Imgs,
    id: &str,
    content_size: (f64, f64),
    player: SharedPlayer,
) -> Scrollable {
    let (content_w, content_h) = content_size;
    let Some(v) = model.video(id) else {
        return scroll(
            Flex::col()
                .pad(24.0)
                .push(empty_state(theme, "Video unavailable", "")),
        );
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
                (
                    s.start as f32 / dur,
                    s.end as f32 / dur,
                    Color::from_rgb8(r, g, b),
                )
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
        let qualities: Vec<String> = crate::app::QUALITIES
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let sel = crate::app::QUALITIES
            .iter()
            .position(|q| *q == model.settings.preferred_quality)
            .unwrap_or(0);
        player
            .borrow_mut()
            .set_setting(ui::widget::video_player::PlayerSetting::choice(
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
    let manual = v
        .sponsor
        .iter()
        .find(|s| model.settings.should_show_skip(s.category) && s.contains(model.playhead));

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
                            radius: f64::from(theme.tokens.radii.full),
                            border: Some((1.0, Color::from_rgba8(255, 255, 255, 40))),
                        })
                        .hover_surface(Surface {
                            fill: Some(theme.accent_primary),
                            radius: f64::from(theme.tokens.radii.full),
                            border: None,
                        })
                        .on_click(bus, Action::SkipSegment(s.end))
                        .accessible_name(format!("Skip {}", s.category.label()))
                        .push(text_c(
                            format!("Skip {}", s.category.label()),
                            13.0,
                            Color::WHITE,
                        ))
                        .push(icon(Icon::SkipForward, 14.0, Color::WHITE)),
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
        if has_sb_timeline {
            col.id("sb_timeline")
        } else {
            col
        }
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
            icon_button(
                theme,
                bus,
                Icon::Settings,
                Action::Navigate(Route::Settings),
            )
            .id("watch_settings"),
        );

    let title = text(v.display_title(dearrow), 22.0);
    let liked = model.is_liked(&v.id);
    let mut meta = Flex::row()
        .fill_w()
        .gap(12.0)
        // Like, queue and share remain individually addressable at large text;
        // wrapping preserves their 44px-ish targets instead of crushing the
        // title statistics into an unreadable single line.
        .wrap()
        .align(Align::Center)
        .push(dim(
            theme,
            format!("{} • {}", human_views(v.views), v.published),
            13.0,
        ))
        .push(spring())
        .push(
            pill(
                theme,
                bus,
                &human_count(model.display_likes(v)),
                Some(Icon::ThumbsUp),
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
            .map_or_else(|| "—".to_string(), human_count);
        meta = meta.push(stat_pill(theme, &label, Icon::ThumbsDown).id("dislike_pill"));
    }
    meta = meta.push(queue_control(model, theme, &v.id));
    let meta = meta.push(
        pill(
            theme,
            bus,
            "Share",
            Some(Icon::ChevronRight),
            Action::Share(v.id.clone()),
            false,
        )
        .id("share_btn"),
    );

    let ch_sub = model.is_subscribed(&v.channel_id);
    let channel_row = Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .push(channel_avatar(
            imgs,
            model,
            &v.channel_id,
            &v.channel_name,
            48.0,
        ))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(2.0)
                .on_click(bus, Action::OpenChannel(v.channel_id.clone()))
                .accessible_name(format!("Open creator: {}", v.channel_name))
                .push(text(&v.channel_name, 15.0))
                .push_if(
                    model
                        .channel(&v.channel_id)
                        .and_then(|c| subs_label(c.subscribers))
                        .is_some(),
                    dim(
                        theme,
                        model
                            .channel(&v.channel_id)
                            .and_then(|c| subs_label(c.subscribers))
                            .unwrap_or_default(),
                        12.0,
                    ),
                ),
        )
        .push(
            if ch_sub {
                pill(
                    theme,
                    bus,
                    "Subscribed",
                    Some(Icon::Bell),
                    Action::ToggleSubscribe(v.channel_id.clone()),
                    false,
                )
            } else {
                pill(
                    theme,
                    bus,
                    "Subscribe",
                    None,
                    Action::ToggleSubscribe(v.channel_id.clone()),
                    true,
                )
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
    comments = comments.push(Flex::row().gap(10.0).align(Align::Center).push(text(
        format!("{} Comments", human_count(v.comments.len() as u64)),
        17.0,
    )));
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
    let current = model.settings.preferred_quality.as_str();
    let index = crate::app::QUALITIES
        .iter()
        .position(|quality| *quality == current)
        .unwrap_or(0);
    let next = crate::app::QUALITIES
        .get((index + 1) % crate::app::QUALITIES.len())
        .copied()
        .unwrap_or("1080p");
    Flex::row()
        .gap(6.0)
        .pad_xy(10.0, 6.0)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: f64::from(theme.tokens.radii.md),
            border: Some((1.0, subtle_edge(theme))),
        })
        .on_click(bus, Action::SetQuality(next.to_string()))
        .accessible_name(format!(
            "Playback quality, currently {}",
            model.settings.preferred_quality
        ))
        .push(text_c(
            &model.settings.preferred_quality,
            12.5,
            theme.text_primary,
        ))
        .push(icon(Icon::ChevronRight, 12.0, theme.text_dim))
}

fn autoplay_toggle(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    Flex::row()
        .gap(8.0)
        .align(Align::Center)
        .push(dim(theme, "Autoplay", 12.5))
        .push(
            Switch::new(
                model.settings.autoplay,
                bus,
                Action::ToggleSetting(SettingFlag::Autoplay),
            )
            .accessible_name("Autoplay"),
        )
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
                .push(icon(Icon::SkipForward, 14.0, theme.accent_primary))
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
        head = head.push(icon(
            Icon::ThumbsUp,
            12.0,
            theme.tokens.colors.semantic.danger,
        ));
    }

    let actions = Flex::row()
        .gap(6.0)
        .align(Align::Center)
        .push(icon(Icon::ThumbsUp, 13.0, theme.text_dim))
        .push(dim(theme, human_count(c.likes), 11.5))
        .push(gap(8.0, 1.0))
        .push_if(
            c.replies > 0,
            dim(theme, format!("{} replies", c.replies), 11.5),
        );

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
/// `DeArrow` title/thumbnail toggles while `DeArrow` itself is off), so its state
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
            col = col.push(divider_on(theme.tokens.colors.surfaces.raised));
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

/// A clickable three-state mode selector for a `SponsorBlock` category. Cycles
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
            Some((1.0, subtle_edge(theme))),
        ),
        // An outline-only pill sits straight on the card, so its edge derives
        // from that surface.
        SkipMode::Disabled => (
            None,
            theme.text_dim,
            Some((
                1.0,
                ui::surface::edge_on(theme.tokens.colors.surfaces.raised),
            )),
        ),
    };
    Flex::row()
        .id(&format!("mode_{}", cat.api_key()))
        .w(Len::Px(104.0))
        .pad_xy(12.0, 7.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .surface(Surface {
            fill,
            radius: f64::from(theme.tokens.radii.full),
            border,
        })
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: f64::from(theme.tokens.radii.full),
            border,
        })
        .on_click(bus, Action::ToggleCategory(cat))
        .accessible_name(format!("{}: {}", cat.label(), mode.label()))
        .push(text_c(mode.label(), 12.5, fg))
}

#[allow(
    clippy::too_many_lines,
    reason = "a single linear layout build over the settings page's fixed sections; \
              splitting it would only relocate the line count, not the amount a reader \
              must hold in mind"
)]
pub fn settings(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Scrollable {
    settings_impl(model, theme, bus, None, None)
}

/// Settings with the process-local Grayjay device surface. The pairing input
/// is explicitly external memory, so invite codes never enter Automerge or
/// collaboration history.
pub fn settings_with_sync(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    sync: &crate::sync::SyncView,
    pairing_input: Arc<Mutex<String>>,
) -> Scrollable {
    settings_impl(model, theme, bus, Some(sync), Some(pairing_input))
}

fn settings_impl(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    sync: Option<&crate::sync::SyncView>,
    pairing_input: Option<Arc<Mutex<String>>>,
) -> Scrollable {
    let playback = playback_settings_card(model, theme, bus);
    let sponsorblock = sponsorblock_settings_card(model, theme, bus);
    let dearrow = dearrow_settings_card(model, theme, bus);
    let device_sync = device_sync_card(theme, bus, sync, pairing_input);

    // Explicit spacers between sections: the scroll wrapper lays out as a block,
    // which ignores flex `gap`, so without these the cards would touch.
    scroll(
        Flex::col()
            .fill_w()
            .max_w(820.0)
            .pad(24.0)
            .push(section_header(theme, Icon::Settings, "Settings", ""))
            .push(vspace(18.0))
            .push(playback)
            .push(vspace(16.0))
            .push(sponsorblock)
            .push(vspace(16.0))
            .push(dearrow)
            .push(vspace(16.0))
            .push(device_sync)
            .push(vspace(24.0)),
    )
}

fn playback_settings_card(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let s = &model.settings;
    settings_card(
        theme,
        "Playback",
        vec![
            setting_row(
                theme,
                "Autoplay next video",
                "Play the up-next video when this one ends.",
                Switch::new(
                    s.autoplay,
                    bus,
                    Action::ToggleSetting(SettingFlag::Autoplay),
                )
                .id("switch_autoplay")
                .accessible_name("Autoplay next video"),
            ),
            setting_row(
                theme,
                "Show comments",
                "Load the comment section on the watch page.",
                Switch::new(
                    s.show_comments,
                    bus,
                    Action::ToggleSetting(SettingFlag::ShowComments),
                )
                .id("switch_comments")
                .accessible_name("Show comments"),
            ),
            setting_row(
                theme,
                "Show dislikes",
                "Display the crowdsourced dislike count (Return YouTube Dislike).",
                Switch::new(
                    s.return_dislike_enabled,
                    bus,
                    Action::ToggleSetting(SettingFlag::ReturnDislike),
                )
                .id("switch_dislikes")
                .accessible_name("Show dislikes"),
            ),
            setting_row(
                theme,
                "Preferred quality",
                "Highest resolution to request when available.",
                quality_menu(model, theme, bus),
            ),
        ],
    )
}

fn sponsorblock_settings_card(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let s = &model.settings;
    let mut rows = vec![setting_row(
        theme,
        "Enable SponsorBlock",
        "Use community segment data to mark and skip parts of videos.",
        Switch::new(
            s.sponsorblock_enabled,
            bus,
            Action::ToggleSetting(SettingFlag::SponsorBlock),
        )
        .id("switch_sponsorblock")
        .accessible_name("Enable SponsorBlock"),
    )];
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
        let stored = s
            .categories
            .get(cat.api_key())
            .copied()
            .unwrap_or(SkipMode::Skip);
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
}

fn dearrow_settings_card(model: &TubeModel, theme: &Theme, bus: &ActionBus) -> Flex {
    let s = &model.settings;
    settings_card(
        theme,
        "DeArrow",
        vec![
            setting_row(
                theme,
                "Enable DeArrow",
                "Use community branding in place of clickbait.",
                Switch::new(
                    s.dearrow_enabled,
                    bus,
                    Action::ToggleSetting(SettingFlag::DeArrow),
                )
                .id("switch_dearrow")
                .accessible_name("Enable DeArrow"),
            ),
            // The two sub-toggles are gated by the master switch: they dim while
            // DeArrow is off (their stored choice is still shown + editable), and
            // only take effect once it's on. `dearrow_titles_on()` /
            // `dearrow_thumbs_on()` read exactly this combination on every card.
            setting_row_gated(
                theme,
                "Replace clickbait titles",
                "Show community-submitted, factual titles instead of clickbait.",
                Switch::new(
                    s.dearrow_titles,
                    bus,
                    Action::ToggleSetting(SettingFlag::DeArrowTitles),
                )
                .id("switch_dearrow_titles")
                .accessible_name("Replace clickbait titles"),
                s.dearrow_enabled,
            ),
            setting_row_gated(
                theme,
                "Replace thumbnails",
                "Use neutral community thumbnails in place of clickbait frames.",
                Switch::new(
                    s.dearrow_thumbnails,
                    bus,
                    Action::ToggleSetting(SettingFlag::DeArrowThumbnails),
                )
                .id("switch_dearrow_thumbs")
                .accessible_name("Replace thumbnails"),
                s.dearrow_enabled,
            ),
        ],
    )
}

fn device_action(
    theme: &Theme,
    bus: &ActionBus,
    id: &str,
    label: &str,
    action: Action,
    danger: bool,
) -> Flex {
    let danger_color = theme.tokens.colors.semantic.danger;
    let foreground = if danger {
        danger_color
    } else {
        theme.text_primary
    };
    Flex::row()
        .id(id)
        .pad_xy(13.0, 8.0)
        .justify(Justify::Center)
        .align(Align::Center)
        .surface(Surface {
            fill: Some(subtle_fill(theme)),
            radius: f64::from(theme.tokens.radii.full),
            border: Some((
                1.0,
                if danger {
                    danger_color
                } else {
                    subtle_edge(theme)
                },
            )),
        })
        .hover_surface(hover_pill_surface(theme))
        .on_click(bus, action)
        .accessible_name(label)
        .push(text_c(label, 12.5, foreground))
}

fn device_sync_card(
    theme: &Theme,
    bus: &ActionBus,
    sync: Option<&crate::sync::SyncView>,
    pairing_input: Option<Arc<Mutex<String>>>,
) -> Flex {
    let capability = crate::sync::capability();
    let summary = sync.map_or(capability.summary, |state| state.summary.as_str());
    let status = sync.map_or("Unavailable", |state| match state.status {
        crate::sync::SyncStatus::Unavailable => "Unavailable",
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Ready => "Ready",
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Pairing => "Pairing…",
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Syncing => "Syncing…",
        #[cfg(feature = "grayjay-sync")]
        crate::sync::SyncStatus::Error => "Needs attention",
    });
    let mut rows = vec![device_sync_header(theme, summary, status)];

    if let (Some(state), Some(input)) = (sync, pairing_input) {
        if matches!(state.status, crate::sync::SyncStatus::Unavailable) {
            rows.push(device_sync_unavailable(theme));
            return settings_card(theme, "Device sync", rows);
        }
        add_device_identity_and_pairing(&mut rows, theme, bus, state, input);
        add_pending_devices(&mut rows, theme, bus, state);
        add_active_devices(&mut rows, theme, bus, state);
    } else if !capability.available {
        // Builds intentionally compiled without the Noise transport must be
        // explicit: a bare "Unavailable" badge looks like a transient outage
        // and invites users to hunt for a pairing button that cannot exist.
        rows.push(device_sync_not_included(theme));
    }
    settings_card(theme, "Device sync", rows)
}

fn device_sync_header(theme: &Theme, summary: &str, status: &str) -> Flex {
    Flex::row()
        .fill_w()
        .gap(14.0)
        .align(Align::Center)
        .push(
            Flex::row()
                .fixed(42.0, 42.0)
                .justify(Justify::Center)
                .align(Align::Center)
                .surface(Surface {
                    fill: Some(theme.bg_active),
                    radius: 12.0,
                    border: None,
                })
                .push(icon(Icon::Wifi, 20.0, theme.accent_primary)),
        )
        .push(
            Flex::col()
                .grow(1.0)
                .gap(3.0)
                .push(text("Grayjay devices", 15.0))
                .push(dim(theme, summary, 12.0)),
        )
        .push(
            Flex::row()
                .pad_xy(10.0, 6.0)
                .surface(subtle_surface(theme))
                .push(text_c(status, 12.0, theme.text_dim)),
        )
}

fn device_sync_unavailable(theme: &Theme) -> Flex {
    Flex::row()
        .fill_w()
        .pad(16.0)
        .surface(subtle_surface(theme))
        .push(dim(
            theme,
            "Encrypted device sync is unavailable on this desktop. Ensure the desktop secret broker is available and unlocked, then reopen Tube.",
            12.5,
        ))
}

fn device_sync_not_included(theme: &Theme) -> Flex {
    Flex::row()
        .fill_w()
        .pad(16.0)
        .surface(subtle_surface(theme))
        .push(dim(
            theme,
            "This Tube build does not include Grayjay device sync. Install a build with device sync support to pair or synchronize devices.",
            12.5,
        ))
}

fn add_device_identity_and_pairing(
    rows: &mut Vec<Flex>,
    theme: &Theme,
    bus: &ActionBus,
    state: &crate::sync::SyncView,
    input: Arc<Mutex<String>>,
) {
    if let Some(public_key) = &state.device_public_key {
        let fingerprint = public_key.chars().take(18).collect::<String>();
        rows.push(
            Flex::row()
                .fill_w()
                .gap(10.0)
                .align(Align::Center)
                .push(icon(Icon::Shield, 16.0, theme.status_success))
                .push(dim(
                    theme,
                    format!("This device · {fingerprint}… · key held by desktop secret broker"),
                    12.0,
                )),
        );
    }
    rows.push(
        Flex::col()
            .fill_w()
            .gap(8.0)
            .push(text("Pair a device", 14.0))
            .push(dim(
                theme,
                "In Grayjay, open Sync → Add device, then paste its invite here.",
                12.0,
            ))
            .push(
                TextInput::local()
                    .with_id("grayjay_pairing_invite")
                    .with_external_buffer(input)
                    .with_placeholder("grayjay:// pairing invite")
                    .with_accessible_name("Grayjay pairing invite")
                    .with_enter_submits(true),
            )
            .push(device_action(
                theme,
                bus,
                "grayjay_pair",
                "Pair device",
                Action::PairDevice,
                false,
            )),
    );
}

fn add_pending_devices(
    rows: &mut Vec<Flex>,
    theme: &Theme,
    bus: &ActionBus,
    state: &crate::sync::SyncView,
) {
    for peer in state
        .peers
        .iter()
        .filter(|peer| peer.pending_approval && peer.revoked_at.is_none())
    {
        rows.push(pending_device_row(theme, bus, peer));
    }
}

fn pending_device_row(theme: &Theme, bus: &ActionBus, peer: &crate::sync::PeerRecord) -> Flex {
    Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .pad(12.0)
        .surface(Surface {
            fill: Some(theme.bg_active),
            radius: 12.0,
            border: Some((1.0, theme.border_focus)),
        })
        .push(icon(Icon::Shield, 19.0, theme.accent_primary))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(2.0)
                .push(text("Confirm this device", 13.5))
                .push(dim(
                    theme,
                    format!("Fingerprint {} · verify it on Grayjay", peer.short_id()),
                    11.5,
                )),
        )
        .push(device_action(
            theme,
            bus,
            &format!("grayjay_approve_{}", peer.short_id()),
            "Approve",
            Action::ApproveDevice(peer.id.clone()),
            false,
        ))
}

fn add_active_devices(
    rows: &mut Vec<Flex>,
    theme: &Theme,
    bus: &ActionBus,
    state: &crate::sync::SyncView,
) {
    let active = state
        .peers
        .iter()
        .filter(|peer| peer.is_active())
        .collect::<Vec<_>>();
    if active.is_empty() {
        rows.push(
            Flex::row()
                .fill_w()
                .pad(16.0)
                .surface(subtle_surface(theme))
                .push(dim(
                    theme,
                    "No approved devices yet. Pairing requires confirmation on both devices.",
                    12.5,
                )),
        );
    } else {
        rows.push(
            Flex::row()
                .fill_w()
                .align(Align::Center)
                .push(text("Approved devices", 14.0))
                .push(spring())
                .push(device_action(
                    theme,
                    bus,
                    "grayjay_sync_now",
                    "Sync now",
                    Action::SyncDevices,
                    false,
                )),
        );
        for peer in active {
            rows.push(active_device_row(theme, bus, peer));
        }
    }
}

fn active_device_row(theme: &Theme, bus: &ActionBus, peer: &crate::sync::PeerRecord) -> Flex {
    let detail = peer.last_error.as_ref().map_or_else(
        || {
            peer.last_sync_at
                .map_or_else(|| "Not synced yet".into(), relative_sync_time)
        },
        |error| format!("Could not sync · {error}"),
    );
    Flex::row()
        .fill_w()
        .gap(12.0)
        .align(Align::Center)
        .pad(12.0)
        .surface(subtle_surface(theme))
        .push(icon(Icon::Monitor, 19.0, theme.text_dim))
        .push(
            Flex::col()
                .grow(1.0)
                .gap(2.0)
                .push(text(&peer.name, 13.5))
                .push(dim(theme, format!("{} · {detail}", peer.short_id()), 11.5)),
        )
        .push(device_action(
            theme,
            bus,
            &format!("grayjay_revoke_{}", peer.short_id()),
            "Revoke",
            Action::RevokeDevice(peer.id.clone()),
            true,
        ))
}

fn relative_sync_time(time: i64) -> String {
    let current = ui::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    format!("Last synced {}", ui::format::relative(current - time))
}

// ---------------------------------------------------------------------------
// Ctrl+K command palette
// ---------------------------------------------------------------------------

/// One result row in the command palette.
fn palette_row(
    theme: &Theme,
    bus: &ActionBus,
    hit: &crate::search_type::SearchHit,
    index: usize,
) -> Flex {
    use crate::search_type::CHANNEL_TYPE_ID;
    let is_channel = hit.type_id == CHANNEL_TYPE_ID;
    let kind = if is_channel {
        Icon::GalleryVerticalEnd
    } else {
        Icon::Play
    };
    let meta = if is_channel {
        format!("Channel · {}", hit.creator)
    } else {
        format!("{} · {}", hit.creator, clock(f64::from(hit.length_secs)))
    };
    Flex::row()
        .id(&format!("palette_row_{index}"))
        .fill_w()
        .gap(12.0)
        .pad_xy(12.0, 9.0)
        .align(Align::Center)
        .hover_surface(Surface {
            fill: Some(hover_fill(theme)),
            radius: f64::from(theme.tokens.radii.md),
            border: None,
        })
        .on_click(bus, Action::ActivateHit(hit.deep_link.clone()))
        .accessible_name(format!("Open search result: {}", hit.title))
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
        .push(icon(Icon::ChevronRight, 14.0, theme.text_dim))
}

/// Tube's in-app Ctrl+K overlay. The host-wide palette receives the canonical
/// [`ui::search::SearchProvider`] separately; this lightweight local overlay
/// keeps the current query immediate while typing. Returns `None` when closed.
pub fn command_palette(
    model: &TubeModel,
    theme: &Theme,
    bus: &ActionBus,
    palette_query: &ui::LocalState<String>,
) -> Option<Flex> {
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
    let input = TextInput::from_state(palette_query)
        .with_id("palette_input")
        .with_kind(ui::widget::TextInputKind::Search)
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
            radius: f64::from(theme.tokens.radii.md),
            border: Some((1.0, theme.border_focus)),
        })
        .push(icon(Icon::Search, 18.0, theme.accent_primary))
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
        .w(Len::Pct(0.94))
        .max_w(560.0)
        .gap(10.0)
        .pad(14.0)
        // A floating dialog: Panel role ground + border (HIG §9.5); the rule
        // between field and list derives against that ground.
        .surface(Surface {
            fill: Some(ui::chrome::panel_ground(theme)),
            radius: f64::from(theme.tokens.radii.lg),
            border: Some((1.0, ui::chrome::panel_border(theme))),
        })
        // Swallow clicks on the card chrome so they don't reach the scrim and
        // close the palette. `OpenPalette` is idempotent (already open).
        .on_click(bus, Action::OpenPalette)
        .accessible_name("Action search dialog")
        .push(field)
        .push(divider_on(ui::chrome::panel_ground(theme)))
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
            .accessible_name("Close action search")
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
    fn selected_and_active_fills_are_readable_in_every_theme() {
        // Case `selected_accent_fill_is_readable`.
        {
            for (name, t) in themes() {
                let s = selected_accent_fill(&t);
                assert!(
                    s.ratio() >= AA_BODY,
                    "[{name}] selected chip/pill contrast {:.2} < {AA_BODY}",
                    s.ratio()
                );
            }
        }
        // Case `selected_nav_item_is_readable`.
        {
            for (name, t) in themes() {
                let s = selected_nav_item(&t);
                assert!(
                    s.ratio() >= AA_BODY,
                    "[{name}] selected nav-item contrast {:.2} < {AA_BODY}",
                    s.ratio()
                );
            }
        }
        // Case `status_pill_on_is_readable`.
        {
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
}

#[cfg(test)]
mod search_screen_tests {
    use super::*;
    use crate::model::Route;
    use std::collections::HashMap;
    use std::sync::Arc;
    use taffy::prelude::*;
    use ui::widget::Widget;
    use vello_encoding::DrawTag;

    /// A tiny valid PNG (4×4 opaque green) so a test can seed the image store
    /// with a decodable thumbnail without a network.
    fn px_png() -> Vec<u8> {
        let (w, h) = (4u32, 4u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&[20, 200, 60, 255]);
        }
        ui::png::encode_rgba8(w, h, &rgba).unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn search_screen_lays_out_chips_inset_and_thumbnails() {
        // Case `search_screen_has_type_chips_and_result_context`:
        // Search preserves query/result context after the top bar scrolls away,
        // and its type-filter chips remain available to narrow by kind.
        {
            use crate::model::Route;

            fn has_id(w: &dyn Widget, id: &str) -> bool {
                if w.test_id() == Some(id) {
                    return true;
                }
                w.children().iter().any(|c| has_id(*c, id))
            }

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
                    Style {
                        size: Size {
                            width: length(w),
                            height: length(h),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();

            assert!(
                has_id(&screen, "search_type_chips"),
                "the Video·Playlist·Creator chips are present"
            );
            assert!(
                has_id(&screen, "search_results_summary"),
                "a sparse result set retains visible result-count context"
            );
            assert!(
                has_id(&screen, "sparse_video_result"),
                "a lone result uses the centered, constrained desktop composition"
            );
            for label in ["video", "playlist", "creator"] {
                assert!(
                    has_id(&screen, &format!("type_chip_{label}")),
                    "the {label} type chip is present",
                );
            }
        }
        // Case `palette_keeps_a_touch_safe_inset_at_the_narrow_breakpoint`.
        {
            fn rect_for_id(w: &dyn Widget, id: &str) -> Option<vello::kurbo::Rect> {
                if w.test_id() == Some(id) {
                    return Some(w.get_rect());
                }
                w.children()
                    .iter()
                    .find_map(|child| rect_for_id(*child, id))
            }

            let mut model = crate::sample::model();
            model.palette_open = true;
            model.palette_query = "rust".into();
            let palette_query = ui::LocalState::new(model.palette_query.clone());
            let theme = Theme::dark();
            let bus = crate::action::new_bus();
            let palette =
                command_palette(&model, &theme, &bus, &palette_query).expect("open palette");
            let palette = ui::headless::Framed::new(
                ui::State::new(),
                palette,
                ui::headless::viewport((560.0, 720.0)),
                theme,
            );
            let card = rect_for_id(&*palette, "palette_card").expect("palette card rect");
            assert!(
                card.x0 >= 16.0 && card.x1 <= 544.0,
                "card has safe side insets: {card:?}"
            );
            assert!(
                (card.width() - 526.4).abs() < 1.0,
                "94% narrow width preserves the 560px max-width composition: {card:?}"
            );
        }
        // Case `search_result_row_paints_the_fetched_thumbnail`:
        // Rendered through the real `search` screen with an image store that has
        // decoded the result's thumbnail URL, a search-result card must emit an
        // IMAGE draw (the real still), not just the gradient poster — the same
        // `video_card` image path Home/Trending use.
        {
            let fetcher: ui::image_store::Fetcher = Arc::new(|_u: &str| Some(px_png()));
            let store = ui::ImageStore::new(&fetcher);

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
            store.block_until_idle(tilo_test_env::hang_bound());
            assert!(
                store.get(&thumb).is_some(),
                "the result thumbnail decoded + cached"
            );

            let theme = Theme::dark();
            let bus = crate::action::new_bus();
            let imgs = Imgs(Some(&store));
            let screen = search(&model, &theme, &bus, imgs, "q");

            // Lay the real screen out in a definite viewport, then draw it. The
            // thumbnail resolves its decoded image lazily in `sync` (from the
            // pre-warmed store), which the production frame runs before drawing.
            let screen = ui::headless::Framed::new(
                ui::State::new(),
                screen,
                ui::headless::viewport((1280.0, 800.0)),
                theme,
            );
            let scene = ui::headless::Framed::harness(&screen).scene();
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
}
