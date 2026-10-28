//! The conversation's rows, kept from frame to frame.
//!
//! Every message is measured once and keeps its height until something it
//! is measured from changes (its content, the column's width, the theme);
//! the timeline's total height is then a sum, not a re-layout. Only the rows
//! in view are painted, each into a scene of its own drawn where the row
//! stands at the top of the content (`y = 0`), and kept with the targets and
//! texts it placed. A frame that only scrolls, or one where only the
//! composer changed, appends those scenes moved to where the rows stand now:
//! it lays out and draws nothing but the rows that came into view.
//!
//! A kept row is reused only when everything it was painted from is as it
//! was: the look of the whole column ([`PaintLook`]), the row's own state
//! ([`ChatApp::row_paint_key`]) and, for each place it asked whether the
//! pointer was over, the same answer where the row stands now. A row whose
//! picture is still arriving, that shares a window live, or that is being
//! edited is painted afresh every frame it shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::theme::Theme;

use crate::app::{ChatApp, Hit, PaintedMessage};
use crate::model::{self, Message};
use crate::render_chat::{MessageColumn, RowM};

/// How many rows keep their scenes at most; past it, the ones painted
/// longest ago are dropped (and painted again if they come back).
const KEPT_SCENES: usize = 160;

/// What the timeline did in its last frame, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimelineCounts {
    /// Rows measured (laid out) because no kept height applied.
    pub(crate) measured: usize,
    /// Rows painted afresh.
    pub(crate) drawn: usize,
    /// Rows shown from their kept scenes.
    pub(crate) reused: usize,
}

/// What every row's paint depends on.
#[derive(Clone, PartialEq)]
struct PaintLook {
    theme: Theme,
    transform: Affine,
    column: [u64; 4],
    mentions: u64,
    people: u64,
    me: String,
    menu_open: bool,
    emoji_open: bool,
    participate: bool,
    i18n: u64,
}

/// What every row's height depends on.
#[derive(Clone, PartialEq)]
struct MeasureLook {
    theme: Theme,
    text_width: u32,
    participate: bool,
    i18n: u64,
}

/// A row's kept paint, drawn with the row's top at `y = 0`.
struct RowPaint {
    key: u64,
    scene: Scene,
    hits: Vec<(Rect, Hit)>,
    /// Where the row asked whether the pointer was over it, and the answer.
    probes: Vec<(Rect, bool)>,
    painted: PaintedMessage,
    /// The frame it was last shown in.
    used: u64,
}

#[derive(Default)]
struct KeptRow {
    measured: Option<(u64, RowM)>,
    paint: Option<RowPaint>,
}

/// The kept rows of one conversation ([`ChatApp::timeline`]).
#[derive(Default)]
pub(crate) struct Timeline {
    /// The messages the fingerprints were taken of: their revision, where
    /// they lie and how many there are.
    seen: Option<(u64, usize, usize)>,
    /// Each message as last seen, and a number that changes whenever it
    /// does: what a kept row is checked against.
    fingerprints: HashMap<String, (Message, u64)>,
    versions: u64,
    /// Each thread's replies, fingerprinted, by the message they answer.
    threads: HashMap<String, u64>,
    groups: Vec<model::MessageGroup>,
    measure_look: Option<MeasureLook>,
    /// The frame, column width and theme [`Self::measure_look`] was last checked
    /// for: nothing it holds changes within a frame, so each row of that
    /// frame need not build and compare it again.
    measure_checked: Option<(u64, u32, usize)>,
    paint_look: Option<PaintLook>,
    rows: HashMap<String, KeptRow>,
    frame: u64,
    counts: TimelineCounts,
    /// The rows the last frame showed, top to bottom.
    shown: Vec<String>,
    /// While a row paints: how far it is moved on screen, and where it
    /// asked about the pointer.
    probing: RefCell<Option<Probing>>,
}

/// Where a row being painted into its kept scene asked about the pointer.
struct Probing {
    /// How far the row stands down the view.
    offset: Vec2,
    /// Each place asked about, in the row's frame, and the answer.
    probes: Vec<(Rect, bool)>,
}

impl PaintedMessage {
    /// The same, moved by `offset`.
    fn moved(self, offset: Vec2) -> Self {
        let text = |(rect, text): (Rect, String)| (rect + offset, text);
        Self {
            avatar: self.avatar.map(|rect| rect + offset),
            author: self.author.map(text),
            service: self.service.map(text),
            time: self.time.map(text),
            lines: self.lines.into_iter().map(text).collect(),
        }
    }
}

impl Timeline {
    /// What the last frame counted.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> TimelineCounts {
        self.counts
    }

    /// The rows the last frame showed, top to bottom.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> &[String] {
        &self.shown
    }

    /// Fingerprint the messages again when they changed.
    fn refresh(&mut self, revision: u64, messages: &[Message]) {
        let seen = (revision, messages.as_ptr().addr(), messages.len());
        if self.seen == Some(seen) {
            return;
        }
        self.seen = Some(seen);
        self.threads.clear();
        let mut was = std::mem::take(&mut self.fingerprints);
        for m in messages {
            // An unchanged message keeps its number, and the copy kept.
            let (kept, fingerprint) = match was.remove(&m.id) {
                Some((kept, version)) if kept == *m => (kept, version),
                _ => {
                    self.versions += 1;
                    (m.clone(), self.versions)
                }
            };
            if let Some(parent) = &m.parent {
                let thread = self.threads.entry(parent.clone()).or_default();
                *thread = thread.rotate_left(5) ^ fingerprint;
            }
            self.fingerprints.insert(m.id.clone(), (kept, fingerprint));
        }
        self.groups = model::group_messages(messages);
        let fingerprints = &self.fingerprints;
        self.rows.retain(|id, _| fingerprints.contains_key(id));
    }

    fn fingerprint(&self, id: &str) -> u64 {
        self.fingerprints
            .get(id)
            .map_or(0, |(_, fingerprint)| *fingerprint)
    }

    fn thread(&self, id: &str) -> u64 {
        self.threads.get(id).copied().unwrap_or_default()
    }
}

impl ChatApp {
    /// The message groups of the conversation, grouped once per change of
    /// its messages, and the timeline's counts restarted for a frame.
    pub(crate) fn timeline_groups(&self) -> Vec<model::MessageGroup> {
        let mut timeline = self.timeline.borrow_mut();
        timeline.refresh(self.messages_revision, &self.messages);
        timeline.frame += 1;
        timeline.counts = TimelineCounts::default();
        timeline.shown.clear();
        timeline.groups.clone()
    }

    /// `m`'s row measured as [`Self::measure_row`] does, from its kept
    /// height when nothing it is measured from changed.
    pub(crate) fn measure_row_kept(
        &self,
        tr: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        m: &Message,
        show_header: bool,
        text_w: f32,
    ) -> RowM {
        if self.editing.as_ref() == Some(&m.id) {
            self.timeline.borrow_mut().counts.measured += 1;
            return self.measure_row(tr, theme, m, show_header, text_w);
        }
        let key = {
            let mut timeline = self.timeline.borrow_mut();
            timeline.refresh(self.messages_revision, &self.messages);
            let checked = (
                timeline.frame,
                text_w.to_bits(),
                std::ptr::from_ref(theme).addr(),
            );
            if timeline.measure_checked != Some(checked) {
                timeline.measure_checked = Some(checked);
                let look = MeasureLook {
                    theme: theme.clone(),
                    text_width: text_w.to_bits(),
                    participate: self.can_participate(),
                    i18n: ui::i18n::I18nManager::revision(),
                };
                if timeline.measure_look.as_ref() != Some(&look) {
                    timeline.measure_look = Some(look);
                    timeline.rows.clear();
                }
            }
            let mut hasher = std::hash::DefaultHasher::new();
            timeline.fingerprint(&m.id).hash(&mut hasher);
            show_header.hash(&mut hasher);
            (timeline.thread(&m.id) != 0).hash(&mut hasher);
            self.expanded_reasoning.contains(&m.id).hash(&mut hasher);
            if let Some(att) = &m.attachment {
                self.embed_allowed(att.blob()).hash(&mut hasher);
            }
            self.is_my_live_share(m).hash(&mut hasher);
            let key = hasher.finish();
            if let Some((kept, rm)) = timeline
                .rows
                .get(&m.id)
                .and_then(|row| row.measured.as_ref())
                && *kept == key
            {
                return rm.clone();
            }
            timeline.counts.measured += 1;
            key
        };
        let rm = self.measure_row(tr, theme, m, show_header, text_w);
        let mut timeline = self.timeline.borrow_mut();
        let row = timeline.rows.entry(m.id.clone()).or_default();
        row.measured = Some((key, rm.clone()));
        // A row measured anew paints anew.
        row.paint = None;
        rm
    }

    /// Start painting the column: forget every kept scene when what they
    /// all depend on changed.
    pub(crate) fn begin_timeline_paint(&self, theme: &Theme, tf: Affine, column: MessageColumn) {
        let mut people = std::hash::DefaultHasher::new();
        for user in &self.users {
            (&user.id, &user.name).hash(&mut people);
        }
        let mut colors: Vec<_> = self.room_colors.iter().collect();
        colors.sort_unstable();
        for color in &colors {
            color.hash(&mut people);
        }
        let mut agents: Vec<_> = self.agent_authors.iter().collect();
        agents.sort_unstable();
        for agent in &agents {
            agent.hash(&mut people);
        }
        self.room_agents.hash(&mut people);
        let look = PaintLook {
            theme: theme.clone(),
            transform: tf,
            column: [
                column.area.x0.to_bits(),
                column.area.x1.to_bits(),
                column.text_x.to_bits(),
                u64::from(column.text_width.to_bits()),
            ],
            mentions: self.mention_paint(theme).0,
            people: people.finish(),
            me: self.me.id.clone(),
            menu_open: self.menu.is_some(),
            emoji_open: self.emoji_open,
            participate: self.can_participate(),
            i18n: ui::i18n::I18nManager::revision(),
        };
        let mut timeline = self.timeline.borrow_mut();
        if timeline.paint_look.as_ref() != Some(&look) {
            timeline.paint_look = Some(look);
            for row in timeline.rows.values_mut() {
                row.paint = None;
            }
        }
    }

    /// Drop the scenes of the rows painted longest ago, past
    /// [`KEPT_SCENES`].
    pub(crate) fn end_timeline_paint(&self) {
        let mut timeline = self.timeline.borrow_mut();
        let mut used: Vec<u64> = timeline
            .rows
            .values()
            .filter_map(|row| row.paint.as_ref().map(|paint| paint.used))
            .collect();
        if used.len() <= KEPT_SCENES {
            return;
        }
        used.sort_unstable_by(|a, b| b.cmp(a));
        let oldest_kept = used.get(KEPT_SCENES - 1).copied().unwrap_or_default();
        for row in timeline.rows.values_mut() {
            if row
                .paint
                .as_ref()
                .is_some_and(|paint| paint.used < oldest_kept)
            {
                row.paint = None;
            }
        }
    }

    /// Whether the pointer is over `rect`, as a row painting sees it: in
    /// the row's own frame while it paints into its kept scene, noting the
    /// question and the answer so a later frame knows when to paint the row
    /// again.
    pub(crate) fn pointer_over(&self, rect: Rect) -> bool {
        let timeline = self.timeline.borrow();
        let mut probing = timeline.probing.borrow_mut();
        match probing.as_mut() {
            Some(probing) => {
                let over = (rect + probing.offset).contains(self.last_mouse);
                probing.probes.push((rect, over));
                over
            }
            None => rect.contains(self.last_mouse),
        }
    }

    /// Where the pointer is in the frame the row being painted draws in.
    pub(crate) fn pointer_in_row(&self) -> Point {
        let timeline = self.timeline.borrow();
        let probing = timeline.probing.borrow();
        probing
            .as_ref()
            .map_or(self.last_mouse, |probing| self.last_mouse - probing.offset)
    }

    /// Note that the row being painted looked at where the pointer is,
    /// over each of `rects`.
    pub(crate) fn note_pointer_probes(&self, rects: impl IntoIterator<Item = Rect>) {
        for rect in rects {
            let _ = self.pointer_over(rect);
        }
    }

    /// Whether `m`'s row cannot be kept: it changes on its own from frame
    /// to frame (a picture arriving, a window shared live) or is the
    /// editor.
    fn row_is_live(&self, m: &Message, rm: &RowM) -> bool {
        rm.editing
            || m.share.as_ref().is_some_and(|share| !share.ended)
            || match &m.attachment {
                Some(crate::model::Attachment::Image { blob, .. }) => {
                    self.embed_allowed(blob)
                        && !blob.starts_with(crate::images::DEMO_SCHEME)
                        && self.images.get(&crate::images::blob_key(blob)).is_none()
                }
                _ => false,
            }
    }

    /// Everything about `m` its row's paint depends on beyond the column's
    /// look.
    fn row_paint_key(&self, m: &Message, rm: &RowM, kept_measure: u64) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        kept_measure.hash(&mut hasher);
        rm.show_header.hash(&mut hasher);
        rm.height().to_bits().hash(&mut hasher);
        (self.revealed_message.as_deref() == Some(m.id.as_str())).hash(&mut hasher);
        self.row_lifted(&m.id).hash(&mut hasher);
        (self.thread_open.as_deref() == Some(m.id.as_str())).hash(&mut hasher);
        self.expanded_reasoning.contains(&m.id).hash(&mut hasher);
        if let Some(range) = &self.revealed_range
            && range.0 == m.id
        {
            range.hash(&mut hasher);
        }
        self.timeline.borrow().thread(&m.id).hash(&mut hasher);
        if rm.show_header {
            self.message_author_name(m).hash(&mut hasher);
            self.shown_avatar(m).is_some().hash(&mut hasher);
        }
        match &m.attachment {
            Some(crate::model::Attachment::Image { blob, .. }) => {
                self.embed_allowed(blob).hash(&mut hasher);
                self.images
                    .get(&crate::images::blob_key(blob))
                    .is_some()
                    .hash(&mut hasher);
            }
            Some(crate::model::Attachment::Video { blob, .. }) => {
                self.embed_allowed(blob).hash(&mut hasher);
                self.video_poster(blob).is_some().hash(&mut hasher);
            }
            Some(att) => self.embed_allowed(att.blob()).hash(&mut hasher),
            None => {}
        }
        if m.reference.is_some() {
            self.resource_navigation
                .label(&m.id, false)
                .hash(&mut hasher);
            match self.resource_navigation.status(&m.id) {
                None => 0.hash(&mut hasher),
                Some(Ok(())) => 1.hash(&mut hasher),
                Some(Err(why)) => why.hash(&mut hasher),
            }
        }
        hasher.finish()
    }

    /// Paint the row `rm` into a scene of its own, at the content's top,
    /// and keep it under `key` with what it placed and where it asked about
    /// the pointer (the row standing `offset` down the view).
    fn keep_row_paint(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        rm: &RowM,
        column: MessageColumn,
        (offset, key): (Vec2, u64),
    ) {
        let PaintCtx {
            transform,
            text,
            theme,
            ..
        } = paint;
        let mut drawn = Scene::new();
        let mut hits = Vec::new();
        *self.timeline.get_mut().probing.get_mut() = Some(Probing {
            offset,
            probes: Vec::new(),
        });
        // Only `x` and the clip's sides of the column hold at the top.
        let local = MessageColumn {
            area: Rect::new(column.area.x0, -1.0e9, column.area.x1, 1.0e9),
            ..column
        };
        let painted = self.draw_row(
            PaintCtx::new(&mut drawn, transform, text, theme),
            presence,
            rm,
            local,
            &mut 0.0,
            &mut hits,
        );
        let timeline = self.timeline.get_mut();
        let probes = timeline
            .probing
            .get_mut()
            .take()
            .map(|probing| probing.probes)
            .unwrap_or_default();
        timeline.counts.drawn += 1;
        let used = timeline.frame;
        timeline.rows.entry(rm.idx_id.clone()).or_default().paint = Some(RowPaint {
            key,
            scene: drawn,
            hits,
            probes,
            painted,
            used,
        });
    }

    /// Paint the row `rm` with its top at `*y`, from its kept scene when
    /// nothing it was painted from changed; advances `*y` past it.
    pub(crate) fn paint_row(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        rm: &RowM,
        column: MessageColumn,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let height = rm.height();
        let Some(m) = self.message(&rm.idx_id).cloned() else {
            *y += height;
            return;
        };
        self.timeline.get_mut().shown.push(m.id.clone());
        // Where the row stands whole, kept or drawn: what the pointer is
        // over when it is over this message ([`Self::message_under`]).
        let inset = f64::from(theme.tokens.spacing.s2);
        let row = Rect::new(
            column.area.x0 + inset,
            *y,
            column.area.x1 - inset,
            *y + height,
        )
        .intersect(column.area);
        if !rm.editing && row.height() > 0.0 {
            self.message_rows.push((row, m.id.clone()));
        }
        if m.reference.is_some() && m.share.is_none() && rm.resource_h > 0.0 {
            self.resource_navigation.shown(&m.id);
        }
        if self.row_is_live(&m, rm) {
            let mut at = *y;
            let parts = self.draw_row(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                rm,
                column,
                &mut at,
                hits,
            );
            self.timeline.borrow_mut().counts.drawn += 1;
            self.note_painted(&m.id, parts, column.area);
            *y += height;
            return;
        }
        // The row lands on a whole device pixel, so a kept scene moved
        // there paints as one drawn there would.
        let scale = tf.as_coeffs()[3].abs().max(f64::EPSILON);
        let top = (*y * scale).round() / scale;
        let offset = Vec2::new(0.0, top);
        let kept_measure = {
            let timeline = self.timeline.borrow();
            timeline
                .rows
                .get(&m.id)
                .and_then(|row| row.measured.as_ref())
                .map_or(0, |(key, _)| *key)
        };
        let key = self.row_paint_key(&m, rm, kept_measure);
        let reusable = self
            .timeline
            .borrow()
            .rows
            .get(&m.id)
            .and_then(|row| row.paint.as_ref())
            .is_some_and(|kept| {
                kept.key == key
                    && kept
                        .probes
                        .iter()
                        .all(|(rect, over)| (*rect + offset).contains(self.last_mouse) == *over)
            });
        if reusable {
            self.timeline.get_mut().counts.reused += 1;
        } else {
            self.keep_row_paint(
                PaintCtx::new(scene, tf, tr, theme),
                presence,
                rm,
                column,
                (offset, key),
            );
        }
        let timeline = self.timeline.get_mut();
        let frame = timeline.frame;
        let Some(kept) = timeline
            .rows
            .get_mut(&m.id)
            .and_then(|row| row.paint.as_mut())
        else {
            *y += height;
            return;
        };
        kept.used = frame;
        let shift = tf * Affine::translate(offset) * tf.inverse();
        scene.append(&kept.scene, Some(shift));
        hits.extend(
            kept.hits
                .iter()
                .map(|(rect, hit)| (*rect + offset, hit.clone())),
        );
        let painted = kept.painted.clone().moved(offset);
        self.note_painted(&m.id, painted, column.area);
        *y += height;
    }
}

#[cfg(test)]
mod tests;
