//! One-shot keyboard handoff for an already authenticated app-owned surface.
use super::{FloatingToplevelOwner, State};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Transfer {
    Float,
    Return { child: u64, presented: bool },
}

pub(super) struct SourceFocus {
    source: u64,
    owner: Arc<FloatingToplevelOwner>,
    transfer: Option<Transfer>,
    preserve_preedit: bool,
}

impl State {
    /// Resume validated retained geometry before changing focus intent, while
    /// preserving whether this request actually began a new detached lifetime.
    /// A rejected resume cannot arm focus; a duplicate live request cannot
    /// resurrect a transfer cancelled by a real intervening seat transition.
    pub(crate) fn resume_floating_request_with_focus(
        &mut self,
        connection: u64,
        public_id: &str,
        geometry: (vello::kurbo::Rect, vello::kurbo::Rect),
        transfer: (
            Option<super::FloatingTransferFrame>,
            Option<super::FloatingTransferSource>,
        ),
        focus: Option<(u64, Arc<FloatingToplevelOwner>)>,
    ) -> Result<Option<String>, &'static str> {
        let already_detached = self.floating_request_is_detached(connection, public_id);
        let resumed =
            self.resume_floating_request(connection, public_id, geometry, transfer.0, transfer.1)?;
        if let Some((source, owner)) = focus {
            self.arm_floating_focus_transition(
                connection,
                public_id,
                source,
                owner,
                already_detached,
            );
        }
        Ok(resumed)
    }

    fn floating_request_is_detached(&self, connection: u64, public_id: &str) -> bool {
        self.floating_toplevels.iter().any(|surface| {
            surface.connection == connection && surface.public_id == public_id && !surface.suspended
        }) || self.pending_floating_toplevels.iter().any(|surface| {
            surface.connection == connection && surface.public_id == public_id && !surface.suspended
        })
    }

    fn arm_floating_focus_transition(
        &mut self,
        connection: u64,
        public_id: &str,
        source: u64,
        owner: Arc<FloatingToplevelOwner>,
        already_detached: bool,
    ) {
        let key = (connection, public_id.to_owned());
        let preserve_preedit = self.input_method_preedit.is_active();
        let transfer =
            (self.client_input.keyboard_focus() == Some(source)).then_some(Transfer::Float);
        if let Some(previous) = self.floating_focus_handoffs.get_mut(&key) {
            if !already_detached && previous.source == source && previous.owner == owner {
                previous.transfer = transfer;
                previous.preserve_preedit = preserve_preedit;
            }
            return;
        }
        if already_detached {
            // Retiring the original source removes its handoff, not the live
            // child. A replacement source must not acquire autofocus merely
            // by repeating Present for that already-detached capability.
            return;
        }
        self.floating_focus_handoffs.insert(
            key,
            SourceFocus {
                source,
                owner,
                transfer,
                preserve_preedit,
            },
        );
    }

    pub(crate) fn arm_floating_return_focus(&mut self, connection: u64, public_id: &str) {
        let child = self
            .floating_toplevels
            .iter()
            .find(|surface| {
                surface.connection == connection
                    && surface.public_id == public_id
                    && !surface.suspended
            })
            .map(|surface| surface.surface_id);
        let Some(child) = child else {
            return;
        };
        if let Some(source) = self
            .floating_focus_handoffs
            .get_mut(&(connection, public_id.to_owned()))
        {
            if source.preserve_preedit
                && self.input_method_preedit.is_active()
                && self.client_input.keyboard_focus() == Some(source.source)
            {
                // The protocol focus never left the source, so Return has no
                // reverse seat transition to perform. Keeping it there also
                // preserves the same live composer through the primary frame.
                source.transfer = None;
                source.preserve_preedit = false;
                return;
            }
            if self.input_method_preedit.is_active()
                && self.client_input.keyboard_focus() == Some(child)
            {
                source.preserve_preedit = true;
            }
            source.transfer =
                (self.client_input.keyboard_focus() == Some(child)).then_some(Transfer::Return {
                    child,
                    presented: false,
                });
        }
    }

    pub(crate) fn floating_return_presented(&mut self, connection: u64, public_id: &str) -> bool {
        if let Some(source) = self
            .floating_focus_handoffs
            .get_mut(&(connection, public_id.to_owned()))
            && let Some(Transfer::Return { presented, .. }) = source.transfer.as_mut()
        {
            let changed = !*presented;
            *presented = true;
            return changed;
        }
        false
    }

    /// Called for every actual seat transition. Going away and back cannot
    /// resurrect an old transfer, even if the final surface ID matches.
    pub(crate) fn cancel_floating_focus_on_transition(&mut self, focus: Option<u64>) {
        for source in self.floating_focus_handoffs.values_mut() {
            let expected = match source.transfer {
                Some(Transfer::Float) => source.source,
                Some(Transfer::Return { child, .. }) => child,
                None => continue,
            };
            if focus != Some(expected) {
                source.transfer = None;
            }
        }
    }

    pub(crate) fn retire_floating_focus_source(&mut self, surface: u64) {
        self.floating_focus_handoffs
            .retain(|_, source| source.source != surface);
    }

    /// Keep the real Wayland text-input focus on the exact source while its
    /// trusted input method owns an unfinished composition. The requested
    /// target is still authenticated against both sides of the compositor's
    /// one-shot floating owner binding; an unrelated surface never qualifies.
    pub(crate) fn preserves_floating_preedit_for(&self, target: u64) -> bool {
        self.floating_preedit_source_for(target).is_some()
    }

    /// The real source seat retained for `target`'s exact authenticated
    /// floating handoff. This is also the sole closed observation used by
    /// native acceptance; an app cannot assert or populate it.
    pub(crate) fn floating_preedit_source_for(&self, target: u64) -> Option<u64> {
        self.input_method_preedit.is_active().then_some(())?;
        self.floating_focus_handoffs
            .iter()
            .find_map(|((connection, public_id), source)| {
                (source.preserve_preedit
                    && matches!(source.transfer, Some(Transfer::Float))
                    && self.client_input.keyboard_focus() == Some(source.source)
                    && self.floating_toplevels.iter().any(|surface| {
                        surface.surface_id == target
                            && surface.connection == *connection
                            && surface.public_id == *public_id
                            && surface.owner.as_deref() == Some(source.owner.as_ref())
                            && !surface.suspended
                            && surface.presented
                            && surface.transfer_source.is_none()
                    })
                    && self
                        .floating_owner_for_surface(source.source)
                        .ok()
                        .as_deref()
                        == Some(source.owner.as_ref()))
                .then_some(source.source)
            })
    }

    /// The exact floating child whose real protocol seat remains active while
    /// its same-owner editor is already presented again in `target`.
    pub(crate) fn floating_preedit_child_for(&self, target: u64) -> Option<u64> {
        self.input_method_preedit.is_active().then_some(())?;
        self.floating_focus_handoffs
            .iter()
            .find_map(|((connection, public_id), source)| {
                let Some(Transfer::Return {
                    child,
                    presented: true,
                }) = source.transfer
                else {
                    return None;
                };
                (source.preserve_preedit
                    && source.source == target
                    && self.client_input.keyboard_focus() == Some(child)
                    && self.client_input.surface(target).is_some()
                    && self.client_input.surface(child).is_some()
                    && self.floating_toplevels.iter().any(|surface| {
                        surface.surface_id == child
                            && surface.connection == *connection
                            && surface.public_id == *public_id
                            && surface.owner.as_deref() == Some(source.owner.as_ref())
                            && surface.suspended
                            && surface.transfer_source.is_none()
                    })
                    && self
                        .floating_owner_for_surface(source.source)
                        .ok()
                        .as_deref()
                        == Some(source.owner.as_ref())
                    && self.floating_owner_for_surface(child).ok().as_deref()
                        == Some(source.owner.as_ref()))
                .then_some(child)
            })
    }

    /// The exact source remains the protocol focus while the editor's live
    /// composition is painted in its authenticated floating child.
    pub(crate) fn floating_preedit_source_focus(&self) -> Option<u64> {
        self.input_method_preedit.is_active().then_some(())?;
        self.floating_focus_handoffs
            .iter()
            .find_map(|((connection, public_id), source)| {
                (source.preserve_preedit
                    && matches!(source.transfer, Some(Transfer::Float))
                    && self.client_input.keyboard_focus() == Some(source.source)
                    && self.floating_toplevels.iter().any(|surface| {
                        surface.connection == *connection
                            && surface.public_id == *public_id
                            && surface.owner.as_deref() == Some(source.owner.as_ref())
                            && !surface.suspended
                    })
                    && self
                        .floating_owner_for_surface(source.source)
                        .ok()
                        .as_deref()
                        == Some(source.owner.as_ref()))
                .then_some(source.source)
            })
    }

    /// Preserve the exact child seat while the reverse proxy awaits its
    /// destination frame. This is not autofocus: a real seat change cancels it.
    pub(crate) fn floating_return_focus(&self) -> Option<u64> {
        self.floating_focus_handoffs
            .values()
            .find_map(|source| match source.transfer {
                Some(Transfer::Return { child, .. })
                    if self.client_input.keyboard_focus() == Some(child)
                        && self.client_input.surface(child).is_some()
                        && self
                            .floating_owner_for_surface(source.source)
                            .ok()
                            .as_deref()
                            == Some(source.owner.as_ref())
                        && self.floating_owner_for_surface(child).ok().as_deref()
                            == Some(source.owner.as_ref()) =>
                {
                    Some(child)
                }
                _ => None,
            })
    }

    pub(crate) fn apply_floating_focus_handoffs(
        &mut self,
        fb: &crate::compositor::frame::FrameBuilder,
    ) -> Option<u64> {
        let candidate = self
            .floating_focus_handoffs
            .iter()
            .find_map(|(key, source)| {
                let surface = self.floating_toplevels.iter().find(|surface| {
                    surface.connection == key.0
                        && surface.public_id == key.1
                        && surface.owner.as_deref() == Some(source.owner.as_ref())
                })?;
                if self
                    .floating_owner_for_surface(source.source)
                    .ok()
                    .as_deref()
                    != Some(source.owner.as_ref())
                    || self
                        .floating_owner_for_surface(surface.surface_id)
                        .ok()
                        .as_deref()
                        != Some(source.owner.as_ref())
                {
                    return None;
                }
                let target = match source.transfer? {
                    Transfer::Float
                        if !surface.suspended
                            && surface.presented
                            && surface.transfer_source.is_none()
                            && fb.floating_surface_rect(surface.surface_id).is_some()
                            && self.client_input.keyboard_focus() == Some(source.source) =>
                    {
                        surface.surface_id
                    }
                    Transfer::Return {
                        child,
                        presented: true,
                    } if surface.suspended
                        && surface.transfer_source.is_none()
                        && self.client_input.keyboard_focus() == Some(child) =>
                    {
                        source.source
                    }
                    _ => return None,
                };
                self.client_input.surface(target)?;
                if matches!(
                    source.transfer,
                    Some(Transfer::Float | Transfer::Return { .. })
                ) && source.preserve_preedit
                    && self.input_method_preedit.is_active()
                {
                    return None;
                }
                Some((key.clone(), target))
            });
        if let Some((key, target)) = candidate {
            let source = self.floating_focus_handoffs.get_mut(&key)?;
            source.transfer = None;
            self.set_activated_toplevel(Some(target));
            self.set_client_keyboard_focus(Some(target));
            return Some(target);
        }
        None
    }
}

#[cfg(test)]
#[path = "floating_focus_tests.rs"]
mod tests;
