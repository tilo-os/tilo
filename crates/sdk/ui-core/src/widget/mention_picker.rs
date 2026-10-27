//! The `@` picker every rich-text field offers: typing `@` and a few letters
//! lists the room's people and agents under the caret, narrowed as the name
//! is typed, and picking one puts a mention of them in the text — one marked
//! run naming their participant id ([`Mark::Mention`](super::Mark::Mention)),
//! not prose that happens to start with `@`.
//!
//! Who is offered is the room's roster ([`PresenceCtx::roster`]), the same
//! participants and names every app's presence shows; an owner can add
//! participants its record knows of that the roster does not (someone who
//! wrote in a channel and has since left). What was typed stays in the text
//! while the list is open, so a person who keeps typing past the list, or
//! never picks, has written ordinary text.
//!
//! - **Arrow Up / Down** move the armed row; **Enter** or **Tab** picks it.
//! - **Escape** closes the list for this `@`; typing on keeps it closed
//!   until another `@` starts.
//! - A click on a row picks it; hovering arms it.
//!
//! The list is a [`SuggestionList`] anchored at the `@`, painted in the
//! window's top layer.

use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::permissions::UserId;
use crate::presence::{ParticipantKind, PresenceCtx, PresenceUser};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::autocomplete::{Suggestion, SuggestionList, SuggestionPerson};

/// Someone a field can mention: a participant of the room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionCandidate {
    /// Their participant id: what the mention records.
    pub id: String,
    /// What the room calls them; the mention reads `@name`.
    pub name: String,
    pub kind: ParticipantKind,
    /// The colour they wear in the room.
    pub color: (u8, u8, u8),
    /// This session's own participant.
    pub is_self: bool,
}

impl MentionCandidate {
    /// A participant an owner knows of: a person unless `agent`, in the
    /// colour their id picks.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, agent: bool) -> Self {
        let id = id.into();
        Self {
            color: crate::presence::user_color(&id),
            id,
            name: name.into(),
            kind: ParticipantKind::from(agent),
            is_self: false,
        }
    }

    /// The text a pick puts in the field.
    #[must_use]
    pub fn label(&self) -> String {
        format!("@{}", self.name)
    }
}

impl MentionCandidate {
    /// `user` of `presence`'s roster, named as every surface names them
    /// ([`PresenceCtx::participant_label`]). Software keeps its own name:
    /// its kind is shown beside it, not in the mention.
    fn of(presence: &PresenceCtx, user: PresenceUser) -> Self {
        let id = UserId(user.id);
        let kind = presence.participant_kind(&id);
        let name = if kind == ParticipantKind::Person {
            user.name
        } else {
            presence
                .names
                .get(&id)
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or(user.name)
        };
        Self {
            is_self: presence.is_own_identity(&id.0),
            id: id.0,
            name,
            kind,
            color: user.color,
        }
    }
}

/// The `@`-run the caret sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionQuery {
    /// Byte offset of the `@`.
    pub start: usize,
    /// What was typed after the `@`, up to the caret.
    pub query: String,
}

/// How long an `@`-run may grow before it stops being somebody's name.
/// Names have spaces, so a space cannot end a run; this is what stops a
/// stray `@` from holding the list open over a whole sentence.
const MAX_QUERY_LEN: usize = 40;

/// The `@`-run ending at `caret` in `text`, if the caret is in one: an `@`
/// that opens a word (at the start, after whitespace or after punctuation
/// that is not part of a word), followed by at most `MAX_QUERY_LEN` bytes
/// with no newline and no other `@`. An `@` inside a word — an email's
/// `a@b` — opens nothing.
#[must_use]
pub fn query_at_caret(text: &str, caret: usize) -> Option<MentionQuery> {
    let caret = text.floor_char_boundary(caret.min(text.len()));
    let head = text.get(..caret)?;
    let at = head.rfind('@')?;
    let opens_word = head.get(..at)?.chars().next_back().is_none_or(|before| {
        before.is_whitespace()
            || (before.is_ascii_punctuation()
                && !matches!(before, '@' | '_' | '.' | '%' | '+' | '-'))
    });
    if !opens_word {
        return None;
    }
    let query = head.get(at + '@'.len_utf8()..)?;
    if query.len() > MAX_QUERY_LEN || query.contains(['\n', '@']) {
        return None;
    }
    Some(MentionQuery {
        start: at,
        query: query.to_owned(),
    })
}

/// The candidates worth offering for `query`, best first: a prefix of the
/// whole name, then of a later word ("lov" finds "Ada Lovelace"), then a
/// substring of the name, then of the id. Within a rank the given order
/// holds, so the list does not reshuffle while a name is typed. An empty
/// query offers everyone.
#[must_use]
pub fn rank<'a>(candidates: &'a [MentionCandidate], query: &str) -> Vec<&'a MentionCandidate> {
    let q = query.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize, &MentionCandidate)> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let name = candidate.name.to_lowercase();
        let rank = if q.is_empty() || name.starts_with(&q) {
            0
        } else if name.split_whitespace().any(|word| word.starts_with(&q)) {
            1
        } else if name.contains(&q) {
            2
        } else if candidate.id.to_lowercase().contains(&q) {
            3
        } else {
            continue;
        };
        ranked.push((rank, index, candidate));
    }
    ranked.compact_sort_by_key(|(rank, index, _)| (*rank, *index));
    ranked
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .collect()
}

/// The roster `presence` names merged with what an owner adds: one entry
/// per participant id, the roster's answer winning, sorted by name.
#[must_use]
pub fn merged_roster(presence: &PresenceCtx, extra: &[MentionCandidate]) -> Vec<MentionCandidate> {
    let mut out: Vec<MentionCandidate> = presence
        .roster()
        .into_iter()
        .map(|user| MentionCandidate::of(presence, user))
        .collect();
    for candidate in extra {
        if candidate.name.trim().is_empty() {
            continue;
        }
        let known = out.iter().any(|known| {
            known.id == candidate.id
                // The room may spell this session's own participant
                // differently from the id an owner recorded.
                || (known.is_self && presence.is_own_identity(&candidate.id))
        });
        if !known {
            out.push(candidate.clone());
        }
    }
    // By name, as a person scans a list of people; the id keeps two of one
    // name in a stable order.
    out.compact_sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// What a key did to an open picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerKey {
    /// Not the picker's: route it on.
    Pass,
    /// Moved the armed row or closed the list.
    Handled,
    /// Picked this candidate, replacing the typed run.
    Pick(MentionCandidate),
}

/// The open picker of one field: which `@` it answers and the rows shown.
#[derive(Default)]
pub(crate) struct MentionPicker {
    list: SuggestionList,
    shown: Vec<MentionCandidate>,
    /// The block and byte of the `@` the list answers.
    open: Option<(usize, usize)>,
    /// Bytes from the `@` to the caret: what a pick replaces.
    typed: usize,
    /// The `@` the person closed the list on: it stays closed while they
    /// type on after it.
    dismissed: Option<(usize, usize)>,
}

impl MentionPicker {
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some() && !self.shown.is_empty()
    }

    /// The block and byte of the `@` the open list answers.
    pub(crate) fn anchor(&self) -> Option<(usize, usize)> {
        self.open.filter(|_| self.is_open())
    }

    /// Bytes before the caret a pick replaces: the `@` and what followed.
    pub(crate) fn typed(&self) -> usize {
        self.typed
    }

    pub(crate) fn close(&mut self) {
        self.open = None;
        self.shown.clear();
        self.list.clear();
    }

    /// Follow the caret: `before` is the focused block's text up to the
    /// caret, `in_mention` whether the `@` there is already a mention's.
    /// Opens, narrows or closes the list; returns whether anything shown
    /// changed.
    pub(crate) fn follow(
        &mut self,
        block: usize,
        before: &str,
        in_mention: impl Fn(usize) -> bool,
        candidates: &[MentionCandidate],
    ) -> bool {
        let was = (self.open, self.shown.clone());
        let query = query_at_caret(before, before.len()).filter(|query| !in_mention(query.start));
        match query {
            Some(query) if self.dismissed != Some((block, query.start)) => {
                self.dismissed = None;
                let ranked: Vec<MentionCandidate> = rank(candidates, &query.query)
                    .into_iter()
                    .cloned()
                    .collect();
                if ranked.is_empty() {
                    self.close();
                } else {
                    let keep = self
                        .list
                        .highlighted()
                        .and_then(|row| ranked.iter().position(|c| c.id == row.value));
                    self.list
                        .set_suggestions(ranked.iter().map(suggestion).collect());
                    self.list.set_highlight(Some(keep.unwrap_or(0)));
                    self.shown = ranked;
                    self.open = Some((block, query.start));
                    self.typed = before.len().saturating_sub(query.start);
                }
            }
            Some(_) => self.close(),
            None => {
                self.dismissed = None;
                self.close();
            }
        }
        was != (self.open, self.shown.clone())
    }

    /// A key while the list is open.
    pub(crate) fn handle_key(&mut self, event: &WindowEvent) -> PickerKey {
        if !self.is_open() {
            return PickerKey::Pass;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return PickerKey::Pass;
        };
        match key {
            NamedKey::ArrowDown | NamedKey::ArrowUp => {
                self.list.move_highlight(*key == NamedKey::ArrowDown);
                PickerKey::Handled
            }
            NamedKey::Enter | NamedKey::Tab => self.picked(self.list.highlight()),
            NamedKey::Escape => {
                self.dismissed = self.open;
                self.close();
                PickerKey::Handled
            }
            _ => PickerKey::Pass,
        }
    }

    /// Whether a key release or repeat of an arrow, Enter, Tab or Escape
    /// belongs to the open list (so the field does not act on it too).
    pub(crate) fn owns_key(&self, event: &WindowEvent) -> bool {
        self.is_open()
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(
                            NamedKey::ArrowDown
                                | NamedKey::ArrowUp
                                | NamedKey::Enter
                                | NamedKey::Tab
                                | NamedKey::Escape
                        ),
                        ..
                    },
                    ..
                }
            )
    }

    fn picked(&self, index: Option<usize>) -> PickerKey {
        let Some(candidate) = index.and_then(|index| self.shown.get(index)).cloned() else {
            return PickerKey::Handled;
        };
        PickerKey::Pick(candidate)
    }

    /// The row under `point`, picked.
    pub(crate) fn click_at(&self, point: Point) -> Option<MentionCandidate> {
        if !self.is_open() {
            return None;
        }
        match self.picked(self.list.row_at(point)) {
            PickerKey::Pick(candidate) => Some(candidate),
            _ => None,
        }
    }

    /// Whether `point` is over the open list.
    pub(crate) fn contains(&self, point: Point) -> bool {
        self.is_open() && self.list.rect().is_some_and(|rect| rect.contains(point))
    }

    /// Arm the row under `point`; returns whether that changed anything.
    pub(crate) fn hover_at(&mut self, point: Point) -> bool {
        if !self.is_open() {
            return false;
        }
        let changed = self.list.hover_at(point);
        if let Some(row) = self.list.row_at(point)
            && self.list.highlight() != Some(row)
        {
            self.list.set_highlight(Some(row));
            return true;
        }
        changed
    }

    /// The list's panel in the last paint.
    pub(crate) fn rect(&self) -> Option<Rect> {
        self.is_open().then(|| self.list.rect()).flatten()
    }

    /// Paint the list under `caret` (the `@`'s caret rect), inside
    /// `viewport` when the window's is known.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        caret: Rect,
        viewport: Option<Rect>,
    ) {
        if !self.is_open() {
            return;
        }
        self.list.set_anchor(caret);
        if let Some(viewport) = viewport {
            self.list.set_viewport(viewport);
        }
        self.list.paint(scene, transform, text, theme);
    }

    /// The rows in view, for assistive technology.
    pub(crate) fn access_rows(&self) -> Vec<crate::a11y::AccessNode> {
        if !self.is_open() {
            return Vec::new();
        }
        self.list.access_rows(|row| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            "mention".hash(&mut hasher);
            row.value.hash(&mut hasher);
            hasher.finish()
        })
    }
}

/// The row a candidate shows as: their avatar, their name and, for anyone
/// who is not simply a person, what they are. Its value is the participant
/// id, because a document's mention records who it is.
pub(crate) fn suggestion(candidate: &MentionCandidate) -> Suggestion {
    suggestion_with_value(candidate, candidate.id.clone())
}

/// The row a candidate shows as when accepting it writes prose: the value
/// is the mention the text resolves (`@name`), not the participant id.
pub(crate) fn mention_suggestion(candidate: &MentionCandidate) -> Suggestion {
    suggestion_with_value(candidate, candidate.label())
}

fn suggestion_with_value(candidate: &MentionCandidate, value: String) -> Suggestion {
    let detail = match candidate.kind {
        ParticipantKind::Person => None,
        ParticipantKind::Agent => Some(crate::i18n!("widget-mention-agent")),
        ParticipantKind::Shortcut => Some(crate::i18n!("widget-mention-shortcut")),
        ParticipantKind::App => Some(crate::i18n!("widget-mention-app")),
        ParticipantKind::Bot => Some(crate::i18n!("widget-mention-bot")),
    };
    let row = Suggestion::labelled(value, candidate.name.clone()).with_person(SuggestionPerson {
        identity: candidate.id.clone(),
        color: candidate.color,
        agent: candidate.kind != ParticipantKind::Person,
    });
    match detail {
        Some(detail) => row.with_detail(detail),
        None => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Vec<MentionCandidate> {
        vec![
            MentionCandidate::new("u_ada", "Ada Lovelace", false),
            MentionCandidate::new("u_alan", "Alan Turing", false),
            MentionCandidate::new("agent:tide", "Tide", true),
        ]
    }

    #[test]
    fn an_at_opening_a_word_starts_a_query_and_one_inside_a_word_does_not() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        assert_eq!(
            query_at_caret("hi @ad", 6),
            Some(MentionQuery {
                start: 3,
                query: "ad".into()
            })
        );
        assert_eq!(query_at_caret("mail me@ad", 10), None);
        assert_eq!(query_at_caret("@a\nb", 4), None);
    }

    #[test]
    fn ranking_prefers_a_whole_name_prefix_then_a_word_prefix() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let room = room();
        let names: Vec<_> = rank(&room, "t")
            .into_iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["Tide", "Alan Turing"]);
        assert_eq!(rank(&room, "").len(), 3);
        assert!(rank(&room, "zz").is_empty());
    }

    #[test]
    fn the_picker_follows_the_caret_and_stays_closed_after_escape() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let room = room();
        let mut picker = MentionPicker::default();
        assert!(picker.follow(0, "hello @a", |_| false, &room));
        assert!(picker.is_open());
        assert_eq!(picker.typed(), 2);
        let escape = crate::synthetic_input::key_press(Key::Named(NamedKey::Escape), None);
        assert_eq!(picker.handle_key(&escape), PickerKey::Handled);
        assert!(!picker.is_open());
        picker.follow(0, "hello @al", |_| false, &room);
        assert!(!picker.is_open(), "the dismissed @ stays closed");
        picker.follow(0, "hello @al @t", |_| false, &room);
        assert!(picker.is_open(), "a new @ opens it again");
        picker.follow(0, "hello @Tide", |at| at == 6, &room);
        assert!(!picker.is_open(), "a finished mention offers nothing");
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;
    use crate::permissions::UserId;

    #[test]
    fn the_room_and_an_owners_candidates_merge_once_by_id_sorted_by_name() {
        let mut presence = crate::presence::PresenceCtx::empty();
        presence
            .names
            .insert(UserId("u_ada".into()), "Ada Lovelace".into());
        presence
            .names
            .insert(UserId("u_alan".into()), "Alan Turing".into());
        let extra = vec![
            MentionCandidate::new("u_ada", "Ada L.", false),
            MentionCandidate::new("u_old", "Zoe Zebra", false),
            MentionCandidate::new("u_blank", "   ", false),
        ];
        let merged = merged_roster(&presence, &extra);
        let names: Vec<_> = merged
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect();
        assert_eq!(names, ["Ada Lovelace", "Alan Turing", "Zoe Zebra"]);
    }
}
