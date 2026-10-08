//! Inert choices use the existing pending host-request lifetime.
use super::{PendingDialog, PortalDialogQueue};
use std::time::{Duration, Instant};
use tilo_actions::remote::ResourceSelectionPromptRequest;
use tilo_actions::remote::{Completer, Pending, pending};

pub(super) struct PendingResourceSelection {
    pub(super) source: [u8; 32],
    pub(super) correlation: u64,
    pub(super) request: ResourceSelectionPromptRequest,
    pub(super) expires: Instant,
    pub(super) reply: Completer<Option<uuid::Uuid>>,
}

#[derive(Clone)]
pub(crate) struct ResourceSelectionView {
    pub(crate) handle: String,
    pub(crate) request: ResourceSelectionPromptRequest,
}

impl PortalDialogQueue {
    pub(crate) fn enqueue_resource_selection(
        &self,
        source: [u8; 32],
        correlation: u64,
        request: ResourceSelectionPromptRequest,
    ) -> Result<Pending<Option<uuid::Uuid>>, String> {
        let handle = format!(
            "resource-selection:{}:{correlation}:{}",
            hex::encode(source),
            uuid::Uuid::new_v4()
        );
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "host request queue unavailable")?;
        if inner.pending.values().any(|entry| {
            matches!(entry, PendingDialog::ResourceSelection(selection)
            if selection.source == source && selection.correlation == correlation)
        }) || inner
            .pending
            .values()
            .filter(|entry| matches!(entry, PendingDialog::ResourceSelection(_)))
            .count()
            >= 4
        {
            return Err("resource selection is already pending or exceeds host bounds".into());
        }
        let (reply, receiver) = pending();
        inner.pending.insert(
            handle,
            PendingDialog::ResourceSelection(PendingResourceSelection {
                source,
                correlation,
                request,
                expires: Instant::now() + Duration::from_secs(30),
                reply,
            }),
        );
        // Palette requests do not enter the sheet controller's order.
        Ok(receiver)
    }

    pub(crate) fn resource_selection(&self, room: &str) -> Option<ResourceSelectionView> {
        let mut inner = self.inner.lock().ok()?;
        let expired: Vec<_> = inner
            .pending
            .extract_if(|_, entry| {
                matches!(entry, PendingDialog::ResourceSelection(selection)
                if selection.reply.is_abandoned() || selection.expires <= Instant::now())
            })
            .collect();
        let view = inner
            .pending
            .iter()
            .filter_map(|(handle, entry)| {
                let PendingDialog::ResourceSelection(selection) = entry else {
                    return None;
                };
                (selection.request.room == room).then(|| ResourceSelectionView {
                    handle: handle.clone(),
                    request: selection.request.clone(),
                })
            })
            .min_by(|left, right| left.handle.cmp(&right.handle));
        drop(inner);
        drop(expired);
        view
    }

    /// Both semantic actions and palette activation answer this exact request.
    pub(crate) fn answer_resource_selection(
        &self,
        room: &str,
        handle: &str,
        token: Option<uuid::Uuid>,
    ) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "host request queue unavailable")?;
        let Some(PendingDialog::ResourceSelection(selection)) = inner.pending.get(handle) else {
            return Err("resource selection retired".into());
        };
        if selection.request.room != room
            || selection.expires <= Instant::now()
            || selection.reply.is_abandoned()
        {
            return Err("resource selection scope retired".into());
        }
        if token.is_some_and(|token| {
            !selection
                .request
                .candidates
                .iter()
                .any(|candidate| candidate.token == token)
        }) {
            return Err("resource selection does not offer this candidate".into());
        }
        let Some(PendingDialog::ResourceSelection(selection)) = inner.pending.remove(handle) else {
            return Err("resource selection retired".into());
        };
        drop(inner);
        selection.reply.complete(token);
        Ok(())
    }

    pub(crate) fn withdraw_resource_selection(
        &self,
        room: &str,
        source: [u8; 32],
        correlation: u64,
    ) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let handle = inner.pending.iter().find_map(|(handle, entry)| {
            matches!(entry, PendingDialog::ResourceSelection(selection)
                if selection.source == source && selection.correlation == correlation && selection.request.room == room)
                .then(|| handle.clone())
        });
        let retired = handle.and_then(|handle| inner.pending.remove(&handle));
        drop(inner);
        retired.is_some()
    }
}
