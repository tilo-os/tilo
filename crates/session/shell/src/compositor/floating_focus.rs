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
    pub(crate) fn arm_floating_focus(
        &mut self,
        connection: u64,
        public_id: &str,
        source: u64,
        owner: Arc<FloatingToplevelOwner>,
    ) {
        let key = (connection, public_id.to_owned());
        let preserve_preedit = self.input_method_preedit.is_active();
        let transfer =
            (self.client_input.keyboard_focus() == Some(source)).then_some(Transfer::Float);
        let already_detached = self.floating_toplevels.iter().any(|surface| {
            surface.connection == connection && surface.public_id == public_id && !surface.suspended
        }) || self.pending_floating_toplevels.iter().any(|surface| {
            surface.connection == connection && surface.public_id == public_id && !surface.suspended
        });
        if let Some(previous) = self.floating_focus_handoffs.get_mut(&key) {
            if !already_detached && previous.source == source && previous.owner == owner {
                previous.transfer = transfer;
                previous.preserve_preedit = preserve_preedit;
            }
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
        self.input_method_preedit.is_active()
            && self
                .floating_focus_handoffs
                .iter()
                .any(|((connection, public_id), source)| {
                    source.preserve_preedit
                        && matches!(source.transfer, Some(Transfer::Float))
                        && self.client_input.keyboard_focus() == Some(source.source)
                        && self.floating_toplevels.iter().any(|surface| {
                            surface.surface_id == target
                                && surface.connection == *connection
                                && surface.public_id == *public_id
                                && surface.owner.as_deref() == Some(source.owner.as_ref())
                                && !surface.suspended
                        })
                        && self
                            .floating_owner_for_surface(source.source)
                            .ok()
                            .as_deref()
                            == Some(source.owner.as_ref())
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
                if matches!(source.transfer, Some(Transfer::Float))
                    && source.preserve_preedit
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
