//! Now Playing: the song large, its cover, title, artist and album, and the
//! way to the album's and the artist's pages.
//!
//! The band's cover opens it: the cover grows from the band into the page
//! (`motion::Zoom`) while the page's lines fade in, and the same cover
//! shrinks back into the band when the page closes, by the cover again, Back
//! or anything else that leaves the page. While the cover is in flight the
//! band leaves its place empty, so one cover moves, not two. A key lands at
//! once; reduced motion fades the cover in place.

use ui::icons::Icon;
use ui::motion::{
    Direction, MotionValue, Timing, Zoom, recent_input, unit_opacity, want_frames_until,
};
use ui::paint::PaintCtx;
use ui::tokens::{EffectiveMotion, MotionSource, TextRole};
use ui::widget::{EmptyStateContent, Link, MediaSize, Song, SongFace, TransportFeed};

use ui::tilo_resource::Effect as _;

use crate::actions::{MusicEffect, Place};
use vello::peniko::BlendMode;

use super::{Action, Fill, Instant, Look, MusicApp, Rect, Theme, View, line_of};

/// The page's song and the cover's flight between the band and the page.
pub(in crate::app) struct NowPlaying {
    song: SongFace,
    /// Where the cover is: 0 in the band, 1 on the page.
    zoom: MotionValue<f64>,
    /// The band's cover when the flight began (in the window).
    band: Rect,
    /// The page's cover at its last layout (in the window).
    page: Rect,
    reduced: bool,
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self {
            song: SongFace::new(),
            zoom: MotionValue::new(0.0),
            band: Rect::ZERO,
            page: Rect::ZERO,
            reduced: false,
        }
    }
}

impl NowPlaying {
    /// Follow the page opening or closing from the frame at `now`: the
    /// cover flies from `band` to `page`, or back.
    fn follow(
        &mut self,
        open: bool,
        band: Option<Rect>,
        page: Rect,
        motion: EffectiveMotion,
        now: Instant,
    ) {
        self.page = page;
        let want = if open { 1.0 } else { 0.0 };
        if (self.zoom.target() - want).abs() < f64::EPSILON {
            return;
        }
        let Some(band) = band.filter(|_| page.width() > 0.0) else {
            self.zoom.snap(want);
            return;
        };
        if !self.zoom.is_moving(now) {
            self.band = band;
        }
        self.reduced = motion.reduced();
        let direction = if open {
            Direction::Enter
        } else {
            Direction::Exit
        };
        let source = recent_input(now).unwrap_or(MotionSource::Pointer);
        let timing = Zoom::timing(self.band, page, direction, motion, source);
        self.zoom.set(want, timing, now);
        if matches!(timing, Timing::Instant) {
            self.zoom.snap(want);
        }
        if let Some(until) = self.zoom.settles_at() {
            want_frames_until(until);
        }
    }

    /// What a screen reader says for the song on the page.
    pub(in crate::app) fn spoken(&self) -> String {
        self.song.spoken()
    }

    pub(in crate::app) fn in_flight(&self, now: Instant) -> bool {
        self.zoom.is_moving(now)
    }

    /// How far the page has come in, for its lines' fade: 1 at rest.
    fn arrived(&self, now: Instant) -> f32 {
        if self.in_flight(now) {
            Zoom::contents(self.zoom.get(now)).1
        } else {
            1.0
        }
    }

    /// The cover in flight, where it stands at `now` and how opaque it is.
    fn flight(&self, now: Instant) -> Option<(Rect, f32)> {
        if !self.in_flight(now) {
            return None;
        }
        let progress = self.zoom.get(now);
        if self.reduced {
            // Reduced motion: the cover fades on the page, nothing travels.
            return Some((self.page, unit_opacity(progress)));
        }
        Some((Zoom::rect_at(self.band, self.page, progress), 1.0))
    }
}

/// One of the page's links: a name, the library item it names (id, kind,
/// the Open's label) and that item's Open.
struct NowPlayingLink {
    name: String,
    resource: (String, &'static str, &'static str),
    effect: MusicEffect,
}

impl NowPlayingLink {
    /// The link, invoking the item's own Open: focusable, opened with Enter,
    /// a click or assistive technology alike.
    fn control(&self) -> Option<Link> {
        let (id, kind, label) = &self.resource;
        let resource = ui::tilo_actions::Resource::new(id.clone(), *kind, self.name.clone())
            .with_action(crate::actions::bound(&self.effect, label));
        let binding =
            ui::tilo_actions::ActionBinding::unit_for(&resource, self.effect.id()).ok()?;
        Some(Link::action(&self.name, binding).with_text_role(TextRole::Body))
    }
}

/// Where the page's parts stand in `body`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    art: Rect,
    /// The column the lines and the buttons share.
    text: Rect,
    /// Beside the cover (wide) or under it.
    beside: bool,
}

impl Layout {
    /// The cover as large as the page allows with room for the lines: beside
    /// them on a wide page, centred above them on a narrow one.
    fn of(body: Rect, look: &Look, theme: &Theme) -> Self {
        let inner = body.inset(-look.gutter);
        let width = inner.width().max(0.0);
        let height = inner.height().max(0.0);
        let column = MediaSize::Large.extent(theme) * 2.0;
        let beside = width >= MediaSize::Regular.extent(theme) * 4.0;
        if beside {
            let side = (width * 0.5).min(height).min(column).max(0.0);
            let top = inner.y0 + (height - side) / 2.0;
            let art = Rect::new(inner.x0, top, inner.x0 + side, top + side);
            let text_x = art.x1 + look.section;
            let text = Rect::new(text_x, art.y0, (text_x + column).min(inner.x1), art.y1);
            return Self { art, text, beside };
        }
        // Under the cover the lines need their three lines and the buttons.
        let reserve = 6.0 * look.target;
        let side = width.min(height - reserve).min(column).max(0.0);
        let left = inner.x0 + (width - side) / 2.0;
        let art = Rect::new(left, inner.y0, left + side, inner.y0 + side);
        let text = Rect::new(inner.x0, art.y1 + look.section, inner.x1, inner.y1);
        Self { art, text, beside }
    }
}

impl MusicApp {
    /// What the band's cover does: open Now Playing, or close it from it.
    pub(in crate::app) fn now_playing_toggle(&self) -> Action {
        if self.view == View::NowPlaying {
            Action::Back
        } else {
            Action::Nav(View::NowPlaying)
        }
    }

    /// The song the page paints: the band's title, and why it can't play
    /// when it can't. The artist and the album are links under it.
    fn now_playing_song(&self) -> Option<Song> {
        self.pb
            .current
            .as_deref()
            .and_then(|id| self.playback_track(id))?;
        let mut song = self.transport.song();
        if !song.error {
            song.subtitle.clear();
        }
        song.detail.clear();
        Some(song)
    }

    /// The artist's and the album's names, each with the item it names
    /// and that item's Open: the page's links, in that order.
    fn now_playing_links(&self, track: &crate::model::Track) -> Vec<NowPlayingLink> {
        let artist = NowPlayingLink {
            name: self.lib.artist_name(&track.artist_id).to_owned(),
            resource: (
                crate::actions::artist_resource_id(&track.artist_id),
                "music.artist",
                "Open artist",
            ),
            effect: MusicEffect::Open {
                place: Place::Artist(track.artist_id.clone()),
            },
        };
        let album = NowPlayingLink {
            name: self.lib.album_title(&track.album_id).to_owned(),
            resource: (
                crate::actions::album_resource_id(&track.album_id),
                "music.album",
                "Open album",
            ),
            effect: MusicEffect::Open {
                place: Place::Album(track.album_id.clone()),
            },
        };
        [(artist, &track.artist_id), (album, &track.album_id)]
            .into_iter()
            .filter(|(link, id)| !id.is_empty() && !link.name.is_empty())
            .map(|(link, _)| link)
            .collect()
    }

    /// Follow the page opening or closing, before the frame is drawn.
    pub(in crate::app) fn follow_now_playing(&mut self, theme: &Theme, now: Instant) {
        let look = Look::of(theme);
        let page = Layout::of(self.body_rect, &look, theme).art;
        let band = self.transport.art_rect();
        let open = self.view == View::NowPlaying;
        self.now_playing
            .follow(open, band, page, theme.motion(), now);
        if let Some(song) = self.now_playing_song() {
            self.now_playing.song.show(&song, theme.motion(), now);
        }
        let lent = self
            .now_playing
            .flight(now)
            .is_some_and(|_| !self.now_playing.reduced);
        self.transport.lend_art(lent);
    }

    /// The page: the cover, the lines, the way to the album and the artist.
    pub(super) fn draw_now_playing(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        look: &Look,
        body: Rect,
        y: f64,
    ) -> f64 {
        let Some(track) = self
            .pb
            .current
            .as_deref()
            .and_then(|id| self.playback_track(id))
            .cloned()
        else {
            let content = EmptyStateContent {
                icon: Some(Icon::Music),
                ..EmptyStateContent::new("Play a song to see it here.")
            };
            return self.empty_page(ctx, body, y, &content, None);
        };
        let now = ui::time::now();
        let theme = ctx.theme;
        let layout = Layout::of(body, look, theme);
        if self.now_playing.flight(now).is_none() {
            self.now_playing
                .song
                .paint_art(ctx, layout.art, Self::page_radius(theme), now);
        }

        let title = theme.text(TextRole::LargeTitle);
        let subtitle = theme.text(TextRole::Body);
        let error = self.now_playing.song.song().error;
        let song_h = line_of(ctx, &title)
            + if error {
                line_of(ctx, &subtitle) + look.tight
            } else {
                0.0
            };
        let links = self.now_playing_links(&track);
        // Each link is a line of body text.
        let nav_h = links.len() as f64 * (line_of(ctx, &subtitle) + look.tight);
        let block_h = song_h + look.related + nav_h;
        let top = if layout.beside {
            layout.text.y0 + ((layout.text.height() - block_h) / 2.0).max(0.0)
        } else {
            layout.text.y0
        };
        let song_rect = Rect::new(layout.text.x0, top, layout.text.x1, top + song_h);
        let opacity = self.now_playing.arrived(now);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            opacity,
            ctx.transform,
            &song_rect.inflate(2.0, 2.0),
        );
        self.now_playing
            .song
            .paint_lines(ctx, song_rect, Some((&title, &subtitle)), now);
        ctx.scene.pop_layer();

        // The artist and the album are the links to their pages: one
        // control each, focusable and opened with Enter like any link.
        let gap = ui::num::f32_of_f64(look.tight);
        let key = format!(
            "now-playing:{:?}",
            links
                .iter()
                .map(|link| (&link.name, &link.resource.0))
                .collect::<Vec<_>>()
        );
        let end = self.page_part(
            ctx,
            (layout.text.x0, layout.text.x1),
            song_rect.y1 + look.related,
            key,
            move |_| {
                let mut column = ui::widget::Column::new().with_no_padding().with_gap(gap);
                for link in links {
                    if let Some(control) = link.control() {
                        column = column.push(Box::new(control));
                    }
                }
                Box::new(column)
            },
        );
        // The page fits the view: it never scrolls under the band.
        let _ = y;
        end.max(layout.art.y1).min(body.y1 - look.gutter)
    }

    /// The cover in flight between the band and the page, over everything.
    pub(super) fn paint_now_playing_flight(&self, ctx: &mut PaintCtx<'_>) {
        let now = ui::time::now();
        let Some((rect, opacity)) = self.now_playing.flight(now) else {
            return;
        };
        let theme = ctx.theme;
        let progress = self.now_playing.zoom.get(now);
        let radius = Zoom::radius_at(
            TransportFeed::art_radius(theme),
            Self::page_radius(theme),
            progress,
        );
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            opacity,
            ctx.transform,
            &rect.inflate(2.0, 2.0),
        );
        self.now_playing.song.paint_art(ctx, rect, radius, now);
        ctx.scene.pop_layer();
    }

    fn page_radius(theme: &Theme) -> f64 {
        f64::from(theme.tokens.radii.panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(width: f64, height: f64) -> Rect {
        Rect::new(0.0, 0.0, width, height)
    }

    #[test]
    fn the_cover_opens_now_playing_and_closes_it_again() {
        let mut app = MusicApp::new();
        app.reload(&crate::seed::sample_document());
        app.on_nav(View::Songs);
        app.pb.current = Some("tk_g1".into());
        app.show_shell();
        assert_eq!(
            app.transport.shown().opens.as_deref(),
            Some("Open Now Playing")
        );
        let Action::Nav(view) = app.now_playing_toggle() else {
            panic!("the cover opens a view");
        };
        app.on_nav(view);
        assert_eq!(app.view, View::NowPlaying);
        app.show_shell();
        assert_eq!(
            app.transport.shown().opens.as_deref(),
            Some("Close Now Playing")
        );
        assert_eq!(app.now_playing_toggle(), Action::Back);
        app.on_back();
        assert_eq!(
            app.view,
            View::Songs,
            "closing returns where the person was"
        );
        let song = app.now_playing_song().expect("a song plays");
        assert!(!song.title.is_empty(), "{song:?}");
        let track = app.playback_track("tk_g1").expect("a track").clone();
        let links: Vec<_> = app
            .now_playing_links(&track)
            .into_iter()
            .map(|link| (link.name.clone(), link.control().is_some()))
            .collect();
        assert_eq!(
            links.len(),
            2,
            "the artist and the album are links: {links:?}"
        );
        assert!(
            links.iter().all(|(name, bound)| !name.is_empty() && *bound),
            "each opens its page: {links:?}"
        );
    }

    #[test]
    fn a_wide_page_sets_the_lines_beside_the_cover_and_a_narrow_one_under_it() {
        let theme = Theme::dark();
        let look = Look::of(&theme);
        let wide = Layout::of(body(1200.0, 700.0), &look, &theme);
        assert!(wide.beside);
        assert!(
            wide.text.x0 > wide.art.x1,
            "the lines stand beside the cover"
        );
        assert!((wide.art.width() - wide.art.height()).abs() < 1e-9);
        let narrow = Layout::of(body(390.0, 700.0), &look, &theme);
        assert!(!narrow.beside);
        assert!(narrow.text.y0 > narrow.art.y1, "the lines stand under it");
        assert!(
            (narrow.art.center().x - 195.0).abs() < 1e-9,
            "the cover is centred"
        );
    }

    #[test]
    fn the_cover_flies_into_the_page_and_back() {
        let theme = Theme::dark();
        let now = ui::time::now();
        let band = Rect::new(10.0, 600.0, 50.0, 640.0);
        let page = Rect::new(100.0, 100.0, 400.0, 400.0);
        let mut np = NowPlaying::default();
        ui::motion::note_source(MotionSource::Pointer);
        np.follow(true, Some(band), page, theme.motion(), now);
        let (start, _) = np.flight(now).expect("the cover is in flight");
        assert!(
            (start.width() - band.width()).abs() < 1.0,
            "it starts as the band's cover"
        );
        let settled = np.zoom.settles_at().expect("it lands");
        assert!(np.flight(settled).is_none(), "and lands on the page");
        np.follow(false, Some(band), page, theme.motion(), settled);
        assert!(np.in_flight(settled), "closing flies it back");
    }

    #[test]
    fn a_key_opens_the_page_at_once() {
        let theme = Theme::dark();
        let now = ui::time::now();
        let mut np = NowPlaying::default();
        ui::motion::note_source(MotionSource::Keyboard);
        np.follow(
            true,
            Some(Rect::new(0.0, 0.0, 40.0, 40.0)),
            Rect::new(100.0, 100.0, 400.0, 400.0),
            theme.motion(),
            now,
        );
        assert!(!np.in_flight(now));
    }

    #[test]
    fn without_the_band_cover_the_page_simply_opens() {
        let theme = Theme::dark();
        let now = ui::time::now();
        let mut np = NowPlaying::default();
        ui::motion::note_source(MotionSource::Pointer);
        np.follow(
            true,
            None,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            theme.motion(),
            now,
        );
        assert!(!np.in_flight(now));
    }
}
