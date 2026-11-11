//! A mount shown as HTML (`mountDesktop(el, {dom: true})`): each frame's
//! scene goes to the page's DOM presenter (`web/dom-present.mjs`) as DOM
//! operations instead of to the GPU. Layout, input, semantics and records
//! take the same paths as a frame drawn on the canvas.
//!
//! Three things differ from a drawn frame:
//! - widgets mark their draws with keys (`ui::scene_parts::key_widgets`,
//!   with the code that comes with the presenter's), so
//!   the page keeps one element per widget from frame to frame;
//! - the scroll regions the frame drew go along, and the page shows each as
//!   a native scroll container; where the person scrolls it comes back
//!   through [`dom_scrolled`];
//! - a frame an input asks for is presented before the browser's next
//!   paint, in a task right after the input's own
//!   ([`Mount::present_after_input`]), not at the next animation frame.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use js_sys::Function;
use wasm_bindgen::prelude::*;

use super::{FrameChange, Mount, with_mount, with_mount_quietly};

#[wasm_bindgen(inline_js = r#"
let channel, queued = [];
export function afterTask(work) {
    if (!channel) {
        channel = new MessageChannel();
        channel.port1.onmessage = () => { const run = queued; queued = []; for (const work of run) work(); };
    }
    queued.push(work);
    if (queued.length === 1) channel.port2.postMessage(0);
}
"#)]
extern "C" {
    /// Run `work` in a task of its own right after the current one: after
    /// every event of one key press (its keydown and the text it commits),
    /// before the browser paints again.
    #[wasm_bindgen(js_name = afterTask)]
    fn after_task(work: &js_sys::Function);
}

thread_local! {
    /// The presenter `present_as_dom` handed over, for the next mount.
    static PRESENTER: RefCell<Option<Presenter>> = const { RefCell::new(None) };
    /// Each mount shown as HTML, by id.
    static MOUNTS: RefCell<HashMap<u64, Shown>> = RefCell::new(HashMap::new());
    /// Mounts with a frame queued to present after the input in hand.
    static QUEUED: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    /// What presents them: one function for the page's whole life.
    static RUN_QUEUED: Closure<dyn FnMut()> = Closure::new(present_queued);
}

/// Present the frame of every mount an input queued one for.
fn present_queued() {
    let ids: Vec<u64> = QUEUED.with_borrow_mut(|queued| queued.drain().collect());
    for id in ids {
        let result = with_mount_quietly(id, |m| {
            if let Err(error) = m.draw_dom() {
                m.frame_failed(id, error);
            }
            Ok(())
        });
        if let Err(error) = result {
            // The runtime was busy: the animation frame draws it.
            web_sys::console::debug_1(&error);
            let _ = with_mount_quietly(id, |m| {
                m.window.request_redraw();
                Ok(())
            });
        }
    }
}

#[derive(Clone)]
struct Presenter {
    present: Function,
}

/// What one mount shown as HTML keeps between frames.
struct Shown {
    presenter: Presenter,
    /// The code's half, once it is in.
    code: Option<Box<dyn crate::dom_code::DomPresent>>,
    /// The theme generation whose variables the page holds.
    theme_published: Option<u64>,
}

/// Show the next mount made as HTML rather than on a WebGPU canvas: each
/// frame calls `present(meta, ops, fonts)` with the frame's DOM operations
/// (`tilo_cpu_raster::dom`), and text in the scene carries its characters.
///
/// The theme is the page's, whichever way a mount shows it
/// ([`crate::page_theme`]): the page starts following the browser's light
/// or dark before anything mounts, so no theme is set here.
#[wasm_bindgen]
pub fn present_as_dom(present: Function) {
    ui::text_render::present_text(true);
    PRESENTER.with_borrow_mut(|slot| *slot = Some(Presenter { present }));
}

/// Whether the mount being made now is to be shown as HTML.
pub(super) fn presenting() -> bool {
    PRESENTER.with_borrow(Option::is_some)
}

/// Mount `id` takes the presenter `present_as_dom` handed over.
pub(super) fn claim(id: u64) {
    if let Some(presenter) = PRESENTER.with_borrow_mut(Option::take) {
        MOUNTS.with_borrow_mut(|mounts| {
            mounts.insert(
                id,
                Shown {
                    presenter,
                    code: None,
                    theme_published: None,
                },
            );
        });
    }
}

/// Whether mount `id` is shown as HTML.
pub(super) fn shows(id: u64) -> bool {
    MOUNTS.with_borrow(|mounts| mounts.contains_key(&id))
}

/// Mount `id` closed.
pub(super) fn forget(id: u64) {
    MOUNTS.with_borrow_mut(|mounts| mounts.remove(&id));
    QUEUED.with_borrow_mut(|queued| queued.remove(&id));
}

/// The person scrolled the region keyed `key` (16 hex digits) of mount
/// `id` to `offset` CSS pixels: the widget that drew it scrolls there, and
/// the next frame draws what came into view. Returns whether the region is
/// still there.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn dom_scrolled(id: u64, key: &str, offset: f64) -> Result<bool, JsValue> {
    let Ok(key) = u64::from_str_radix(key, 16) else {
        return Ok(false);
    };
    with_mount(id, |m| {
        let found = MOUNTS.with_borrow(|mounts| {
            mounts
                .get(&id)
                .and_then(|shown| shown.code.as_ref())
                .is_some_and(|code| code.scrolled(m.frame.root_widget_mut(), key, offset))
        });
        if found {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(found)
    })
}

impl Mount {
    /// Whether the mount can present a frame: on its GPU surface, or as
    /// HTML.
    pub(super) fn presents(&self) -> bool {
        self.gpu.is_some() || shows(self.id)
    }

    /// Present the frame an input just asked for before the browser paints
    /// again: in a task right after the input's own, so that the key and
    /// the text it commits (two events of one task) share one frame. A
    /// mount drawn on the canvas waits for its animation frame.
    pub(super) fn present_after_input(&mut self) {
        self.pending = true;
        if !shows(self.id) {
            self.window.request_redraw();
            return;
        }
        let first = QUEUED.with_borrow_mut(|queued| {
            let first = queued.is_empty();
            queued.insert(self.id);
            first
        });
        if first {
            RUN_QUEUED.with(|run| after_task(run.as_ref().unchecked_ref()));
        }
    }

    /// Lay the frame out for [`Self::draw_dom`]: the viewport, the theme,
    /// who is here and the widgets. Returns whether the layout settled; a
    /// frame that did not is skipped.
    fn lay_out_dom(&mut self, replica: bool, theme_changed: bool) -> Result<bool, String> {
        let frame = &mut self.frame;
        let mut unsettled = None;
        self.viewport
            .apply_for_acquired_texture(Some(()), |viewport| {
                frame.set_input_scale(viewport.dpr)?;
                unsettled = frame.resize(viewport.logical_size()).err();
                Ok::<_, String>(())
            })?;
        let mut host_changed = replica;
        if theme_changed {
            self.theme_seen = crate::page_theme::generation();
            self.frame.set_theme(crate::page_theme::current());
            host_changed = true;
        }
        self.place_here();
        self.place_presence();
        self.frame.set_pan_replay(false);
        ui::scene_parts::begin_frame();
        let polled = match self.frame.poll_frame() {
            Ok(polled) => polled,
            Err(error) => {
                unsettled = Some(error);
                false
            }
        };
        if polled {
            self.note_measured();
        }
        if host_changed && !polled && unsettled.is_none() {
            unsettled = self
                .frame
                .relayout_at(self.viewport.requested().logical_size())
                .err();
        }
        self.save_desktop();
        if let Some(peer) = self.peer {
            crate::replication::publish(peer);
        }
        if let Some(error) = unsettled {
            if let Some(line) = self.frame.skip_frame(&error) {
                web_sys::console::error_1(&JsValue::from_str(&line));
            }
            self.pending = false;
            self.held = "a layout that did not settle";
            return Ok(false);
        }
        Ok(true)
    }

    /// Whether a frame is due, noted in `pending`; returns whether a peer's
    /// change arrived and whether the theme changed since the last frame.
    fn note_dom_frame_wanted(&mut self) -> (bool, bool) {
        let schedule = self.frame.frame_schedule();
        let due = schedule
            .next_wake_deadline
            .is_some_and(|wake| wake <= ui::time::now());
        let theme_changed = self.theme_seen != crate::page_theme::generation();
        let replica = self.replica_changed.replace(false);
        if let Some(peer) = self.peer {
            crate::replication::follow_answers(peer);
        }
        self.pending |= self.viewport.needs_present()
            || schedule.queued_work
            || schedule.needs_animation_frame
            || due
            || theme_changed
            || replica;
        (replica, theme_changed)
    }

    /// Lay out and present one frame as HTML, with the presentation's code
    /// ([`draw_dom_frame`]) once it is in.
    pub(super) fn draw_dom(&mut self) -> Result<(), String> {
        QUEUED.with_borrow_mut(|queued| queued.remove(&self.id));
        let Some(code) = crate::dom_code::DOM_CODE.get() else {
            // Fetched now; its arrival draws every mount again.
            self.held = "the HTML presenter's code";
            return Ok(());
        };
        (code.draw)(self)
    }
}

/// Lay out and present one frame of `m` as HTML: the scene goes to the
/// page's DOM presenter as operations ([`tilo_cpu_raster::dom`]) instead of
/// to the GPU. Input, semantics and records take the same paths as a drawn
/// frame. Part of the presentation's code (`crate::dom_code`), reached only
/// through it.
pub(crate) fn draw_dom_frame(m: &mut Mount) -> Result<(), String> {
    m.draw_dom_frame()
}

impl Mount {
    fn draw_dom_frame(&mut self) -> Result<(), String> {
        let (replica, theme_changed) = self.note_dom_frame_wanted();
        if !self.pending {
            self.held = "nothing to draw";
            return Ok(());
        }
        if !ui::scene_parts::keys_widgets() {
            ui::scene_parts::key_widgets(Some(ui::scene_parts::keyer()));
        }
        let started = web_time::Instant::now();
        if !self.lay_out_dom(replica, theme_changed)? {
            return Ok(());
        }
        let layout_ms = started.elapsed().as_secs_f64() * 1000.0;
        let input = self.frame.frame_render_input();
        let theme_generation = self.theme_seen;
        let viewport = self.viewport.requested().logical_size();
        let presented = MOUNTS.with_borrow_mut(|mounts| {
            let shown = mounts.get_mut(&self.id)?;
            let publish_theme = shown.theme_published != Some(theme_generation);
            shown.theme_published = Some(theme_generation);
            shown
                .code
                .get_or_insert_with(crate::dom_code::presenter)
                .present(crate::dom_code::Frame {
                    present: &shown.presenter.present,
                    id: self.id,
                    scene: input.scene,
                    background: input.background,
                    viewport,
                    layout_ms,
                    theme: publish_theme.then(|| self.frame.theme()),
                });
            Some(())
        });
        if presented.is_none() {
            self.held = "no presenter";
            return Ok(());
        }
        self.viewport.did_present();
        self.pending = false;
        self.frames = self.frames.saturating_add(1);
        self.presentation.set(true);
        self.publish_frame(0, FrameChange::Other, true);
        self.presentation.set(false);
        Ok(())
    }
}
