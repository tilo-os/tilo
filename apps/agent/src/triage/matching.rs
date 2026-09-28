//! Whether a new report repeats an earlier one, decided without a model.
//!
//! A report is read by what it carries, never by guessing at its prose
//! first: the address it was written about (`reference`), the app that
//! address belongs to, and the recording's summary (the apps it covers and
//! the layer the problem is in). The text is compared last, as words, and
//! only ever adds to what the structure says.
//!
//! The weights are chosen so that no single weak signal reaches the bar on
//! its own: two reports about the same control of the same app, worded
//! alike, are a duplicate; two reports that merely both mention Mail are
//! not.

use std::collections::BTreeSet;

use chat::model::{Attachment, Message};
use tilo_reference::SessionLayer;

/// The score at which a report is taken for a repeat without a model.
pub const DUPLICATE_AT: f32 = 0.55;

/// The score at which an earlier report is worth showing a model as a
/// candidate. Lower than [`DUPLICATE_AT`]: the model may confirm a match the
/// structure alone could not.
pub const CANDIDATE_AT: f32 = 0.3;

/// How many candidates a model is shown at most.
pub const MAX_CANDIDATES: usize = 3;

/// Words too common in bug reports to say anything about which bug.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "when", "then", "than", "from", "have", "has",
    "was", "are", "but", "not", "does", "doesn", "don", "did", "can", "cannot", "into", "onto",
    "just", "also", "there", "their", "what", "which", "while", "after", "before", "again", "its",
    "you", "your", "our", "out", "all", "any", "some", "get", "got", "set", "use", "using",
];

/// What a report carries that identifies what it is about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signals {
    /// The address it was written about, whole: kind and id.
    pub target: Option<(String, String)>,
    /// The app namespaces it concerns: the address's own, and the apps its
    /// recording covers (`so.tilo.mail` → `mail`).
    pub apps: BTreeSet<String>,
    /// The layer its recording's problem is in.
    pub layer: Option<SessionLayer>,
    /// The distinctive words of its text.
    pub words: BTreeSet<String>,
}

/// The namespace of an app id or a resource kind: `so.tilo.mail` and
/// `mail.field` are both `mail`.
fn namespace(id: &str) -> Option<String> {
    let id = id.trim();
    let name = if let Some(rest) = id.strip_prefix("so.tilo.") {
        rest
    } else {
        id.split(['.', ':']).next().unwrap_or(id)
    };
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// The distinctive words of `text`, folded to lower case with a plural `s`
/// dropped, so "buttons" and "Button" are one word.
pub fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= 3 && !STOP_WORDS.contains(&word.as_str()))
        .map(|word| match word.strip_suffix('s') {
            Some(stem) if stem.chars().count() >= 3 && !stem.ends_with('s') => stem.to_string(),
            _ => word,
        })
        .collect()
}

impl Signals {
    pub fn of(message: &Message) -> Self {
        let mut apps = BTreeSet::new();
        let target = message.reference.as_ref().map(|reference| {
            if let Some(app) = namespace(&reference.kind) {
                apps.insert(app);
            }
            (reference.kind.clone(), reference.id.clone())
        });
        for reference in &message.also {
            if let Some(app) = namespace(&reference.kind) {
                apps.insert(app);
            }
        }
        let session = message
            .attachment
            .as_ref()
            .and_then(Attachment::record)
            .and_then(|record| record.session.as_ref());
        if let Some(summary) = session {
            apps.extend(summary.apps.iter().filter_map(|app| namespace(app)));
        }
        Self {
            target,
            apps,
            layer: session.map(|summary| summary.layer),
            words: words(&message.text),
        }
    }
}

/// How alike two reports are, from 0 (nothing in common) to 1.
pub fn similarity(a: &Signals, b: &Signals) -> f32 {
    let mut score = 0.0_f32;
    if a.target.is_some() && a.target == b.target {
        score += 0.45;
    }
    if !a.apps.is_disjoint(&b.apps) {
        score += 0.15;
    }
    if a.layer.is_some() && a.layer == b.layer {
        score += 0.05;
    }
    let shared = a.words.intersection(&b.words).count();
    let union = a.words.union(&b.words).count();
    if union > 0 {
        #[allow(clippy::cast_precision_loss, reason = "word counts are small")]
        let jaccard = shared as f32 / union as f32;
        score += 0.5 * jaccard;
    }
    score.min(1.0)
}

/// Earlier reports that `report` may repeat, best first, each with its
/// score. Only reports older than `report` count: the earlier one is the
/// original.
pub fn candidates<'a>(report: &Message, earlier: &'a [Message]) -> Vec<(&'a Message, f32)> {
    let signals = Signals::of(report);
    let mut out: Vec<(&Message, f32)> = earlier
        .iter()
        .filter(|other| {
            let (earlier, then) = ((other.ts, &other.id), (report.ts, &report.id));
            earlier < then
        })
        .map(|other| (other, similarity(&signals, &Signals::of(other))))
        .filter(|(_, score)| *score >= CANDIDATE_AT)
        .collect();
    out.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then_with(|| a.ts.cmp(&b.ts)));
    out.truncate(MAX_CANDIDATES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_reference::{RecordAttachment, ResourceRef, SessionSummary};

    pub(crate) fn report(id: &str, ts: i64, text: &str) -> Message {
        Message {
            id: id.into(),
            channel: tilo_reference::FEEDBACK_CHANNEL.into(),
            author: "u_reporter".into(),
            ts,
            text: text.into(),
            body: Vec::new(),
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![],
            mentions: vec![],
            reference: None,
            also: Vec::new(),
            attachment: None,
            attachment_withdrawn: false,
            pinned: false,
            work: None,
            report: None,
            approved: None,
        }
    }

    fn about(mut m: Message, kind: &str, id: &str) -> Message {
        m.reference = Some(ResourceRef::whole(kind, id, "To"));
        m
    }

    fn recorded(mut m: Message, apps: &[&str], layer: SessionLayer) -> Message {
        m.attachment = Some(Attachment::Record(Box::new(RecordAttachment::session(
            "uuid:rec",
            "Recording",
            "room",
            SessionSummary {
                apps: apps.iter().map(|app| (*app).to_string()).collect(),
                duration_ms: 1000,
                steps: 3,
                layer,
                redactions: tilo_reference::Redactions::default(),
                generation: Some(120),
            },
        ))));
        m
    }

    #[test]
    fn the_same_control_worded_alike_is_a_repeat_and_a_shared_app_alone_is_not() {
        let first = about(
            report(
                "m_1",
                10,
                "Send does nothing when the To field ends with a comma",
            ),
            "mail.field",
            "mail.field:to",
        );
        let again = about(
            report(
                "m_2",
                20,
                "Pressing send does nothing if the To field has a trailing comma",
            ),
            "mail.field",
            "mail.field:to",
        );
        let other = about(
            report("m_3", 30, "The sidebar font is blurry at 150% scale"),
            "mail.sidebar",
            "mail.sidebar:folders",
        );
        let score = similarity(&Signals::of(&again), &Signals::of(&first));
        assert!(score >= DUPLICATE_AT, "{score}");
        let unrelated = similarity(&Signals::of(&other), &Signals::of(&first));
        assert!(unrelated < CANDIDATE_AT, "{unrelated}");

        let earlier = [first.clone(), other];
        let found = candidates(&again, &earlier);
        assert_eq!(found.first().map(|(m, _)| m.id.as_str()), Some("m_1"));
        assert!(
            candidates(&first, &[again]).is_empty(),
            "a later report is never the original"
        );
    }

    #[test]
    fn a_recording_contributes_its_apps_and_layer() {
        let a = recorded(
            report("m_1", 10, "Clipped sidebar labels"),
            &["so.tilo.file-explorer"],
            SessionLayer::BelowToolkit,
        );
        let b = recorded(
            report("m_2", 20, "Sidebar labels are clipped"),
            &["so.tilo.file-explorer"],
            SessionLayer::BelowToolkit,
        );
        assert!(Signals::of(&a).apps.contains("file-explorer"));
        assert!(similarity(&Signals::of(&b), &Signals::of(&a)) >= DUPLICATE_AT);
    }
}
