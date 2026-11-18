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
                let name = self.interaction_status_label_written(&interaction);
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

/// The characters `changes`, one action's patches, wrote into `doc`, as the anchor an invocation
/// names text by: the region and block each text is drawn in, and a range in each block counted
/// in characters. A rich text's block is the paragraph the field holding the text names (`body`)
/// and its range what was inserted there; the first region it wrote in is the one it names.
/// Without one, an action that changed one plain text field of what is already there (a rename)
/// names all of that field, as one value: the resource's own field is its own block (empty), an
/// item's field (`title`) is in the item's block, and the words of an item kept in another item's
/// collection (a comment in an issue's `comments`) are in that collection, under both items
/// (`<issue>/<comment>`). An action that made an item names what it made instead. `None` when it
/// wrote no text.
pub fn written_by(
    doc: &impl automerge::ReadDoc,
    changes: &[automerge::Patch],
) -> Option<tilo_actions::InteractionAnchor> {
    use automerge::{ObjType, PatchAction, Prop, Value};
    let mut region: Option<&str> = None;
    let mut ranges: Vec<tilo_actions::InteractionTextRange> = Vec::new();
    let mut fields: Vec<(&automerge::ObjId, Vec<&str>)> = Vec::new();
    let mut made = false;
    for patch in changes {
        let keys: Vec<&str> = patch
            .path
            .iter()
            .map(|(_, prop)| match prop {
                Prop::Map(key) => key.as_str(),
                Prop::Seq(_) => "",
            })
            .collect();
        let rich = keys
            .iter()
            .position(|key| *key == tilo_doc::schema::KEY_BLOCKS);
        let (blocks, at, inserted) = match (&patch.action, rich) {
            (PatchAction::SpliceText { index, value, .. }, Some(blocks)) => (blocks, *index, value),
            (PatchAction::SpliceText { .. } | PatchAction::DeleteSeq { .. }, None) => {
                if !fields.iter().any(|(obj, _)| **obj == patch.obj) {
                    fields.push((&patch.obj, keys));
                }
                continue;
            }
            (
                PatchAction::PutMap {
                    value: (Value::Object(ObjType::Map), _),
                    ..
                },
                _,
            ) => {
                made = true;
                continue;
            }
            _ => continue,
        };
        // A block's text is `…/<field>/blocks/<block>/text`.
        let [.., block, tilo_doc::schema::BLOCK_TEXT] = keys.as_slice() else {
            continue;
        };
        let field = blocks.checked_sub(1).map_or("body", |at| keys[at]);
        // One region: the first the action wrote in.
        if block.is_empty() || field.is_empty() || *region.get_or_insert(field) != field {
            continue;
        }
        let start = at as u64;
        let end = start + inserted.make_string().chars().count() as u64;
        if let Some(range) = ranges.iter_mut().find(|range| range.block == *block) {
            range.start = range.start.min(start);
            range.end = range.end.max(end);
        } else if ranges.len() < tilo_actions::MAX_ANCHOR_ITEMS {
            let block = (*block).to_owned();
            ranges.push(tilo_actions::InteractionTextRange { block, start, end });
        }
    }
    if let Some(region) = region {
        return Some(tilo_actions::InteractionAnchor::Text {
            region: region.to_owned(),
            ranges,
        });
    }
    let [(obj, keys)] = fields.as_slice() else {
        return None;
    };
    if made {
        return None;
    }
    let (region, block) = field_place(keys)?;
    let end = doc.text(*obj).ok()?.chars().count() as u64;
    Some(tilo_actions::InteractionAnchor::Text {
        region,
        ranges: vec![tilo_actions::InteractionTextRange {
            block,
            start: 0,
            end,
        }],
    })
}

/// The region and block a plain text field at `keys` is drawn in ([`written_by`]): the field
/// and the item holding it, or, for an item inside another item's collection, that collection
/// and the items' path. `None` where a list stands on the way.
fn field_place(keys: &[&str]) -> Option<(String, String)> {
    let (field, above) = keys.split_last()?;
    if keys.iter().any(|key| key.is_empty()) {
        return None;
    }
    // Each item on the way is `<collection>/<id>/v`.
    let items: Vec<(&str, &str)> = above
        .windows(3)
        .filter(|keys| keys[2] == tilo_resource::shape::ENTRY_VALUE_KEY)
        .map(|keys| (keys[0], keys[1]))
        .collect();
    match items.as_slice() {
        [] => Some(((*field).to_owned(), String::new())),
        [(_, item)] => Some(((*field).to_owned(), (*item).to_owned())),
        [.., (collection, _)] => Some((
            (*collection).to_owned(),
            items
                .iter()
                .map(|(_, id)| *id)
                .collect::<Vec<_>>()
                .join("/"),
        )),
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
        let write = rename(1, Acting::agent("helper"), InteractionPhase::Committed);
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
        let applying = rename(1, Acting::agent("helper"), InteractionPhase::Applying);
        presence.apply_interaction_at(applying, at).unwrap();
        assert_eq!(
            frame(&presence, at, TITLE, (None, None)),
            (1, 1),
            "under way, with its caret"
        );
        let mut write = rename(1, Acting::agent("helper"), InteractionPhase::Committed);
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
        let again = rename(2, Acting::agent("helper"), InteractionPhase::Committed);
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
        let write = rename(1, Acting::agent("helper"), InteractionPhase::Committed);
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

    /// What `change` wrote into a document holding issue `7d2b` titled "Book the hall", with
    /// one comment `c9f1`.
    fn wrote(change: impl FnOnce(&mut automerge::AutoCommit)) -> Option<InteractionAnchor> {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        let mut doc = automerge::AutoCommit::new();
        let issues = doc.put_object(ROOT, "issues", ObjType::Map).unwrap();
        let entry = doc.put_object(&issues, "7d2b", ObjType::Map).unwrap();
        let issue = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        let title = doc.put_object(&issue, "title", ObjType::Text).unwrap();
        doc.splice_text(&title, 0, 0, "Book the hall").unwrap();
        let comments = doc.put_object(&issue, "comments", ObjType::Map).unwrap();
        let entry = doc.put_object(&comments, "c9f1", ObjType::Map).unwrap();
        let comment = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        let text = doc.put_object(&comment, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "Hall 4").unwrap();
        let before = doc.get_heads();
        change(&mut doc);
        let after = doc.get_heads();
        let changes = doc.diff(&before, &after);
        written_by(&doc, &changes)
    }

    fn text_at(region: &str, block: &str, end: u64) -> InteractionAnchor {
        InteractionAnchor::Text {
            region: region.into(),
            ranges: vec![InteractionTextRange {
                block: block.into(),
                start: 0,
                end,
            }],
        }
    }

    #[test]
    fn a_change_to_one_text_field_names_all_of_it_where_it_is_drawn() {
        use automerge::{ROOT, ReadDoc, transaction::Transactable};
        let field = |doc: &automerge::AutoCommit, path: &[&str]| {
            path.iter().fold(ROOT, |obj, key| {
                doc.get(&obj, *key).unwrap().expect("there").1
            })
        };
        let renamed = wrote(|doc| {
            let title = field(doc, &["issues", "7d2b", "v", "title"]);
            doc.splice_text(&title, 5, 4, "").unwrap();
            doc.splice_text(&title, 9, 0, " 4").unwrap();
        });
        assert_eq!(
            renamed,
            Some(text_at("title", "7d2b", 11)),
            "a rename names all of the title, in its item's block"
        );
        let shortened = wrote(|doc| {
            let title = field(doc, &["issues", "7d2b", "v", "title"]);
            doc.splice_text(&title, 4, 9, "").unwrap();
        });
        assert_eq!(shortened, Some(text_at("title", "7d2b", 4)), "a cut too");
        let said = wrote(|doc| {
            let text = field(
                doc,
                &["issues", "7d2b", "v", "comments", "c9f1", "v", "text"],
            );
            doc.splice_text(&text, 6, 0, " is free").unwrap();
        });
        assert_eq!(
            said,
            Some(text_at("comments", "7d2b/c9f1", 14)),
            "a comment's words are in its issue's thread"
        );
        let both = wrote(|doc| {
            let title = field(doc, &["issues", "7d2b", "v", "title"]);
            doc.splice_text(&title, 0, 0, "Re: ").unwrap();
            let text = field(
                doc,
                &["issues", "7d2b", "v", "comments", "c9f1", "v", "text"],
            );
            doc.splice_text(&text, 0, 0, "Re: ").unwrap();
        });
        assert_eq!(both, None, "two fields are no one place");
        let made = wrote(|doc| {
            let issues = field(doc, &["issues"]);
            let entry = doc
                .put_object(&issues, "e41a", automerge::ObjType::Map)
                .unwrap();
            let issue = doc
                .put_object(&entry, "v", automerge::ObjType::Map)
                .unwrap();
            let title = doc
                .put_object(&issue, "title", automerge::ObjType::Text)
                .unwrap();
            doc.splice_text(&title, 0, 0, "New").unwrap();
        });
        assert_eq!(
            made, None,
            "what made an item names the item, not its title"
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
