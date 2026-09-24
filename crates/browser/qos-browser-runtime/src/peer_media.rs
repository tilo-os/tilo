//! Private media participation over shared delivery authority.
pub(crate) use crate::peer_delivery_auth::{
    Bootstrap, OwnerView, PreparedRequest, VerifiedBootstrap,
};
use crate::peer_delivery_auth::{DeliveryAuthority, HttpStep, fail, time};
use ed25519_dalek::Signer;
#[cfg(test)]
use ed25519_dalek::SigningKey;
#[cfg(test)]
use qos_peer_state::StatePeer;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use tilo_relay::delivery_v3::{
    descriptor::{self, VerifiedDescriptor},
    media_wire,
};
#[cfg(test)]
use tilo_user_identity::{DeviceCert, RevocationList};

pub(crate) enum Step {
    Request(PreparedRequest),
    HttpReady,
    Grant(control::GrantView),
    Relay(control::RelayView),
    Presence(control::PresenceView),
    Refreshed,
    Left,
    RelayClosed,
}
enum Pending {
    Control {
        intent: u32,
        stage: control::ControlStage,
    },
    Http {
        request: u32,
    },
}
pub(crate) struct PeerMedia {
    authority: Rc<RefCell<DeliveryAuthority>>,
    pending: BTreeMap<u32, Pending>,
    next: u32,
    closed: bool,
    suspended: bool,
    resume: Option<control::ResumeIntent>,
    participation: Option<control::Participation>,
    ending: Option<control::Participation>,
}
impl PeerMedia {
    pub fn new(
        lease: &str,
        descriptor: VerifiedDescriptor,
        floor: crate::peer_journal::AcknowledgedDescriptorFloor,
    ) -> Result<Self, String> {
        Ok(Self::with_authority(Rc::new(RefCell::new(
            DeliveryAuthority::new(lease, descriptor, floor)?,
        ))))
    }
    pub(crate) fn with_authority(authority: Rc<RefCell<DeliveryAuthority>>) -> Self {
        Self {
            authority,
            pending: BTreeMap::new(),
            next: 0,
            closed: false,
            suspended: false,
            resume: None,
            participation: None,
            ending: None,
        }
    }
    fn check(&self, view: &OwnerView<'_>) -> Result<(), String> {
        if self.closed || self.suspended {
            return Err("media custody unavailable".into());
        }
        self.authority.borrow().check(view)
    }
    fn request(
        &mut self,
        target: &'static str,
        body: Vec<u8>,
        proof: Option<Vec<u8>>,
        pending: Pending,
    ) -> Result<PreparedRequest, String> {
        if self.pending.len() >= 8 {
            return Err(fail("media control capacity"));
        }
        let id = self
            .next
            .checked_add(1)
            .ok_or("media operation counter exhausted")?;
        self.next = id;
        self.pending.insert(id, pending);
        Ok(PreparedRequest {
            id,
            origin: self.authority.borrow().descriptor.origin().into(),
            target,
            body,
            proof,
        })
    }
    fn wrap_http(&mut self, prepared: PreparedRequest) -> Result<PreparedRequest, String> {
        let inner = prepared.id;
        match self.request(
            prepared.target,
            prepared.body,
            prepared.proof,
            Pending::Http { request: inner },
        ) {
            Ok(request) => Ok(request),
            Err(error) => {
                self.authority.borrow_mut().forget(inner);
                Err(error)
            }
        }
    }
    pub fn start_http(&mut self, view: &OwnerView<'_>) -> Result<PreparedRequest, String> {
        self.check(view)?;
        if !self.pending.is_empty() {
            return Err("media HTTP handshake already active".into());
        }
        let request = self.authority.borrow_mut().start_http(view)?;
        self.wrap_http(request)
    }
    pub fn advance(&mut self, view: &OwnerView<'_>, id: u32, bytes: &[u8]) -> Result<Step, String> {
        self.check(view)?;
        let pending = self
            .pending
            .remove(&id)
            .ok_or("unknown or consumed media operation")?;
        if bytes.len() > 8192 {
            if let Pending::Http { request } = pending {
                self.authority.borrow_mut().forget(request);
            }
            return Err("oversized media response".into());
        }
        match pending {
            Pending::Control { intent, stage } => {
                if self.ending.as_ref().is_some_and(|p| p.id == intent) {
                    let live = self.participation.take();
                    self.participation = self.ending.take();
                    let result = self.advance_control(view, intent, stage, bytes);
                    self.ending = self.participation.take();
                    self.participation = live;
                    result
                } else {
                    self.advance_control(view, intent, stage, bytes)
                }
            }
            Pending::Http { request } => {
                let step = self.authority.borrow_mut().advance(view, request, bytes)?;
                match step {
                    HttpStep::Request(request) => self.wrap_http(request).map(Step::Request),
                    HttpStep::Ready => Ok(Step::HttpReady),
                }
            }
        }
    }
    pub fn forget(&mut self, id: u32) {
        if let Some(Pending::Http { request }) = self.pending.remove(&id) {
            self.authority.borrow_mut().forget(request);
        }
    }
    pub fn current(&self, view: &OwnerView<'_>) -> bool {
        !self.closed && !self.suspended && self.authority.borrow().current(view)
    }
    pub fn supported(&self, view: &OwnerView<'_>) -> bool {
        self.current(view)
            && self.authority.borrow().descriptor.media_capabilities() & descriptor::MEDIA_VOICE_V1
                != 0
    }
    pub fn http_ready(&self) -> bool {
        !self.closed && !self.suspended && self.authority.borrow().http_ready()
    }
    /// Retire expired authorization while preserving only established user intent.
    pub fn expire_authorization(&mut self) -> bool {
        if self.closed || self.suspended {
            return false;
        }
        self.suspended = true;
        self.clear_pending();
        if self.resume.is_none() {
            self.resume = self.participation.as_ref().and_then(|part| {
                let context = part.context.as_ref()?;
                part.grant.as_ref()?;
                (!part.leaving).then(|| control::ResumeIntent {
                    handle: part.id,
                    request: part.request.clone(),
                    epoch: context.epoch,
                    replacement: None,
                })
            });
        }
        self.participation = None;
        self.ending = None;
        true
    }
    pub fn rebind_authority(
        &mut self,
        view: &OwnerView<'_>,
        authority: Rc<RefCell<DeliveryAuthority>>,
    ) -> Result<(), String> {
        if self.closed {
            return Err("media intent explicitly closed".into());
        }
        authority.borrow().check(view)?;
        if !authority.borrow().http_ready() {
            return Err("fresh HTTP custody unavailable".into());
        }
        if !self.suspended && !Rc::ptr_eq(&self.authority, &authority) {
            return Err("media authority replacement requires expiry fence".into());
        }
        self.authority = authority;
        self.suspended = false;
        Ok(())
    }
    fn clear_pending(&mut self) {
        for (_, pending) in std::mem::take(&mut self.pending) {
            if let Pending::Http { request } = pending {
                self.authority.borrow_mut().forget(request);
            }
        }
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.clear_pending();
        self.resume = None;
        self.participation = None;
        self.ending = None;
    }
}
mod control;
#[cfg(test)]
mod tests;
