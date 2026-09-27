//! Host-owned asynchronous requests. Preview renderers receive frozen bytes only.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::history_mode::{
    HistoricalAssetRequest, HistoricalAssetResponse, HistorySnapshot, assets::AssetResponseKey,
};
use std::sync::mpsc;

type Completion = (AssetResponseKey, Result<FrozenPreviewAssets, String>);
pub(super) struct AssetSession {
    host: uuid::Uuid,
    generation: u64,
    sender: mpsc::Sender<Completion>,
    receiver: mpsc::Receiver<Completion>,
    pending: RefCell<Vec<Completion>>,
    expected: [Option<AssetResponseKey>; 2],
    pub(super) snapshots: [Option<HistorySnapshot>; 2],
    requested: bool,
}
impl AssetSession {
    pub(super) fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            host: uuid::Uuid::new_v4(),
            generation: 0,
            sender,
            receiver,
            pending: RefCell::new(vec![]),
            expected: [None, None],
            snapshots: [None, None],
            requested: false,
        }
    }
    pub(super) fn cancel(&mut self) {
        if let Some(next) = self.generation.checked_add(1) {
            self.generation = next;
        } else {
            self.host = uuid::Uuid::new_v4();
            self.generation = 0;
        }
        self.expected = [None, None];
        self.snapshots = [None, None];
        self.requested = false;
        self.pending.get_mut().clear();
    }
    pub(super) fn has_results(&self) -> bool {
        self.pending.borrow_mut().extend(self.receiver.try_iter());
        !self.pending.borrow().is_empty()
    }
}
impl HistoryHost {
    pub(super) fn refresh_media(&mut self) {
        let Some(scope) = self.registration.definition.scopes.get(self.active_scope) else {
            return;
        };
        let Some(create) = scope.presentation.preview_factory() else {
            return;
        };
        let provider = self.registration.assets.get(scope.key).cloned();
        let resource = match self.source() {
            HistorySourceRegistration::HostDocument { resource, .. }
            | HistorySourceRegistration::OwnedDocument { resource, .. } => resource.clone(),
            HistorySourceRegistration::Unversioned { .. }
            | HistorySourceRegistration::CheckedFiles { .. } => return,
        };
        if !self.assets.requested && self.assets.snapshots.iter().any(Option::is_some) {
            self.assets.requested = true;
            for ((canvas, snapshot), expected) in [&mut self.before, &mut self.after]
                .into_iter()
                .zip(self.assets.snapshots.iter().cloned())
                .zip(self.assets.expected.iter_mut())
            {
                let Some(snapshot) = snapshot else {
                    continue;
                };
                canvas.install(create(PreviewInput {
                    snapshot: snapshot.clone(),
                    initial_view: PreviewView::default(),
                    assets: FrozenPreviewAssets::default(),
                }));
                if let Some(provider) = &provider {
                    let key = AssetResponseKey {
                        host: self.assets.host,
                        resource: resource.clone(),
                        generation: self.assets.generation,
                        side: snapshot.side(),
                        heads: snapshot.heads().to_vec(),
                    };
                    *expected = Some(key.clone());
                    canvas.set_status(Some(crate::i18n::I18nManager::lookup_plain(
                        "history-mode-loading-media",
                    )));
                    provider.request(
                        HistoricalAssetRequest {
                            resource: resource.clone(),
                            snapshot,
                        },
                        HistoricalAssetResponse::new(key, self.assets.sender.clone()),
                    );
                }
            }
        }
        self.assets.has_results();
        let completions = self.assets.pending.get_mut().drain(..).collect::<Vec<_>>();
        for (key, result) in completions {
            let index = usize::from(key.side != PreviewSide::Before);
            let Some(expected) = self.assets.expected.get_mut(index) else {
                continue;
            };
            // The request's host, generation, exact owner namespace, side and
            // immutable heads must still match. No result can retarget itself.
            if expected.as_ref() != Some(&key) || key.resource != resource || self.invalid_owner {
                continue;
            }
            *expected = None;
            let Some(snapshot) = self.assets.snapshots.get(index).cloned().flatten() else {
                continue;
            };
            let canvas = if index == 0 {
                &mut self.before
            } else {
                &mut self.after
            };
            let view = canvas
                .preview
                .as_ref()
                .map(|preview| preview.view())
                .unwrap_or_default();
            match result {
                Ok(assets) => canvas.install(create(PreviewInput {
                    snapshot,
                    initial_view: view,
                    assets,
                })),
                Err(error) => {
                    canvas.set_status(Some(error));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_at_generation_limit_retires_the_request_namespace() {
        let mut session = AssetSession::new();
        let original_host = session.host;
        session.generation = u64::MAX;
        let old_key = AssetResponseKey {
            host: original_host,
            generation: u64::MAX,
            resource: crate::history_mode::HistoryResource::new(
                "profile",
                "room",
                "app",
                crate::RecordId::for_tab(1),
            ),
            side: PreviewSide::After,
            heads: vec![],
        };
        session.expected[1] = Some(old_key.clone());
        let response = HistoricalAssetResponse::new(old_key.clone(), session.sender.clone());
        session.cancel();
        assert_ne!(session.host, original_host);
        assert_eq!(session.generation, 0);
        assert!(session.expected.iter().all(Option::is_none));
        response.complete(Err("late old namespace".into()));
        assert!(session.has_results());
        let completed = session.pending.borrow();
        assert_eq!(completed[0].0, old_key);
        assert_ne!(completed[0].0.host, session.host);
    }
}
