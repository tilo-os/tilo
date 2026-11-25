//! What a write put into a text, drawn where it stands: its characters washed in the writer's
//! colour with the writer's caret at their end while it arrives, and an agent's last write in a
//! place keeping a quiet wash after that, so a person who looks a moment later still sees what the
//! agent added. Local view state only: nothing of it is stored or sent.

use super::PresenceCtx;
use crate::paint::PaintCtx;
use crate::permissions::MemberId;
use crate::text_render::TextRenderer;
use crate::time::Instant;
use std::cell::Cell;
use tilo_actions::{InteractionPhase, SemanticInteraction};
use vello::kurbo::Rect;

/// How strongly an agent's last write in a place stays washed once its arrival faded, against
/// the full wash it arrives with: light enough to read as a trace, not as a selection.
const RESTING: f32 = 0.3;

/// Where a write landed: who wrote, in which resource, field and block. Each holds that writer's
/// last write there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct WrittenPlace {
    by: tilo_actions::Actor,
    resource: String,
    region: String,
    block: String,
}

#[derive(Debug, Clone)]
pub(super) struct Written {
    interaction: SemanticInteraction,
    /// The first frame that drew its current phase. Its arrival fades from there once it landed,
    /// so a write that came while its place was hidden or scrolled away arrives when it shows.
    drawn_at: Cell<Option<Instant>>,
}

impl Written {
    fn landed(&self) -> bool {
        self.interaction.phase.is_terminal()
    }

    /// Whether its mark changes at `now`: under way, or arriving.
    fn changing(&self, now: Instant) -> bool {
        !self.landed()
            || self.drawn_at.get().is_some_and(|drawn| {
                now.saturating_duration_since(drawn) < crate::motion::Trace::LIFE
            })
    }
}

/// Whether a person's caret or selection `selected` stands within `start..end`: a selection over
/// any of it, or a caret between two of its characters. A caret at either end is where an edit
/// next to it left the caret, not the person reaching into it.
fn touches((a, b): (usize, usize), (start, end): (usize, usize)) -> bool {
    let (a, b) = (a.min(b), a.max(b));
    if a == b {
        start < a && a < end
    } else {
        a < end && start < b
    }
}

impl PresenceCtx {
    /// Hold what `interaction` writes, place by place, in place of its writer's earlier write
    /// there: the mark moves to the new text.
    pub(super) fn note_written(&mut self, interaction: &SemanticInteraction) {
        let tilo_actions::InteractionAnchor::Text { region, ranges } = &interaction.anchor else {
            return;
        };
        let written = self.written.get_mut();
        written.retain(|_, held| held.interaction.invocation != interaction.invocation);
        for range in ranges {
            let place = WrittenPlace {
                by: interaction.actor.by.clone(),
                resource: interaction.resource.id.clone(),
                region: region.clone(),
                block: range.block.clone(),
            };
            let held = Written {
                interaction: interaction.clone(),
                drawn_at: Cell::new(None),
            };
            written.insert(place, held);
        }
        self.written_moved();
    }

    pub(super) fn remove_written_by(&mut self, member: &MemberId) {
        self.written
            .get_mut()
            .retain(|_, held| held.interaction.actor.member_id() != Some(member));
        self.written_moved();
    }

    fn written_moved(&self) {
        self.written_generation
            .set(self.written_generation.get().wrapping_add(1));
    }

    /// Counts each change to what writes are held: a retained drawing of text made at another
    /// count may miss a write's mark, or still show one that went.
    pub fn written_generation(&self) -> u64 {
        self.written_generation.get()
    }

    /// Whether a write's mark on text changes at `now`: under way, or arriving. A retained drawing
    /// of a widget is stale while it does, and on the frame after.
    pub fn shows_writes(&self, now: Instant) -> bool {
        self.written
            .borrow()
            .values()
            .any(|held| held.changing(now))
    }

    /// The next step of an arriving write's fade: a frame the host asks for.
    pub(super) fn written_next_wake(&self, now: Instant) -> Option<Instant> {
        self.written
            .borrow()
            .values()
            .filter(|held| held.landed())
            .filter_map(|held| crate::motion::Trace::next_step(held.drawn_at.get()?, now))
            .min()
    }

    /// Draw each write that landed in `text`, the field `region` (any, where `None`) of the item
    /// or block `block`, where it stands: while under way and as it arrives, its characters washed
    /// in the writer's colour and its caret at their end wearing what it did ("Helper: renamed"),
    /// fading over [`crate::motion::Trace`] from the first frame that drew it landed. An agent's
    /// last write there then keeps a quiet wash, without caret, until the agent writes there
    /// again, the person `edited` there since it was drawn or put their caret or selection
    /// (`selected`, byte offsets) within it, or its text is gone. `place` gives, in the caller's
    /// coordinates and measured with the renderer it is handed, the bands a byte range of `text`
    /// covers and the caret at its end, where it wears one. A write drawn so leaves the activity
    /// rail. Returns who wrote what it drew with a caret, whose own caret there needs no second
    /// tag.
    pub fn draw_writes_in(
        &self,
        paint: &mut PaintCtx<'_>,
        (region, block, text): (Option<&str>, &str, &str),
        (edited, selected): (Option<Instant>, Option<(usize, usize)>),
        field: Option<&str>,
        place: &mut dyn FnMut(
            &mut TextRenderer,
            &SemanticInteraction,
            usize,
            usize,
        ) -> (Vec<Rect>, Option<super::LabeledCaret>),
    ) -> Vec<MemberId> {
        let now = crate::time::now();
        let mut here: Vec<(
            WrittenPlace,
            SemanticInteraction,
            Option<Instant>,
            (usize, usize),
        )> = self
            .written
            .borrow()
            .iter()
            .filter_map(|(at, held)| {
                let range = held.interaction.written_in(region, block, text)?;
                Some((
                    at.clone(),
                    held.interaction.clone(),
                    held.drawn_at.get(),
                    range,
                ))
            })
            .collect();
        here.sort_by_key(|(_, interaction, ..)| interaction.invocation.0);
        let mut writers = Vec::new();
        let mut gone = Vec::new();
        for (at, interaction, drawn, (start, end)) in here {
            // The text it wrote is its place, drawn or not: no card stands for it elsewhere.
            self.note_placed(interaction.invocation);
            let landed = interaction.phase.is_terminal();
            let put_away = edited
                .zip(drawn)
                .is_some_and(|(edited, drawn)| edited >= drawn)
                || (landed && selected.is_some_and(|selected| touches(selected, (start, end))))
                || (landed && drawn.is_some() && start == end);
            if put_away {
                gone.push(at);
                continue;
            }
            if start == end {
                // Its text has not reached this copy yet.
                continue;
            }
            let Some(user) = interaction
                .actor
                .members()
                .first()
                .map(|member| (*member).clone())
            else {
                continue;
            };
            let drawn = drawn.unwrap_or_else(|| {
                if let Some(held) = self.written.borrow().get(&at) {
                    held.drawn_at.set(Some(now));
                }
                now
            });
            self.note_drawn(interaction.invocation, now);
            let arriving = if landed {
                crate::motion::Trace::alpha(now.saturating_duration_since(drawn))
            } else {
                Some(1.0)
            };
            let rests = matches!(interaction.phase, InteractionPhase::Committed)
                && !interaction.actor.is_person();
            if arriving.is_none() && !rests {
                gone.push(at);
                continue;
            }
            let (bands, caret) = place(paint.text, &interaction, start, end);
            let bands = bands
                .into_iter()
                .map(|band| paint.transform.transform_rect_bbox(band));
            let Some(alpha) = arriving else {
                for band in bands {
                    self.draw_wash(paint.scene, paint.theme, &user, band, RESTING);
                }
                continue;
            };
            let alpha = if rests { alpha.max(RESTING) } else { alpha };
            let bands: Vec<(Rect, f32)> = bands.map(|band| (band, alpha)).collect();
            self.draw_peer_typing(paint.scene, paint.theme, &user, &bands);
            if let Some(caret) = caret {
                let name = if landed {
                    self.interaction_status_label_written(&interaction)
                } else if let Some(field) = field {
                    // Under way, the words say which field is being edited where the app names
                    // it, rather than the past tense of what it wrote ("editing Title").
                    let doing = crate::i18n::widget_format(
                        "interaction-editing-field",
                        &[("field", field.into())],
                    );
                    self.interaction_status_label_doing(&interaction, &doing)
                } else {
                    self.interaction_status_label_written(&interaction)
                };
                self.draw_action_caret_labeled_in(
                    paint.scene,
                    paint.theme,
                    paint.text,
                    paint.transform,
                    &user,
                    &name,
                    caret,
                );
            }
            writers.push(user);
        }
        if !gone.is_empty() {
            let mut written = self.written.borrow_mut();
            for at in &gone {
                written.remove(at);
            }
            drop(written);
            self.written_moved();
        }
        writers
    }
}

/// The item the words a write names stand under, where they are an item of its own inside it (a
/// comment in an issue's thread, its block `<issue>/<comment>`) or the item it wrote in (a note it
/// made): a view showing the item but not the words shows the write on the item.
pub(crate) fn written_under(interaction: &SemanticInteraction) -> Option<String> {
    let tilo_actions::InteractionAnchor::Text { ranges, .. } = &interaction.anchor else {
        return None;
    };
    match ranges.first()?.block.split_once('/') {
        Some((item, _)) => (!item.is_empty()).then(|| item.to_owned()),
        None => super::interaction::acted_item(interaction),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::Trace;
    use crate::time::{Duration, testing::with_monotonic_time};
    use tilo_actions::{Acting, InteractionAnchor, InteractionTextRange};

    const TITLE: &str = "Call the plumber today";
    const SECOND: Duration = Duration::from_secs(1);

    /// `actor` renaming task 1 to [`TITLE`], as invocation `id`.
    fn rename(id: u128, actor: Acting, phase: InteractionPhase) -> SemanticInteraction {
        let mut write = super::super::interaction::tests::interaction(id, 1, phase);
        write.actor = actor;
        write.anchor = InteractionAnchor::Text {
            region: "title".into(),
            ranges: vec![InteractionTextRange {
                block: "task-1".into(),
                start: 0,
                end: 22,
            }],
        };
        write
    }

    /// An agent's rename that landed at `at`, drawn by a first frame then.
    fn landed(at: Instant) -> PresenceCtx {
        let mut presence = PresenceCtx::empty();
        let write = rename(
            1,
            Acting::agent("helper", "ada"),
            InteractionPhase::Committed,
        );
        presence.apply_interaction_at(write, at).unwrap();
        frame(&presence, at, TITLE, (None, None));
        presence
    }

    /// One frame drawing the title `text` at `at`: how many writes it drew, and how many carets.
    fn frame(
        presence: &PresenceCtx,
        at: Instant,
        text: &str,
        touched: (Option<Instant>, Option<(usize, usize)>),
    ) -> (usize, usize) {
        with_monotonic_time(at, || {
            let (mut scene, mut renderer) = (vello::Scene::new(), TextRenderer::new());
            let theme = crate::theme::Theme::light();
            let mut drawn = 0;
            let carets = presence.draw_writes_in(
                &mut PaintCtx::new(
                    &mut scene,
                    vello::kurbo::Affine::IDENTITY,
                    &mut renderer,
                    &theme,
                ),
                (Some("title"), "task-1", text),
                touched,
                None,
                &mut |_, _, start, end| {
                    drawn += 1;
                    (vec![Rect::new(start as f64, 0.0, end as f64, 10.0)], None)
                },
            );
            (drawn, carets.len())
        })
    }

    #[test]
    fn an_agents_write_arrives_then_rests_where_it_landed() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let applying = rename(
            1,
            Acting::agent("helper", "ada"),
            InteractionPhase::Applying,
        );
        presence.apply_interaction_at(applying, at).unwrap();
        assert_eq!(
            frame(&presence, at, TITLE, (None, None)),
            (1, 1),
            "under way, with its caret"
        );
        let mut write = rename(
            1,
            Acting::agent("helper", "ada"),
            InteractionPhase::Committed,
        );
        write.sequence = 2;
        presence.apply_interaction_at(write, at + SECOND).unwrap();
        assert_eq!(
            frame(&presence, at + SECOND, TITLE, (None, None)),
            (1, 1),
            "it arrives"
        );
        let half = at + SECOND + Duration::from_millis(500);
        assert_eq!(frame(&presence, half, TITLE, (None, None)), (1, 1));
        assert!(presence.shows_writes(half), "arriving, it changes");
        let step = presence.interaction_next_wake_deadline(half);
        assert_eq!(
            step,
            Some(half + Duration::from_millis(250)),
            "each step is a frame"
        );
        for later in [3, 10, 600] {
            let rest = frame(&presence, at + SECOND * (later + 1), TITLE, (None, None));
            assert_eq!(
                rest,
                (1, 0),
                "{later} s after it landed it rests, without its caret"
            );
        }
        assert!(!presence.shows_writes(at + SECOND * 11));
        assert_eq!(
            presence.written_next_wake(at + SECOND * 600),
            None,
            "resting asks no frames"
        );
    }

    #[test]
    fn the_mark_moves_when_the_agent_writes_there_again() {
        let at = crate::time::now();
        let mut presence = landed(at);
        let generation = presence.written_generation();
        let again = rename(
            2,
            Acting::agent("helper", "ada"),
            InteractionPhase::Committed,
        );
        presence
            .apply_interaction_at(again, at + SECOND * 10)
            .unwrap();
        assert_ne!(
            presence.written_generation(),
            generation,
            "a retained drawing is stale"
        );
        assert_eq!(
            frame(&presence, at + SECOND * 10, TITLE, (None, None)),
            (1, 1),
            "one, arriving"
        );
        let held = presence.written.borrow();
        assert!(
            held.len() == 1
                && held
                    .values()
                    .all(|held| held.interaction.invocation.0.as_u128() == 2)
        );
    }

    #[test]
    fn the_person_reaching_into_it_or_its_text_going_puts_it_away() {
        let at = crate::time::now();
        let later = at + SECOND * 10;
        for (touched, text, why) in [
            ((Some(later), None), TITLE, "edited there"),
            ((None, Some((5, 9))), TITLE, "selected in it"),
            ((None, Some((4, 4))), TITLE, "a caret in it"),
            ((None, None), "", "its text deleted"),
        ] {
            let presence = landed(at);
            assert_eq!(frame(&presence, later, text, touched).0, 0, "{why}");
            assert_eq!(
                frame(&presence, later, TITLE, (None, None)).0,
                0,
                "{why}: it stays away"
            );
        }
        let after = frame(&landed(at), later, TITLE, (None, Some((22, 22))));
        assert_eq!(after.0, 1, "a caret just after it is not in it");
    }

    #[test]
    fn a_write_that_came_unseen_arrives_when_it_first_shows() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let write = rename(
            1,
            Acting::agent("helper", "ada"),
            InteractionPhase::Committed,
        );
        presence.apply_interaction_at(write, at).unwrap();
        assert_eq!(
            presence.written_next_wake(at),
            None,
            "nothing arrives while unseen"
        );
        let shown = at + SECOND * 60;
        assert_eq!(
            frame(&presence, shown, TITLE, (None, None)),
            (1, 1),
            "it arrives when shown"
        );
        assert_eq!(
            frame(&presence, shown + Trace::LIFE / 2, TITLE, (None, None)),
            (1, 1)
        );
        assert_eq!(
            frame(&presence, shown + Trace::LIFE, TITLE, (None, None)),
            (1, 0)
        );
    }

    #[test]
    fn a_persons_write_fades_and_goes() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let write = rename(1, Acting::person("ada"), InteractionPhase::Committed);
        presence.apply_interaction_at(write, at).unwrap();
        assert_eq!(frame(&presence, at, TITLE, (None, None)), (1, 1));
        assert_eq!(
            frame(&presence, at + Trace::LIFE, TITLE, (None, None)),
            (0, 0)
        );
        assert!(
            presence.written.borrow().is_empty(),
            "nothing of it is kept"
        );
    }
}
