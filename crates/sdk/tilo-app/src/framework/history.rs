//! The resource's history as actions every framework app answers: the
//! verbs themselves are [`ui_core::history_source::verbs`], the same ones a
//! tilolang host answers for its instances. What is here says them in this
//! thread's words ([`ThreadWords`]) and keeps the in-app history lists up
//! to date.

use ui_core::automerge::{AutoCommit, ChangeHash, Patch};
use ui_core::history_source::ThreadWords;

pub use ui_core::history_source::verbs::{
    EDIT_TYPE, HistoryEdit, HistoryVersion, PART_EDIT_TYPE, PartEdit, VIEW_TYPE, is_edit,
    part_edit_specs, review, specs, version_name,
};

/// A history edit of `R`'s document: `R`'s own registry is the only decoder
/// for its recorded compensation. Wire context never installs a decoder or
/// a caller.
pub(crate) struct ResourceHistoryEdit<R> {
    edit: HistoryEdit,
    resource: std::marker::PhantomData<fn() -> R>,
}

impl<R> ResourceHistoryEdit<R> {
    /// `edit` as `R`'s history applies it.
    pub(crate) const fn new(edit: HistoryEdit) -> Self {
        Self {
            edit,
            resource: std::marker::PhantomData,
        }
    }
}

impl<R> std::fmt::Debug for ResourceHistoryEdit<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt_history_edit(&self.edit, formatter)
    }
}

/// `edit` as `Debug` writes it, by one function for every resource.
#[inline(never)]
fn fmt_history_edit(
    edit: &HistoryEdit,
    formatter: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    std::fmt::Debug::fmt(edit, formatter)
}

impl<'de, R> serde::Deserialize<'de> for ResourceHistoryEdit<R> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self::new(
            history_edit(value).map_err(serde::de::Error::custom)?,
        ))
    }
}

/// A history edit from its JSON form, decoded by one function for every
/// resource.
#[inline(never)]
fn history_edit(value: serde_json::Value) -> Result<HistoryEdit, serde_json::Error> {
    <HistoryEdit as serde::Deserialize>::deserialize(value)
}

impl<R: tilo_resource::Resource> tilo_actions::Action for ResourceHistoryEdit<R> {
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.edit.change_kind()
    }

    fn describe(&self, doc: &tilo_actions::Doc) -> Option<tilo_actions::Described> {
        Some(self.edit.described(doc))
    }

    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        self.edit
            .apply_registered(doc, tilo_resource::group::register::<R>, &ThreadWords)
    }
}

/// The edit `part` names as the part's history applies it, decoding
/// recorded actions with `R`'s registry.
pub(crate) fn part_action<R: tilo_resource::Resource>(part: &PartEdit) -> ResourceHistoryEdit<R> {
    ResourceHistoryEdit::new(part.edit.clone())
}

/// The last `last` versions, oldest first, in this thread's words.
pub(crate) fn recent(doc: &AutoCommit, last: usize) -> Vec<HistoryVersion> {
    ui_core::history_source::verbs::recent(doc, last, &ThreadWords)
}

/// Answer a history view in this thread's words.
pub(crate) fn answer(
    doc: &AutoCommit,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    ui_core::history_source::verbs::answer(doc, payload, &ThreadWords)
}

/// What a history verb did, answered to its caller in this thread's words.
pub(crate) fn result(
    action: &str,
    doc: &AutoCommit,
    patches: &[Patch],
) -> Option<serde_json::Value> {
    ui_core::history_source::verbs::result(action, doc, patches, &ThreadWords)
}

/// The history lists views follow ([`ViewCx::history`](super::ViewCx::history)),
/// brought up to date when the document moves.
#[derive(Clone, Default)]
pub(crate) struct HistoryFeeds(std::rc::Rc<std::cell::RefCell<FeedList>>);

type Feed = (
    std::rc::Weak<()>,
    usize,
    ui_core::LocalState<Vec<HistoryVersion>>,
);

#[derive(Default)]
struct FeedList {
    feeds: Vec<Feed>,
    /// The heads the feeds last showed; empty forces a refresh.
    heads: Vec<ChangeHash>,
}

impl HistoryFeeds {
    pub(crate) fn follow(
        &self,
        alive: std::rc::Weak<()>,
        last: usize,
    ) -> ui_core::LocalState<Vec<HistoryVersion>> {
        let state = ui_core::LocalState::new(Vec::new());
        let mut registry = self.0.borrow_mut();
        registry.feeds.push((alive, last, state.clone()));
        registry.heads.clear();
        state
    }

    /// Show the versions up to `doc`'s heads, when they moved.
    #[inline(never)]
    pub(crate) fn refresh(&self, doc: &AutoCommit) {
        let mut list = self.0.borrow_mut();
        list.feeds.retain(|(alive, _, _)| alive.strong_count() > 0);
        if list.feeds.is_empty() {
            return;
        }
        let heads = doc.committed_heads();
        if list.heads == heads {
            return;
        }
        let most = list
            .feeds
            .iter()
            .map(|(_, last, _)| *last)
            .max()
            .unwrap_or(0);
        let versions = recent(doc, most);
        for (_, last, state) in &list.feeds {
            let shown = versions
                .get(versions.len().saturating_sub(*last)..)
                .unwrap_or_default()
                .to_vec();
            if state.with(|current| *current != shown) {
                state.set(shown);
            }
        }
        list.heads = heads;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::EncodedAction;
    use ui_core::automerge::ROOT;
    use ui_core::automerge::transaction::Transactable;
    use ui_core::history_source::verbs::review::RestoreReview;

    #[derive(tilo_resource::Model, Default, Debug, Clone, PartialEq)]
    pub struct ReplayBoard {
        pub title: String,
    }

    #[tilo_resource::resource(kind = "test.review-replay")]
    impl ReplayBoard {}

    #[test]
    fn reviewed_restore_replays_identically_beside_concurrent_work_and_duplicate_delivery() {
        use ui_core::automerge::{ActorId, ReadDoc};
        let mut origin = AutoCommit::new();
        origin.put(ROOT, "title", "old").unwrap();
        let target = origin.commit().unwrap();
        origin.put(ROOT, "title", "new").unwrap();
        origin.commit();
        let response = review::answer(&origin, &target.to_string(), &["title".into()]).unwrap();
        let review: RestoreReview = serde_json::from_value(response["review"].clone()).unwrap();
        let edit = HistoryEdit::RestoreReviewed { review };
        let encoded = EncodedAction {
            type_id: EDIT_TYPE.into(),
            payload: serde_json::to_vec(&edit).unwrap(),
        };
        let mut registry = tilo_actions::ActionRegistry::new();
        registry.register::<ResourceHistoryEdit<ReplayBoard>>(EDIT_TYPE);
        let action = registry.decode(&encoded).unwrap();
        let mut peer = origin.fork();
        peer.set_actor(ActorId::from(b"peer".as_slice()));
        peer.put(ROOT, "unrelated", "keep").unwrap();
        let unrelated = peer.commit().unwrap();
        let mut transport = tilo_actions::LocalTransport::new();
        tilo_actions::dispatch_with_context(
            &mut origin,
            action.as_ref(),
            encoded,
            &mut transport,
            tilo_actions::ActionContext::for_resource("board", "test.review-replay", "Board"),
        )
        .unwrap();
        let restored = origin.get_heads()[0];
        tilo_actions::replay(&mut peer, &transport.replicated[0], &registry).unwrap();
        assert!(peer.get_heads().contains(&restored));
        assert!(peer.get_heads().contains(&unrelated));
        assert_eq!(
            peer.get(ROOT, "title").unwrap().unwrap().0.as_str(),
            Some("old")
        );
        assert_eq!(
            peer.get(ROOT, "unrelated").unwrap().unwrap().0.as_str(),
            Some("keep")
        );
        assert_eq!(
            tilo_actions::verify_change(&mut peer, restored, &registry).unwrap(),
            tilo_actions::Verified::Reproduced
        );
        let before = peer.get_heads();
        tilo_actions::replay(&mut peer, &transport.replicated[0], &registry).unwrap();
        assert_eq!(peer.get_heads(), before);
    }
}
