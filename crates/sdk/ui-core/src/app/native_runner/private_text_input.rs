//! Original-field admission and receipts beside standard Wayland text delivery.
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use taffy::NodeId;
use winit::event::{Ime, PrivateIme, PrivateImeKind, WindowEvent};
use winit::window::Window;

use crate::app::App;
use crate::webview::text_input::{Failure, Receipt};

#[cfg(test)]
mod tests;

pub(super) trait Transport {
    fn publish(&self, snapshot: Option<[u8; 32]>) -> bool;
    fn acknowledge(&self, serial: u32, outcome: u32);
    fn redraw(&self);
}
impl Transport for Window {
    fn publish(&self, snapshot: Option<[u8; 32]>) -> bool {
        tilo_presentation::text_input::publish(self, snapshot)
    }
    fn acknowledge(&self, serial: u32, outcome: u32) {
        tilo_presentation::text_input::acknowledge(self, serial, outcome);
    }
    fn redraw(&self) {
        self.request_redraw();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Scope {
    node: NodeId,
    stamp: [u8; 32],
}
struct Grant {
    serial: u32,
    scope: Scope,
    receipt: Option<Receipt>,
}
struct Pending {
    serial: u32,
    kind: PrivateImeKind,
    scope: Scope,
    receipt: Receipt,
}
struct Final {
    serial: u32,
    scope: Option<Scope>,
}
pub(super) struct Bridge {
    key: [u8; 32],
    advertised: Option<Scope>,
    warm: Option<Grant>,
    started: Option<Grant>,
    pending: Option<Pending>,
    final_delivery: Option<Final>,
}
impl Default for Bridge {
    fn default() -> Self {
        let mut key = [0; 32];
        UnwrapErr(SysRng).fill_bytes(&mut key);
        Self {
            key,
            advertised: None,
            warm: None,
            started: None,
            pending: None,
            final_delivery: None,
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Bridge {
    fn scope(&self, app: &App) -> Option<Scope> {
        let node = app.focused_node?;
        let stamp = app
            .inspect_node(node, |widget| {
                (widget.is_enabled()
                    && widget.ime_cursor_rect().is_some()
                    && widget.ime_requires_deferred_ack()
                    && widget.ime_purpose().is_some())
                .then(|| widget.ime_edit_fingerprint(&self.key))
                .flatten()
            })
            .ok()
            .flatten()?;
        Some(Scope {
            node,
            stamp: crate::ime::scoped_fingerprint(&u64::from(node).to_le_bytes(), stamp, &self.key),
        })
    }
    fn cancel(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.receipt.cancel();
        }
        for grant in [self.started.take(), self.warm.take()]
            .into_iter()
            .flatten()
        {
            if let Some(receipt) = grant.receipt {
                receipt.cancel();
            }
        }
        // A queued final must still consume its standard commit after cancellation.
        if let Some(final_delivery) = self.final_delivery.as_mut() {
            final_delivery.scope = None;
        }
    }
    pub(super) fn admitted(&self) -> bool {
        self.warm.is_some()
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn busy(&self) -> bool {
        self.admitted() || self.pending() || self.final_delivery.is_some()
    }

    pub(super) fn service(&mut self, app: &App, window: &impl Transport, allowed: bool) {
        // Start cannot retain an old field's authority while its renderer reply
        // is pending. Revalidate before accepting even an already queued reply.
        if let Some(pending) = self.pending.as_ref()
            && pending.kind == PrivateImeKind::Start
            && (!allowed || self.scope(app) != Some(pending.scope))
        {
            let serial = pending.serial;
            self.cancel();
            window.acknowledge(serial, 1);
        }
        // Deliver the actual private outcome before post-commit field retirement.
        if let Some(result) = self
            .pending
            .as_ref()
            .and_then(|pending| pending.receipt.poll())
            && let Some(pending) = self.pending.take()
        {
            let outcome = match result {
                Ok(None) => 0,
                Ok(Some(_)) | Err(Failure::Unknown) => 2,
                Err(_) => 1,
            };
            window.acknowledge(pending.serial, outcome);
            if outcome == 0 && pending.kind == PrivateImeKind::Start {
                self.started = Some(Grant {
                    serial: pending.serial,
                    scope: pending.scope,
                    receipt: Some(pending.receipt),
                });
            } else {
                self.cancel();
            }
        }
        let mut next = allowed.then(|| self.scope(app)).flatten();
        // Natural retirement during an asynchronous transaction is settled by its
        // receipt. A focus move still revokes the original scope immediately.
        if let Some(pending) = self.pending.as_ref()
            && pending.kind == PrivateImeKind::Final
            && allowed
            && app.focused_node == Some(pending.scope.node)
        {
            next = Some(pending.scope);
        }
        if self.advertised != next {
            self.cancel();
            self.advertised = next;
            if !window.publish(next.map(|scope| scope.stamp)) {
                self.cancel();
                self.advertised = None;
            }
        }
    }

    fn operation(&mut self, app: &mut App, window: &impl Transport, event: PrivateIme) {
        if event.kind == PrivateImeKind::Cancel {
            let matches = self.warm.as_ref().is_some_and(|grant| {
                grant.serial == event.serial && grant.scope.stamp == event.snapshot
            }) || self.started.as_ref().is_some_and(|grant| {
                grant.serial == event.serial && grant.scope.stamp == event.snapshot
            }) || self.pending.as_ref().is_some_and(|pending| {
                pending.serial == event.serial && pending.scope.stamp == event.snapshot
            }) || self.final_delivery.as_ref().is_some_and(|delivery| {
                delivery.serial == event.serial
                    && delivery
                        .scope
                        .is_some_and(|scope| scope.stamp == event.snapshot)
            });
            if matches {
                self.cancel();
            }
            return;
        }
        let scope = self
            .advertised
            .filter(|scope| scope.stamp == event.snapshot)
            .filter(|scope| self.scope(app) == Some(*scope));
        if event.kind == PrivateImeKind::Final {
            let admitted = scope
                .filter(|scope| {
                    self.started
                        .as_ref()
                        .is_some_and(|grant| grant.scope == *scope)
                })
                .filter(|_| self.pending.is_none() && self.final_delivery.is_none());
            // Reserve even rejected finals: never dispatch their text to a successor.
            self.final_delivery = Some(Final {
                serial: event.serial,
                scope: admitted,
            });
            return;
        }
        let Some(scope) = scope else {
            window.acknowledge(event.serial, 1);
            return;
        };
        match event.kind {
            PrivateImeKind::Prepare if !self.busy() => {
                self.warm = Some(Grant {
                    serial: event.serial,
                    scope,
                    receipt: None,
                });
                // sync_ime applies the purpose before standard enable. Preparation
                // grants no renderer insertion and starts no audio capture.
                window.acknowledge(event.serial, 0);
            }
            PrivateImeKind::Start
                if self.pending.is_none()
                    && self.started.is_none()
                    && self.warm.as_ref().is_some_and(|grant| grant.scope == scope) =>
            {
                let receipt = app
                    .mutate_node(scope.node, |widget| widget.start_deferred_text())
                    .ok()
                    .flatten();
                if let Some(receipt) = receipt {
                    self.pending = Some(Pending {
                        serial: event.serial,
                        kind: event.kind,
                        scope,
                        receipt,
                    });
                    window.redraw();
                } else {
                    window.acknowledge(event.serial, 1);
                }
            }
            _ => window.acknowledge(event.serial, 1),
        }
    }
    pub(super) fn event(
        &mut self,
        app: &mut App,
        window: &impl Transport,
        event: &WindowEvent,
    ) -> bool {
        if let WindowEvent::PrivateIme(event) = event {
            self.operation(app, window, *event);
            return true;
        }
        if let WindowEvent::Ime(Ime::Commit(text)) = event {
            if let Some(final_delivery) = self.final_delivery.take() {
                let receipt = final_delivery
                    .scope
                    .filter(|scope| self.scope(app) == Some(*scope))
                    .and_then(|scope| {
                        app.mutate_node(scope.node, |widget| widget.commit_deferred_text(text))
                            .ok()
                            .flatten()
                            .map(|receipt| (scope, receipt))
                    });
                if let Some((scope, receipt)) = receipt {
                    self.pending = Some(Pending {
                        serial: final_delivery.serial,
                        kind: PrivateImeKind::Final,
                        scope,
                        receipt,
                    });
                    window.redraw();
                } else {
                    window.acknowledge(final_delivery.serial, 1);
                }
                return true;
            }
            return self.admitted();
        }
        matches!(event, WindowEvent::Ime(Ime::Preedit(..))) && self.admitted()
    }
}
