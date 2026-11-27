//! The `@` picker every rich-text field offers: typing `@` and a few letters lists the
//! workspace's people and agents under the caret, narrowed as the name is typed, and
//! picking one puts a mention of them in the text — one marked run naming their member
//! id ([`Mark::Mention`](super::Mark::Mention)), not prose that happens to start with `@`.

use tilo_sort::CompactSort as _;
use tilo_ui_base::i18n::widget_text;

use crate::widget::autocomplete::Suggestion;
use tilo_member_types::MemberId;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::text_match::FieldWeight;
use tilo_ui_base::tokens::PresenceHue;

/// Someone a field can mention: a member of the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionCandidate {
    pub id: MemberId,
    pub name: String,
    pub hue: PresenceHue,
    pub is_self: bool,
}

impl MentionCandidate {
    /// A member an owner knows of, in the presence `hue` the workspace gives them
    /// ([`PresenceCtx::hue_for`]).
    #[must_use]
    pub fn new(id: MemberId, name: impl Into<String>, hue: PresenceHue) -> Self {
        Self {
            hue,
            id,
            name: name.into(),
            is_self: false,
        }
    }

    /// The text a pick puts in the field.
    #[must_use]
    pub fn label(&self) -> String {
        format!("@{}", self.name)
    }

    /// The value a mention of them is stored as.
    #[must_use]
    pub fn reference(&self) -> String {
        self.id.mark_value()
    }
}

#[cfg(feature = "model")]
impl MentionCandidate {
    fn of(presence: &PresenceCtx, member: collab_types::Member) -> Self {
        let id = member.id;
        let name = if id.is_person() {
            member.name
        } else {
            presence
                .names
                .get(&id)
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or(member.name)
        };
        Self {
            is_self: member.is_self,
            id,
            name,
            hue: member.colour,
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
/// ([`tilo_ui_base::text_match::ranked_rows`]): a prefix of the whole name, then of a later word
/// ("lov" finds "Ada Lovelace"), then the member id.
#[must_use]
pub fn rank<'a>(candidates: &'a [MentionCandidate], query: &str) -> Vec<&'a MentionCandidate> {
    let index = tilo_ui_base::text_match::ListIndex::default();
    tilo_ui_base::text_match::ranked_rows(
        &index,
        query.trim(),
        candidates.iter().map(|candidate| {
            [
                (candidate.name.as_str(), FieldWeight::TITLE),
                (candidate.id.id(), FieldWeight::IDENTITY),
            ]
        }),
    )
    .into_iter()
    .filter_map(|at| candidates.get(at))
    .collect()
}

/// The roster `presence` names merged with what an owner adds: one entry per member
/// id, the roster's answer winning, sorted by name.
#[must_use]
pub fn merged_roster(presence: &PresenceCtx, extra: &[MentionCandidate]) -> Vec<MentionCandidate> {
    // The workspace's people come from its roster; a toolkit without the model has none, and
    // completes from what its app supplied.
    #[cfg(feature = "model")]
    let mut out: Vec<MentionCandidate> = presence
        .roster()
        .into_iter()
        .map(|user| MentionCandidate::of(presence, user))
        .collect();
    #[cfg(not(feature = "model"))]
    let mut out: Vec<MentionCandidate> = Vec::new();
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

/// The row a candidate shows as: their avatar, their name and, for anyone
/// who is not simply a person, what they are. Its value is the member
/// id, because a document's mention records who it is.
pub fn suggestion(candidate: &MentionCandidate) -> Suggestion {
    suggestion_with_value(candidate, candidate.reference())
}

/// A member as a completion offers them: their avatar and name; a pick writes `@name`
/// and, in rich text, a mention of their member id.
pub fn candidate(candidate: &MentionCandidate) -> crate::widget::completion::Candidate {
    let row = suggestion_with_value(candidate, candidate.label());
    let mut out = crate::widget::completion::Candidate::reference(
        &candidate.name,
        row.value,
        candidate.reference(),
    );
    out.person = row.person;
    out.label = row.detail;
    out
}

fn suggestion_with_value(candidate: &MentionCandidate, value: String) -> Suggestion {
    let detail = match candidate.id {
        MemberId::Person(_) => None,
        MemberId::Agent { .. } => Some(widget_text("widget-mention-agent")),
        MemberId::Bot(_) => Some(widget_text("widget-mention-bot")),
    };
    let row = Suggestion::labelled(value, candidate.name.clone()).with_person(
        tilo_ui_base::component::Person {
            identity: candidate.id.clone().into(),
            name: candidate.name.clone(),
            hue: candidate.hue,
            picture: None,
        },
    );
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
                MemberId::person("u_ada"),
                "Ada Lovelace",
                PresenceHue::of_identity("u_ada"),
            ),
            MentionCandidate::new(
                MemberId::person("u_alan"),
                "Alan Turing",
                PresenceHue::of_identity("u_alan"),
            ),
            MentionCandidate::new(
                MemberId::agent("tide", "u_alan"),
                "Tide",
                PresenceHue::of_identity("agent:tide"),
            ),
        ]
    }

    tilo_test_env::scenario! {
        fn the_mention_picker_queries_ranks_and_follows_the_caret;

        phase a_candidate_wears_the_hue_the_member_list_announced;
        // A face in the `@` list wears the hue the workspace announced for that person, never one
        // derived from their id here.
        {
            let mut presence = PresenceCtx::empty();
            presence.set_display_name(MemberId::person("u_ada"), "Ada");
            presence.set_presence_hue(MemberId::person("u_ada"), PresenceHue::Violet);
            let announced = presence.hue_for(&tilo_member_types::MemberId::person("u_ada"));
            assert_ne!(
                announced,
                PresenceHue::of_identity("u_ada"),
                "the test needs a hue her id does not derive"
            );
            let ada = presence
                .roster()
                .into_iter()
                .find(|member| member.id == MemberId::person("u_ada"))
                .expect("Ada is in the roster");
            assert_eq!(MentionCandidate::of(&presence, ada).hue, announced);
        }

        phase an_at_opening_a_word_starts_a_query_and_one_inside_a_word_does_not;
        {
            let _settled = tilo_ui_base::motion::testing::settled();
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

        phase ranking_prefers_a_whole_name_prefix_then_a_word_prefix;
        {
            let _settled = tilo_ui_base::motion::testing::settled();
            let workspace = workspace();
            let names: Vec<_> = rank(&workspace, "t")
                .into_iter()
                .map(|c| c.name.as_str())
                .collect();
            assert_eq!(names, ["Tide", "Alan Turing"]);
            assert_eq!(rank(&workspace, "").len(), 3);
            assert!(rank(&workspace, "zz").is_empty());
        }
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;
    use tilo_member_types::MemberId;

    #[test]
    fn the_workspace_and_an_owners_candidates_merge_once_by_id_sorted_by_name() {
        let mut presence = tilo_ui_base::presence::PresenceCtx::empty();
        presence
            .names
            .insert(MemberId::person("u_ada"), "Ada Lovelace".into());
        presence
            .names
            .insert(MemberId::person("u_alan"), "Alan Turing".into());
        let extra = vec![
            MentionCandidate::new(
                MemberId::person("u_ada"),
                "Ada L.",
                PresenceHue::of_identity("u_ada"),
            ),
            MentionCandidate::new(
                MemberId::person("u_old"),
                "Zoe Zebra",
                PresenceHue::of_identity("u_old"),
            ),
            MentionCandidate::new(
                MemberId::person("u_blank"),
                "   ",
                PresenceHue::of_identity("u_blank"),
            ),
        ];
        let merged = merged_roster(&presence, &extra);
        let names: Vec<_> = merged
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect();
        assert_eq!(names, ["Ada Lovelace", "Alan Turing", "Zoe Zebra"]);
    }
}
