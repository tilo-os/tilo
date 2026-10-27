//! What a frame of a conversation costs.
//!
//! A frame of the list is the measure taffy asks a leaf for and the draw that
//! follows it. Both used to lay out and shape every line the conversation held,
//! so a thread of a few hundred messages was a thread that stuttered on frames
//! no line had changed — a caret blink, a presence change, one arriving message.
//! The layout is kept now ([`ListCache`]) and only the rows in view are shaped,
//! and these tests hold the list to both: the work a frame reports is the work
//! it did, and it is the work of the screen, not of the conversation.

use super::*;
use crate::time::Duration;
use crate::widget::Widget;

/// 2026-06-30T14:05:00Z.
const NOONISH: i64 = 1_782_828_300;

/// An ordinary window's conversation: the reading column, as tall as a window.
const VIEWPORT: Rect = Rect::new(0.0, 0.0, 900.0, 800.0);

/// A list driven through the passes one frame gives it, with a scrolling
/// ancestor's viewport pushed, as a scrolled conversation has.
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
    /// A list of `lines`, laid out and read at its newest message: the content's
    /// top is above the window, as it is while you read the bottom of a thread.
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

/// `count` messages from two people over several days, long enough to wrap.
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
            })
        );
        assert_eq!(
            work.measured, 0,
            "a frame that changed nothing measures nothing: {work:?}"
        );
        assert_eq!(work.reused, count, "every line keeps its measurement");
    }
}

/// The cost of a frame is the cost of the screen: a thread ten times longer
/// measures nothing more.
#[test]
fn a_ten_times_longer_thread_measures_nothing_more() {
    let mut short = Frames::of(thread(40));
    let mut long = Frames::of(thread(400));
    short.frames(10);
    long.frames(10);
    let short_work = short.list.work();
    let long_work = long.list.work();
    assert_eq!(short_work.measured, long_work.measured);
    assert_eq!(short_work.reused, 40);
    assert_eq!(long_work.reused, 400);
}
