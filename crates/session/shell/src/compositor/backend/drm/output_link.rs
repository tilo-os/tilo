//! Turning one output on and off without ever waiting for it.
//!
//! Lighting a display is a modeset, and a modeset can take seconds in the
//! kernel: a DP sink trains its link first, and an msm DP link that
//! fails training spends its whole timeout there and then leaves the CRTC on
//! with nothing scanned out. A legacy `set_crtc` blocks the caller for all of
//! it, and with one render thread presenting every output, waking the
//! displays froze the built-in panel and the pointer with them.
//!
//! So where the driver takes atomic commits, each output's power change is
//! one nonblocking atomic commit of that output alone (its connector, CRTC and
//! primary plane, `ALLOW_MODESET | NONBLOCK | PAGE_FLIP_EVENT`), completed by
//! its page-flip event on the card's event stream. Until the output is proven
//! up it presents nothing, so no frame of any other output waits for it.
//!
//! Proven up means a vblank happened after the modeset. The driver sends the
//! modeset's event even when the link never came up (after its commit-done
//! timeout), so the event alone says nothing. Right after it, the CRTC's
//! vblank counter is read and a nonblocking commit of the same framebuffer is
//! queued; its event lands on a later vblank, with a higher count, only on a
//! CRTC that scans out. The same count means no vblank happened: the link is
//! down. The output is then switched off and reported failed, and it is not
//! committed again until something real changes (a replug, a display
//! settings change), never on a timer.
//!
//! Drivers without vblank counters (the query fails) cannot be checked and
//! count as up once the modeset completes. Outputs whose flips stay legacy
//! (virtual connectors, [`super::kms_flip::KMS_ATOMIC_ENV`]`=0`, a driver
//! without atomic) keep their blocking `set_crtc`: there is nothing to commit
//! atomically with.

use std::io;

use smithay::reexports::drm as drm_re;

use drm_re::Device as _;
use drm_re::control::atomic::AtomicModeReq;
use drm_re::control::{
    AtomicCommitFlags, Device as ControlDevice, Mode, connector, framebuffer, property,
};
use drm_re::{VblankWaitFlags, VblankWaitTarget};

use super::kms_flip::{FlipTarget, PlaneCommit, PrimaryPlane, plane_request};
use super::output_signal::find_property;
use super::scanout::Card;

/// Whether an output is wanted lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Power {
    On,
    Off,
}

/// One commit an [`OutputLink`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkChange {
    /// Light the output. `full` also routes the connector to the CRTC and
    /// sets the mode: the CRTC is fresh, or another DRM master had it.
    On { full: bool },
    /// The CRTC's vblank-proof commit: the framebuffer it shows, again.
    Probe,
    /// Switch the CRTC off and keep its mode and routing, as DPMS does.
    Off,
}

/// The KMS calls an [`OutputLink`] makes, so it can be proven without a card.
pub(super) trait LinkDevice {
    /// Commit `change`, nonblocking, with a page-flip event.
    ///
    /// # Errors
    ///
    /// What the kernel answered: `EBUSY` while an earlier commit on the CRTC
    /// has not completed, `EACCES` without DRM master, anything else a
    /// refusal.
    fn commit(&self, change: LinkChange) -> io::Result<()>;

    /// The CRTC's vblank count now, without waiting for one. `None` when the
    /// driver keeps no vblank counter for it.
    fn vblank_count(&self) -> Option<u32>;
}

/// Why an output could not be lit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LinkFailure {
    /// The modeset completed but no vblank followed: the link to the sink
    /// is down (a DP link that failed training, a cable or adapter
    /// fault).
    NoVblank,
    /// The driver refused to light it.
    Refused(String),
}

impl LinkFailure {
    /// One line for the session log.
    pub(super) fn describe(&self) -> String {
        match self {
            Self::NoVblank => {
                "no vblank after the modeset: the link to the display did not come up".to_owned()
            }
            Self::Refused(error) => format!("the driver refused to light the output: {error}"),
        }
    }
}

/// What a page-flip event for the output's CRTC turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Completion {
    /// It completed a commit of the link's own; nothing else may take it.
    Link(LinkStep),
    /// It completed a flip the presenter queued.
    Flip,
}

/// Where a link's own completion left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkStep {
    /// Still on the way (the modeset is done, the probe follows; or the
    /// next wanted commit was queued).
    Pending,
    /// A vblank followed the modeset (or the driver cannot tell): the
    /// output presents from now on.
    Up,
    /// The link did not come up. The output is being switched off.
    Failed,
    /// The link did not come up, and the wake this output is in may redo
    /// its modeset once: the CRTC is switched off, then modeset from
    /// scratch ([`super::wake_check`]).
    Redone,
    Off,
}

/// The vblank proof after a modeset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Proof {
    /// Not proving anything.
    Idle,
    /// The modeset completed; the probe commit is still to be accepted.
    Due,
    /// The probe is in flight; the vblank count read when it was queued.
    Awaiting(u32),
}

/// One output's power, committed one nonblocking atomic commit at a time.
///
/// At most one of the link's own commits is in flight, and none while a
/// flip the presenter queued is (the kernel would refuse it as busy). A
/// change wanted meanwhile is committed when the earlier one completes, or
/// on the presenter's next pass ([`Self::advance`]).
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is one independent fact about the link's commits (a full modeset owed, a commit in flight, a flip event due first, proven up); every combination is reachable"
)]
pub(super) struct OutputLink {
    wanted: Power,
    /// What the last accepted modeset asked for.
    committed: Power,
    /// The next `On` routes the connector and sets the mode.
    full: bool,
    /// The link's own commit whose event has not arrived yet.
    in_flight: bool,
    /// Events still due for flips queued before that commit: the kernel
    /// accepts a commit once a flip's event is sent, before it is read.
    flips_ahead: bool,
    proof: Proof,
    /// The output is proven up and presents.
    up: bool,
    failure: Option<LinkFailure>,
    /// The wake the output is in may redo its modeset once when no vblank
    /// follows it, instead of failing the output.
    redo_owed: bool,
    /// The CRTC is to be switched off on the way to a full modeset: the
    /// redo of a wake's modeset. Re-lighting a CRTC the kernel still holds
    /// active with the same mode changes nothing; from off it retrains the
    /// link and re-enables the panel.
    remodeset: bool,
}

impl OutputLink {
    /// A fresh output, wanted lit, on a CRTC it has not modeset yet.
    pub(super) fn new() -> Self {
        Self {
            wanted: Power::On,
            committed: Power::Off,
            full: true,
            in_flight: false,
            flips_ahead: false,
            proof: Proof::Idle,
            up: false,
            failure: None,
            redo_owed: false,
            remodeset: false,
        }
    }

    /// The output may present: lit, proven up, nothing of the link's own
    /// pending.
    pub(super) fn presentable(&self) -> bool {
        self.up && self.failure.is_none() && self.wanted == Power::On && !self.in_flight
    }

    /// Something is still to happen: a commit in flight, a wanted change or
    /// proof not committed yet. The presenter reads the card's events and
    /// asks for frames while it is.
    pub(super) fn busy(&self) -> bool {
        self.in_flight || self.proof != Proof::Idle || self.next_change().is_some()
    }

    /// Why the output is dark, once it failed.
    pub(super) fn failure(&self) -> Option<&LinkFailure> {
        self.failure.as_ref()
    }

    /// The output is wanted lit.
    pub(super) fn wanted_on(&self) -> bool {
        self.wanted == Power::On
    }

    /// Want the output on or off, and commit that when nothing is in the way.
    /// `flip_unread`: a flip the presenter queued has not had its event read.
    pub(super) fn want(&mut self, power: Power, device: &impl LinkDevice, flip_unread: bool) {
        self.wanted = power;
        if power == Power::Off {
            self.up = false;
            self.proof = Proof::Idle;
        }
        self.advance(device, flip_unread);
    }

    /// Another DRM master had the CRTC, or the machine slept: the next `On`
    /// modesets it from scratch. The device was powered down or handed
    /// away and back, which is a real change: a link that failed before is
    /// tried again.
    pub(super) fn resume(&mut self, device: &impl LinkDevice, flip_unread: bool) {
        self.failure = None;
        self.full = true;
        self.committed = Power::Off;
        self.up = false;
        self.proof = Proof::Idle;
        self.advance(device, flip_unread);
    }

    /// Let the wake the output is entering redo its modeset once when no
    /// vblank follows it ([`LinkStep::Redone`]).
    pub(super) fn arm_wake_redo(&mut self) {
        self.redo_owed = true;
    }

    /// The wake is over: a later missing vblank fails the output.
    pub(super) fn disarm_wake_redo(&mut self) {
        self.redo_owed = false;
    }

    /// Redo the modeset from the CRTC switched off: the wake's one recovery
    /// when the link came up but the output is still not lit (its CRTC
    /// reads back inactive, its panel's backlight stayed blanked).
    pub(super) fn redo(&mut self, device: &impl LinkDevice, flip_unread: bool) {
        if self.failure.is_some() {
            return;
        }
        self.redo_owed = false;
        self.remodeset = true;
        self.up = false;
        self.proof = Proof::Idle;
        self.advance(device, flip_unread);
    }

    /// The commit the link still owes, if any.
    fn next_change(&self) -> Option<LinkChange> {
        if self.failure.is_some() {
            // A failed output only ever goes off.
            return (self.committed == Power::On).then_some(LinkChange::Off);
        }
        if self.remodeset && self.committed == Power::On {
            return Some(LinkChange::Off);
        }
        match (self.wanted, self.committed) {
            (Power::On, Power::Off) => Some(LinkChange::On { full: self.full }),
            (Power::On, Power::On) if self.proof == Proof::Due => Some(LinkChange::Probe),
            (Power::Off, Power::On) => Some(LinkChange::Off),
            _ => None,
        }
    }

    /// Commit what the link owes, unless one of its commits is in flight.
    /// A busy CRTC or a lost DRM master leaves it owed for the next pass; a
    /// refused `On` fails the output.
    pub(super) fn advance(&mut self, device: &impl LinkDevice, flip_unread: bool) {
        if self.in_flight {
            return;
        }
        let Some(change) = self.next_change() else {
            return;
        };
        match device.commit(change) {
            Ok(()) => {
                self.in_flight = true;
                self.flips_ahead = flip_unread;
                match change {
                    LinkChange::On { .. } => {
                        self.committed = Power::On;
                        self.full = false;
                        self.up = false;
                    }
                    LinkChange::Probe => {}
                    LinkChange::Off => {
                        self.committed = Power::Off;
                        self.up = false;
                        self.end_remodeset_off();
                    }
                }
            }
            Err(error) if retry_later(&error) => {}
            Err(error) => match change {
                LinkChange::On { .. } => {
                    self.failure = Some(LinkFailure::Refused(error.to_string()));
                }
                LinkChange::Probe => {
                    // The CRTC took the modeset but not its own framebuffer
                    // again: nothing can be proven, and nothing presents.
                    self.proof = Proof::Idle;
                    self.failure = Some(LinkFailure::Refused(error.to_string()));
                }
                LinkChange::Off => {
                    tracing::warn!(%error, "DRM scanout: switching the output off was refused");
                    // Retrying a refused commit every frame would be a loop.
                    self.committed = Power::Off;
                    self.end_remodeset_off();
                }
            },
        }
    }

    /// The off half of a redone modeset is committed: the on half routes
    /// the connector and sets the mode afresh.
    fn end_remodeset_off(&mut self) {
        if std::mem::take(&mut self.remodeset) {
            self.full = true;
        }
    }

    /// Take one page-flip event for this output's CRTC, carrying vblank
    /// `frame`. Events arrive in commit order.
    pub(super) fn complete(&mut self, frame: u32, device: &impl LinkDevice) -> Completion {
        if self.flips_ahead {
            self.flips_ahead = false;
            return Completion::Flip;
        }
        if !self.in_flight {
            return Completion::Flip;
        }
        self.in_flight = false;
        let failed_before = self.failure.is_some();
        let step = match (self.committed, self.proof) {
            (Power::Off, _) => {
                self.proof = Proof::Idle;
                LinkStep::Off
            }
            (Power::On, Proof::Awaiting(seen)) => {
                self.proof = Proof::Idle;
                if frame == seen && std::mem::take(&mut self.redo_owed) {
                    self.remodeset = true;
                    LinkStep::Redone
                } else if frame == seen {
                    self.failure = Some(LinkFailure::NoVblank);
                    LinkStep::Failed
                } else {
                    self.up = true;
                    LinkStep::Up
                }
            }
            (Power::On, Proof::Idle | Proof::Due) => {
                if let Some(count) = device.vblank_count() {
                    self.proof = Proof::Due;
                    // The probe is queued right away; its count is the one now.
                    self.advance_probe(device, count);
                    LinkStep::Pending
                } else {
                    self.up = true;
                    LinkStep::Up
                }
            }
        };
        if self.failure.is_some() || self.wanted != self.committed {
            self.up = false;
        }
        self.advance(device, false);
        if !failed_before && self.failure.is_some() {
            return Completion::Link(LinkStep::Failed);
        }
        if step == LinkStep::Up && !self.presentable() {
            return Completion::Link(LinkStep::Pending);
        }
        Completion::Link(step)
    }

    /// Queue the vblank probe, remembering the count it must beat.
    fn advance_probe(&mut self, device: &impl LinkDevice, count: u32) {
        if self.wanted != Power::On || self.failure.is_some() {
            self.proof = Proof::Idle;
            return;
        }
        match device.commit(LinkChange::Probe) {
            Ok(()) => {
                self.in_flight = true;
                self.flips_ahead = false;
                self.proof = Proof::Awaiting(count);
            }
            // Owed: the next pass commits it and reads the count again.
            Err(error) if retry_later(&error) => {}
            Err(error) => {
                self.proof = Proof::Idle;
                self.failure = Some(LinkFailure::Refused(error.to_string()));
            }
        }
    }

    /// The presenter's pass: commit what is owed. A probe owed since a busy
    /// CRTC refused it reads the vblank count again first.
    pub(super) fn service(&mut self, device: &impl LinkDevice, flip_unread: bool) {
        if self.in_flight {
            return;
        }
        if self.proof == Proof::Due && self.committed == Power::On && self.wanted == Power::On {
            if flip_unread {
                return;
            }
            if let Some(count) = device.vblank_count() {
                self.advance_probe(device, count);
            } else {
                self.proof = Proof::Idle;
                self.up = self.failure.is_none();
            }
            return;
        }
        self.advance(device, flip_unread);
    }
}

/// The CRTC is busy with an earlier commit, or the session lost DRM master:
/// the same commit may succeed later.
fn retry_later(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EBUSY | libc::EAGAIN | libc::EINTR | libc::EACCES | libc::EPERM)
    )
}

/// The connector and CRTC properties an output's modesets write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModesetProps {
    connector_crtc: property::Handle,
    active: property::Handle,
    mode_id: property::Handle,
}

/// Everything one output's atomic modesets name: its connector, CRTC (and
/// that CRTC's index, which vblank queries take), mode blob and primary
/// plane.
#[derive(Debug)]
pub(super) struct LinkTarget {
    connector: connector::Handle,
    flip: FlipTarget,
    pipe: u32,
    props: ModesetProps,
    mode_blob: u64,
    plane: PrimaryPlane,
}

impl LinkTarget {
    /// Look up `connector`'s and `crtc`'s modeset properties and create the
    /// blob for `mode`; `plane` is the primary plane the flips use. `None`
    /// (and the reason) when the driver lacks one of them: that output's
    /// modesets stay legacy.
    pub(super) fn probe(
        card: &Card,
        connector: connector::Handle,
        flip: FlipTarget,
        mode: Mode,
        plane: PrimaryPlane,
    ) -> Result<Self, String> {
        let prop = |object: &dyn Fn(&str) -> Option<property::Handle>, name: &str| {
            object(name).ok_or_else(|| format!("no {name} property for atomic modesets"))
        };
        let on_connector = |name: &str| find_property(card, connector, name).map(|(h, _, _)| h);
        let on_crtc = |name: &str| find_property(card, flip.crtc, name).map(|(h, _, _)| h);
        let props = ModesetProps {
            connector_crtc: prop(&on_connector, "CRTC_ID")?,
            active: prop(&on_crtc, "ACTIVE")?,
            mode_id: prop(&on_crtc, "MODE_ID")?,
        };
        let pipe = card
            .resource_handles()
            .map_err(|error| format!("list CRTCs: {error}"))?
            .crtcs()
            .iter()
            .position(|&handle| handle == flip.crtc)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or_else(|| "the CRTC is not in the card's resources".to_owned())?;
        let property::Value::Blob(mode_blob) = card
            .create_property_blob(&mode)
            .map_err(|error| format!("create the mode blob: {error}"))?
        else {
            return Err("the mode blob is not a blob".to_owned());
        };
        Ok(Self {
            connector,
            flip,
            pipe,
            props,
            mode_blob,
            plane,
        })
    }

    /// Release the mode blob; the target is not committed again.
    pub(super) fn destroy(&self, card: &Card) {
        if let Err(error) = card.destroy_property_blob(self.mode_blob) {
            tracing::debug!(%error, "DRM scanout: destroying the mode blob failed");
        }
    }

    /// The request for `change`, showing `framebuffer` where it lights.
    fn request(&self, change: LinkChange, framebuffer: framebuffer::Handle) -> AtomicModeReq {
        use property::Value;
        let plane = || {
            plane_request(
                &self.plane,
                PlaneCommit {
                    target: self.flip,
                    framebuffer,
                },
            )
        };
        let crtc = self.flip.crtc;
        match change {
            LinkChange::On { full } => {
                let mut request = plane();
                if full {
                    request.add_property(
                        self.connector,
                        self.props.connector_crtc,
                        Value::CRTC(Some(crtc)),
                    );
                    request.add_property(crtc, self.props.mode_id, Value::Blob(self.mode_blob));
                }
                request.add_property(crtc, self.props.active, Value::Boolean(true));
                request
            }
            LinkChange::Probe => plane(),
            LinkChange::Off => {
                let mut request = AtomicModeReq::new();
                request.add_property(crtc, self.props.active, Value::Boolean(false));
                request
            }
        }
    }

    /// Ask KMS, changing nothing, whether it would light this output: the
    /// one check a modeset gets on the loop thread, which must never wait
    /// for the hardware.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(super) fn test_on(&self, card: &Card, framebuffer: framebuffer::Handle) -> io::Result<()> {
        card.atomic_commit(
            AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET,
            self.request(LinkChange::On { full: true }, framebuffer),
        )
    }
}

/// A [`LinkDevice`] on a real card, lighting with `framebuffer`.
pub(super) struct AtomicLink<'a> {
    pub card: &'a Card,
    pub target: &'a LinkTarget,
    pub framebuffer: framebuffer::Handle,
}

impl LinkDevice for AtomicLink<'_> {
    fn commit(&self, change: LinkChange) -> io::Result<()> {
        let mut flags = AtomicCommitFlags::NONBLOCK | AtomicCommitFlags::PAGE_FLIP_EVENT;
        if change != LinkChange::Probe {
            flags |= AtomicCommitFlags::ALLOW_MODESET;
        }
        self.card
            .atomic_commit(flags, self.target.request(change, self.framebuffer))
    }

    fn vblank_count(&self) -> Option<u32> {
        // A relative wait for zero vblanks answers at once with the count.
        self.card
            .wait_vblank(
                VblankWaitTarget::Relative(0),
                VblankWaitFlags::empty(),
                self.target.pipe,
                0,
            )
            .ok()
            .map(|reply| reply.frame())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// A card that answers each commit as told and keeps a vblank counter.
    #[derive(Default)]
    struct FakeCrtc {
        commits: RefCell<Vec<LinkChange>>,
        /// The errno the next commits answer with, until cleared.
        refuse: Cell<Option<i32>>,
        vblank: Cell<Option<u32>>,
    }

    impl LinkDevice for FakeCrtc {
        fn commit(&self, change: LinkChange) -> io::Result<()> {
            if let Some(errno) = self.refuse.get() {
                return Err(io::Error::from_raw_os_error(errno));
            }
            self.commits.borrow_mut().push(change);
            Ok(())
        }

        fn vblank_count(&self) -> Option<u32> {
            self.vblank.get()
        }
    }

    fn counting(at: u32) -> FakeCrtc {
        FakeCrtc {
            vblank: Cell::new(Some(at)),
            ..FakeCrtc::default()
        }
    }

    #[test]
    fn a_healthy_link_presents_after_one_vblank_follows_the_modeset() {
        let crtc = counting(100);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        assert_eq!(*crtc.commits.borrow(), [LinkChange::On { full: true }]);
        assert!(
            !link.presentable(),
            "nothing presents while the modeset runs"
        );

        // The modeset's event: the probe follows at once.
        assert_eq!(
            link.complete(100, &crtc),
            Completion::Link(LinkStep::Pending)
        );
        assert_eq!(
            *crtc.commits.borrow(),
            [LinkChange::On { full: true }, LinkChange::Probe]
        );
        assert!(!link.presentable());

        assert_eq!(link.complete(101, &crtc), Completion::Link(LinkStep::Up));
        assert!(link.presentable());
        assert!(!link.busy());
        assert_eq!(link.complete(102, &crtc), Completion::Flip);
    }

    #[test]
    fn a_link_that_never_scans_out_fails_once_and_is_switched_off() {
        // msm DP after failed link training: the modeset and the probe both
        // complete only after the driver's timeout, and no vblank counts.
        let crtc = counting(7);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Pending));
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Failed));
        assert_eq!(link.failure(), Some(&LinkFailure::NoVblank));
        assert!(!link.presentable());
        assert_eq!(
            *crtc.commits.borrow(),
            [
                LinkChange::On { full: true },
                LinkChange::Probe,
                LinkChange::Off
            ],
            "the CRTC the driver left on is switched off"
        );
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Off));

        // Nothing commits it again: not a wake, not a pass, not an event.
        link.want(Power::On, &crtc, false);
        link.service(&crtc, false);
        link.want(Power::Off, &crtc, false);
        link.want(Power::On, &crtc, false);
        assert_eq!(crtc.commits.borrow().len(), 3);
        assert!(!link.busy());
        assert!(!link.presentable());
    }

    #[test]
    fn a_busy_crtc_defers_the_commit_to_a_later_pass() {
        let crtc = counting(1);
        crtc.refuse.set(Some(libc::EBUSY));
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        assert!(crtc.commits.borrow().is_empty());
        assert!(link.busy(), "the modeset is still owed");
        crtc.refuse.set(None);
        link.service(&crtc, false);
        assert_eq!(*crtc.commits.borrow(), [LinkChange::On { full: true }]);
    }

    #[test]
    fn a_refused_modeset_fails_the_output_without_retrying() {
        let crtc = counting(1);
        crtc.refuse.set(Some(libc::EINVAL));
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        assert!(matches!(link.failure(), Some(LinkFailure::Refused(_))));
        crtc.refuse.set(None);
        link.service(&crtc, false);
        link.want(Power::On, &crtc, false);
        assert!(crtc.commits.borrow().is_empty());
        assert!(!link.busy());
    }

    #[test]
    fn a_flip_event_read_after_the_modeset_was_accepted_stays_the_flips() {
        let crtc = counting(5);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        link.complete(5, &crtc);
        link.complete(6, &crtc);
        assert!(link.presentable());
        // Blank while the last frame's flip event is still unread.
        link.want(Power::Off, &crtc, true);
        assert_eq!(crtc.commits.borrow().last(), Some(&LinkChange::Off));
        assert_eq!(link.complete(7, &crtc), Completion::Flip);
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Off));
    }

    #[test]
    fn waking_during_the_blank_commits_once_the_blank_completes() {
        let crtc = counting(10);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        link.complete(10, &crtc);
        link.complete(11, &crtc);
        link.want(Power::Off, &crtc, false);
        // Woken before the blank's event: one commit at a time.
        link.want(Power::On, &crtc, false);
        assert_eq!(crtc.commits.borrow().last(), Some(&LinkChange::Off));
        assert_eq!(
            link.complete(12, &crtc),
            Completion::Link(LinkStep::Off),
            "the blank completed"
        );
        assert_eq!(
            crtc.commits.borrow().last(),
            Some(&LinkChange::On { full: false }),
            "and the wake follows from its event, without a full modeset"
        );
        assert_eq!(
            link.complete(13, &crtc),
            Completion::Link(LinkStep::Pending)
        );
        assert_eq!(link.complete(14, &crtc), Completion::Link(LinkStep::Up));
    }

    #[test]
    fn a_driver_without_vblank_counters_is_up_once_the_modeset_completes() {
        let crtc = FakeCrtc::default();
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        assert_eq!(link.complete(0, &crtc), Completion::Link(LinkStep::Up));
        assert!(link.presentable());
        assert_eq!(*crtc.commits.borrow(), [LinkChange::On { full: true }]);
    }

    #[test]
    fn a_stalled_or_failing_output_never_holds_back_another() {
        // The laptop with an external DP sink: waking both displays. The
        // panel's modeset completes at once; the DP one is still training
        // its link (its event has not come) and then fails. Nothing the
        // panel does waits for the DP CRTC, and the DP CRTC is committed
        // exactly once per step, never in a loop.
        let panel = counting(500);
        let dp = counting(40);
        let mut panel_link = OutputLink::new();
        let mut dp_link = OutputLink::new();
        for _ in 0..3 {
            // Every pass of the presenter, while the DP modeset is stuck.
            panel_link.service(&panel, false);
            dp_link.service(&dp, false);
        }
        assert_eq!(
            dp.commits.borrow().len(),
            1,
            "one modeset, not one per pass"
        );
        assert_eq!(
            panel_link.complete(500, &panel),
            Completion::Link(LinkStep::Pending)
        );
        assert_eq!(
            panel_link.complete(501, &panel),
            Completion::Link(LinkStep::Up)
        );
        assert!(
            panel_link.presentable(),
            "the panel presents beside the stalled DP"
        );
        assert!(!dp_link.presentable());
        assert!(dp_link.busy());
        // The panel's flips go on, frame after frame.
        for frame in 502..600 {
            assert_eq!(panel_link.complete(frame, &panel), Completion::Flip);
        }
        // Seconds later the DP driver gives up and sends both events late.
        assert_eq!(
            dp_link.complete(40, &dp),
            Completion::Link(LinkStep::Pending)
        );
        assert_eq!(
            dp_link.complete(40, &dp),
            Completion::Link(LinkStep::Failed)
        );
        assert_eq!(dp_link.complete(40, &dp), Completion::Link(LinkStep::Off));
        for _ in 0..3 {
            dp_link.service(&dp, false);
            dp_link.want(Power::On, &dp, false);
        }
        assert_eq!(dp.commits.borrow().len(), 3, "on, probe, off: then nothing");
        assert!(panel_link.presentable());
        assert_eq!(panel.commits.borrow().len(), 2);
    }

    #[test]
    fn a_wake_redoes_a_modeset_no_vblank_followed_once_from_the_crtc_switched_off() {
        // The panel after a resume: the modeset's event came, the probe's
        // event carried the same vblank count.
        let crtc = counting(7);
        let mut link = OutputLink::new();
        link.arm_wake_redo();
        link.service(&crtc, false);
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Pending));
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Redone));
        assert_eq!(link.failure(), None, "the output is not failed yet");
        assert!(!link.presentable());
        assert_eq!(
            *crtc.commits.borrow(),
            [
                LinkChange::On { full: true },
                LinkChange::Probe,
                LinkChange::Off
            ],
            "the CRTC is switched off first"
        );
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Off));
        assert_eq!(
            crtc.commits.borrow().last(),
            Some(&LinkChange::On { full: true }),
            "then modeset from scratch"
        );
        crtc.vblank.set(Some(8));
        assert_eq!(link.complete(8, &crtc), Completion::Link(LinkStep::Pending));
        assert_eq!(link.complete(9, &crtc), Completion::Link(LinkStep::Up));
        assert!(link.presentable());
    }

    #[test]
    fn a_second_missing_vblank_in_the_same_wake_fails_the_output() {
        let crtc = counting(7);
        let mut link = OutputLink::new();
        link.arm_wake_redo();
        link.service(&crtc, false);
        link.complete(7, &crtc);
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Redone));
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Off));
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Pending));
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Failed));
        assert_eq!(link.failure(), Some(&LinkFailure::NoVblank));
        assert_eq!(
            crtc.commits.borrow().len(),
            6,
            "on, probe, off, on, probe, off: then nothing"
        );
    }

    #[test]
    fn a_lit_output_whose_wake_is_not_lit_redoes_its_modeset_from_off() {
        let crtc = counting(20);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        link.complete(20, &crtc);
        assert_eq!(link.complete(21, &crtc), Completion::Link(LinkStep::Up));
        link.redo(&crtc, false);
        assert!(!link.presentable());
        assert_eq!(crtc.commits.borrow().last(), Some(&LinkChange::Off));
        assert_eq!(link.complete(22, &crtc), Completion::Link(LinkStep::Off));
        assert_eq!(
            crtc.commits.borrow().last(),
            Some(&LinkChange::On { full: true })
        );
    }

    #[test]
    fn a_resume_tries_a_failed_link_again() {
        let crtc = counting(7);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        link.complete(7, &crtc);
        assert_eq!(link.complete(7, &crtc), Completion::Link(LinkStep::Failed));
        link.complete(7, &crtc);
        link.resume(&crtc, false);
        assert_eq!(link.failure(), None);
        assert_eq!(
            crtc.commits.borrow().last(),
            Some(&LinkChange::On { full: true })
        );
    }

    #[test]
    fn a_resume_modesets_from_scratch() {
        let crtc = counting(3);
        let mut link = OutputLink::new();
        link.service(&crtc, false);
        link.complete(3, &crtc);
        link.complete(4, &crtc);
        link.resume(&crtc, false);
        assert_eq!(
            crtc.commits.borrow().last(),
            Some(&LinkChange::On { full: true })
        );
        assert!(!link.presentable());
    }
}
