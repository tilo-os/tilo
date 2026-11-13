//! What a frame of a conversation costs.

use super::*;
use crate::time::Duration;
use crate::widget::Widget;

const NOONISH: i64 = 1_782_828_300;

const VIEWPORT: Rect = Rect::new(0.0, 0.0, 900.0, 800.0);

struct Frames {
    list: ConversationList,
    text: TextRenderer,
    theme: Theme,
    scene: Scene,
    presence: PresenceCtx,
    width: f32,
    top: f64,
    height: f64,
}

impl Frames {
    fn of(lines: Vec<ConversationLine>) -> Self {
        let theme = Theme::dark();
        let mut list = ConversationList::new(lines);
        list.theme = Some(theme.clone());
        let presence = PresenceCtx::empty();
        presence.push_viewport(VIEWPORT);
        let mut frames = Self {
            list,
            text: crate::font_catalog::fixture_renderer(),
            theme,
            scene: Scene::new(),
            presence,
            width: 900.0,
            top: 0.0,
            height: 0.0,
        };
        frames.frame();
        frames.top = VIEWPORT.height() - frames.height;
        frames.frame();
        frames
    }

    fn frame(&mut self) {
        let known = taffy::Size {
            width: Some(self.width),
            height: None,
        };
        let available = taffy::Size {
            width: taffy::AvailableSpace::Definite(self.width),
            height: taffy::AvailableSpace::MaxContent,
        };
        let measured = self.list.measure(known, available, &mut self.text);
        self.height = f64::from(measured.height);
        self.list.rect = Rect::new(0.0, self.top, f64::from(self.width), self.top + self.height);
        self.scene.reset();
        self.list.draw(
            &mut self.scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }

    fn frames(&mut self, count: u32) -> Duration {
        let started = crate::time::now();
        for _ in 0..count {
            self.frame();
        }
        started.elapsed() / count
    }
}

fn thread(count: usize) -> Vec<ConversationLine> {
    (0..count)
        .map(|index| {
            let at_ms = NOONISH
                .saturating_add(i64::try_from(index).unwrap_or_default() * 60)
                .saturating_add(i64::try_from(index / 50).unwrap_or_default() * 86_400)
                .saturating_mul(1_000);
            let body = format!(
                "Message {index}: the call overlay mock is ready for a look — the host tile keeps \
                 the live camera and everyone else collapses to chips."
            );
            if index % 3 == 0 {
                ConversationLine::outgoing(format!("m{index}"), at_ms, body)
            } else {
                ConversationLine::incoming(format!("m{index}"), at_ms, "Ada Chen", body)
            }
        })
        .collect()
}

/// The rows a viewport shows, at most: a window of 800 points holds fewer than
/// this many rows of any text scale the tokens allow.
const MOST_ROWS_IN_VIEW: usize = 60;

#[test]
fn a_steady_frame_measures_nothing_and_paints_only_what_is_in_view() {
    for count in [40_usize, 400] {
        let mut frames = Frames::of(thread(count));
        let steady = frames.frames(30);
        let work = frames.list.work();
        eprintln!(
            "conversation-frame {}",
            serde_json::json!({
                "lines": count,
                "profile": if cfg!(debug_assertions) { "test-debug" } else { "optimized" },
                "frame_us": steady.as_micros(),
                "total_height": frames.height,
                "measured": work.measured,
                "reused": work.reused,
                "painted": work.painted,
                "culled": work.culled,
            })
        );
        assert_eq!(
            work.measured, 0,
            "a frame that changed nothing measures nothing: {work:?}"
        );
        assert_eq!(work.reused, count, "every line keeps its measurement");
        assert!(
            work.painted <= MOST_ROWS_IN_VIEW,
            "a frame paints what is on screen, not the thread: {work:?}"
        );
        assert_eq!(
            work.painted + work.culled,
            count + count.div_ceil(50),
            "every row — a line or a day divider — is either painted or culled: {work:?}"
        );
    }
}

#[test]
fn a_ten_times_longer_thread_costs_the_same_frame() {
    let mut short = Frames::of(thread(40));
    let mut long = Frames::of(thread(400));
    short.frames(10);
    long.frames(10);
    let short_work = short.list.work();
    let long_work = long.list.work();
    assert_eq!(short_work.measured, long_work.measured);
    assert_eq!(short_work.reused, 40);
    assert_eq!(long_work.reused, 400);
    assert_eq!(short_work.painted, long_work.painted);
}
