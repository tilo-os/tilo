//! The `@` picker every rich-text field offers: typing `@` and a few letters lists the
//! workspace's people and agents under the caret, narrowed as the name is typed, and
//! picking one puts a mention of them in the text — one marked run naming their participant
//! id ([`Mark::Mention`](super::Mark::Mention)), not prose that happens to start with `@`.

use crate::i18n::widget_text;
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::permissions::UserId;
use crate::presence::{ParticipantKind, PresenceCtx, PresenceUser};
use crate::search::FieldWeight;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::PresenceHue;
use crate::widget::autocomplete::{Suggestion, SuggestionList, SuggestionPerson};

/// Someone a field can mention: a participant of the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionCandidate {
    pub id: String,
    pub name: String,
    pub kind: ParticipantKind,
    pub hue: PresenceHue,
    pub is_self: bool,
}

impl MentionCandidate {
    /// A participant an owner knows of: a person unless `agent`, in the presence `hue` the
    /// workspace gives them ([`PresenceCtx::hue_of`]).
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        agent: bool,
        hue: PresenceHue,
    ) -> Self {
        Self {
            hue,
            id: id.into(),
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
    fn of(presence: &PresenceCtx, user: PresenceUser) -> Self {
        let hue = presence.hue_for(&UserId(user.id.clone()));
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
            hue,
        }
    }
}

/// The `@`-run the caret sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionQuery {
    pub start: usize,
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

/// The candidates worth offering for `query`, best first, ranked as every list search ranks
/// ([`crate::search::ranked_rows`]): a prefix of the whole name, then of a later word
/// ("lov" finds "Ada Lovelace"), then the participant id.
#[must_use]
pub fn rank<'a>(candidates: &'a [MentionCandidate], query: &str) -> Vec<&'a MentionCandidate> {
    let index = crate::search::ListIndex::default();
    crate::search::ranked_rows(
        &index,
        query.trim(),
        candidates.iter().map(|candidate| {
            [
                (candidate.name.as_str(), FieldWeight::TITLE),
                (candidate.id.as_str(), FieldWeight::IDENTITY),
            ]
        }),
    )
    .into_iter()
    .filter_map(|at| candidates.get(at))
    .collect()
}

/// The roster `presence` names merged with what an owner adds: one entry per participant
/// id, the roster's answer winning, sorted by name.
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
            known.id == candidate.id || (known.is_self && presence.is_own_identity(&candidate.id))
        });
        if !known {
            out.push(candidate.clone());
        }
    }
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
    Pass,
    Handled,
    Pick(MentionCandidate),
}

/// The open picker of one field: which `@` it answers and the rows shown.
#[derive(Default)]
pub(crate) struct MentionPicker {
    list: SuggestionList,
    shown: Vec<MentionCandidate>,
    open: Option<(usize, usize)>,
    typed: usize,
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
                    self.list.set_rows(ranked.iter().map(suggestion).collect());
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

    /// Whether a key release or repeat of an arrow, Enter, Tab or Escape belongs to the
    /// open list (so the field does not act on it too).
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

    /// Paint the list under `caret` (the `@`'s caret rect), inside `viewport` when the
    /// window's is known.
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

/// A participant as a completion offers them: their avatar and name; a pick writes `@name`
/// and, in rich text, a mention of their participant id.
pub(crate) fn candidate(candidate: &MentionCandidate) -> crate::widget::completion::Candidate {
    let row = suggestion_with_value(candidate, candidate.label());
    let mut out =
        crate::widget::completion::Candidate::reference(&candidate.name, row.value, &candidate.id);
    out.person = row.person;
    out.label = row.detail;
    out
}

fn suggestion_with_value(candidate: &MentionCandidate, value: String) -> Suggestion {
    let detail = match candidate.kind {
        ParticipantKind::Person => None,
        ParticipantKind::Agent => Some(widget_text("widget-mention-agent")),
        ParticipantKind::Shortcut => Some(widget_text("widget-mention-shortcut")),
        ParticipantKind::App => Some(widget_text("widget-mention-app")),
        ParticipantKind::Bot => Some(widget_text("widget-mention-bot")),
    };
    let row = Suggestion::labelled(value, candidate.name.clone()).with_person(SuggestionPerson {
        identity: candidate.id.clone(),
        hue: candidate.hue,
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

    fn workspace() -> Vec<MentionCandidate> {
        vec![
            MentionCandidate::new(
                "u_ada",
                "Ada Lovelace",
                false,
                PresenceHue::of_identity("u_ada"),
            ),
            MentionCandidate::new(
                "u_alan",
                "Alan Turing",
                false,
                PresenceHue::of_identity("u_alan"),
            ),
            MentionCandidate::new(
                "agent:tide",
                "Tide",
                true,
                PresenceHue::of_identity("agent:tide"),
            ),
        ]
    }

    /// A face in the `@` list wears the hue the workspace announced for that person, never one
    /// derived from their id here.
    #[test]
    fn a_candidate_wears_the_hue_the_member_list_announced() {
        let mut presence = PresenceCtx::empty();
        presence.set_display_name(UserId("u_ada".into()), "Ada");
        presence.set_presence_hue(UserId("u_ada".into()), PresenceHue::Violet);
        let announced = presence.hue_of("u_ada");
        assert_ne!(
            announced,
            PresenceHue::of_identity("u_ada"),
            "the test needs a hue her id does not derive"
        );
        let ada = presence
            .roster()
            .into_iter()
            .find(|user| user.id == "u_ada")
            .expect("Ada is in the roster");
        assert_eq!(MentionCandidate::of(&presence, ada).hue, announced);
    }

    #[test]
    fn an_at_opening_a_word_starts_a_query_and_one_inside_a_word_does_not() {
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
        let _settled = crate::motion::testing::settled();
        let workspace = workspace();
        let names: Vec<_> = rank(&workspace, "t")
            .into_iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["Tide", "Alan Turing"]);
        assert_eq!(rank(&workspace, "").len(), 3);
        assert!(rank(&workspace, "zz").is_empty());
    }

    #[test]
    fn the_picker_follows_the_caret_and_stays_closed_after_escape() {
        let _settled = crate::motion::testing::settled();
        let workspace = workspace();
        let mut picker = MentionPicker::default();
        assert!(picker.follow(0, "hello @a", |_| false, &workspace));
        assert!(picker.is_open());
        assert_eq!(picker.typed(), 2);
        let escape = crate::synthetic_input::key_press(Key::Named(NamedKey::Escape), None);
        assert_eq!(picker.handle_key(&escape), PickerKey::Handled);
        assert!(!picker.is_open());
        picker.follow(0, "hello @al", |_| false, &workspace);
        assert!(!picker.is_open(), "the dismissed @ stays closed");
        picker.follow(0, "hello @al @t", |_| false, &workspace);
        assert!(picker.is_open(), "a new @ opens it again");
        picker.follow(0, "hello @Tide", |at| at == 6, &workspace);
        assert!(!picker.is_open(), "a finished mention offers nothing");
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;
    use crate::permissions::UserId;

    #[test]
    fn the_workspace_and_an_owners_candidates_merge_once_by_id_sorted_by_name() {
        let mut presence = crate::presence::PresenceCtx::empty();
        presence
            .names
            .insert(UserId("u_ada".into()), "Ada Lovelace".into());
        presence
            .names
            .insert(UserId("u_alan".into()), "Alan Turing".into());
        let extra = vec![
            MentionCandidate::new("u_ada", "Ada L.", false, PresenceHue::of_identity("u_ada")),
            MentionCandidate::new(
                "u_old",
                "Zoe Zebra",
                false,
                PresenceHue::of_identity("u_old"),
            ),
            MentionCandidate::new("u_blank", "   ", false, PresenceHue::of_identity("u_blank")),
        ];
        let merged = merged_roster(&presence, &extra);
        let names: Vec<_> = merged
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect();
        assert_eq!(names, ["Ada Lovelace", "Alan Turing", "Zoe Zebra"]);
    }
}
