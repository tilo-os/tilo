//! Page flips: the one way a framebuffer reaches an output's primary plane
//! once the CRTC is set up.
//!
//! Every flip the DRM backend makes — the readback ring's dumb buffers, the
//! direct path's GBM targets, a fullscreen client's own buffer — goes
//! through [`KmsFlip::queue`]. Where the driver accepts
//! `DRM_CLIENT_CAP_ATOMIC` and the CRTC's primary plane is found, the flip
//! is a nonblocking atomic commit of that plane with a page-flip event
//! ([`FlipApi::Atomic`]); otherwise it is a legacy `drmModePageFlip`
//! ([`FlipApi::Legacy`]). Both complete with the same `PageFlipEvent` on
//! the card's event stream, so retirement and vblank pacing do not change.
//!
//! An atomic flip may change the framebuffer's pixel format — a legacy one
//! cannot (the kernel refuses it) — so a fullscreen client's `XBGR8888`
//! buffer can follow the output's `XRGB8888` targets
//! ([`KmsFlip::changes_format`]). The commit never carries
//! `ALLOW_MODESET` (a flip that would need a modeset fails rather than
//! flicker) nor `ASYNC` (no tearing).
//!
//! Atomic is the default and needs no configuration. [`KMS_ATOMIC_ENV`]`=0`
//! keeps every flip legacy, and an atomic flip that fails for any reason but
//! a busy CRTC or lost DRM master turns the output to legacy flips for the
//! rest of its life, retrying that frame legacy: a driver that misbehaves
//! degrades to the flips Tilo used before, without a rebuild.
//!
//! An output with atomic flips also lights and blanks through nonblocking
//! atomic modesets ([`super::output_link`]). HDR binding, adaptive sync and
//! the cursor plane stay on their legacy ioctls (`scanout.rs`,
//! `output_signal.rs`).

use std::io;
use std::sync::OnceLock;

use smithay::reexports::drm as drm_re;

use drm_re::ClientCapability;
use drm_re::Device as BasicDevice;
use drm_re::control::atomic::AtomicModeReq;
use drm_re::control::{
    AtomicCommitFlags, Device as ControlDevice, PageFlipFlags, crtc, framebuffer, plane, property,
};

use super::client_scanout::{PlaneFormats, parse_in_formats};
use super::output_signal::find_property;
use super::scanout::Card;

/// Set to `0` to page-flip through the legacy `drmModePageFlip` only: the
/// flips Tilo made before atomic presentation, for an A/B measurement or a
/// driver whose atomic flips misbehave.
pub const KMS_ATOMIC_ENV: &str = "TILO_KMS_ATOMIC";

/// `DRM_PLANE_TYPE_PRIMARY`.
const PLANE_TYPE_PRIMARY: u64 = 1;

/// Whether [`KMS_ATOMIC_ENV`] keeps every flip legacy.
pub fn atomic_flips_forced_off() -> bool {
    static FORCED_OFF: OnceLock<bool> = OnceLock::new();
    *FORCED_OFF.get_or_init(|| std::env::var_os(KMS_ATOMIC_ENV).is_some_and(|value| value == "0"))
}

/// The primary-plane properties a flip writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlaneProps {
    fb_id: property::Handle,
    crtc_id: property::Handle,
    src_x: property::Handle,
    src_y: property::Handle,
    src_w: property::Handle,
    src_h: property::Handle,
    crtc_x: property::Handle,
    crtc_y: property::Handle,
    crtc_w: property::Handle,
    crtc_h: property::Handle,
}

/// The plane KMS shows a CRTC's framebuffers on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PrimaryPlane {
    pub handle: plane::Handle,
    props: PlaneProps,
    /// What it scans out, from `IN_FORMATS`.
    pub formats: PlaneFormats,
}

/// The CRTC a flip lands on, and its mode's size: the whole of which the
/// framebuffer covers, as the legacy modeset left the plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FlipTarget {
    pub crtc: crtc::Handle,
    pub size: (u32, u32),
}

/// One framebuffer on the primary plane of `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlaneCommit {
    pub target: FlipTarget,
    pub framebuffer: framebuffer::Handle,
}

/// Whether an atomic commit only asks or flips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommitMode {
    /// `TEST_ONLY`: nothing changes.
    Test,
    /// `NONBLOCK | PAGE_FLIP_EVENT`: shown at the next vblank, with the
    /// flip event a legacy flip sends.
    Flip,
}

/// The KMS calls a flip makes, so the choice between them can be tested
/// without a card.
pub(super) trait FlipDevice {
    /// Turn on `DRM_CLIENT_CAP_ATOMIC` for this file description.
    fn enable_atomic(&self) -> io::Result<()>;
    /// The primary plane showing `crtc`, with the properties a flip writes.
    fn primary_plane(&self, crtc: crtc::Handle) -> Result<PrimaryPlane, String>;
    fn commit_plane(
        &self,
        plane: &PrimaryPlane,
        commit: PlaneCommit,
        mode: CommitMode,
    ) -> io::Result<()>;
    /// `drmModePageFlip` with a page-flip event.
    fn legacy_flip(&self, crtc: crtc::Handle, framebuffer: framebuffer::Handle) -> io::Result<()>;
}

impl FlipDevice for Card {
    fn enable_atomic(&self) -> io::Result<()> {
        self.set_client_capability(ClientCapability::Atomic, true)?;
        // Atomic turns on aspect-ratio flags in modes, which the rest of the
        // backend does not expect.
        if let Err(error) = self.set_client_capability(ClientCapability::AspectRatio, false) {
            tracing::debug!(%error, "could not switch mode aspect-ratio flags back off");
        }
        Ok(())
    }

    fn primary_plane(&self, crtc: crtc::Handle) -> Result<PrimaryPlane, String> {
        let planes = self
            .plane_handles()
            .map_err(|error| format!("list planes: {error}"))?;
        let resources = self
            .resource_handles()
            .map_err(|error| format!("list CRTCs: {error}"))?;
        // The primary plane already showing `crtc`; else, for a CRTC an
        // atomic modeset lights from nothing, its own primary: drivers
        // create one primary plane per CRTC, in CRTC order, and two outputs
        // lit at once must not both pick the first free one.
        let primaries: Vec<_> = planes
            .into_iter()
            .filter_map(|handle| Some((handle, self.get_plane(handle).ok()?)))
            .filter(|(handle, _)| {
                find_property(self, *handle, "type")
                    .is_some_and(|(_, kind, _)| kind == PLANE_TYPE_PRIMARY)
            })
            .collect();
        let crtc_index = resources.crtcs().iter().position(|&handle| handle == crtc);
        let can_show = |info: &plane::Info| {
            info.crtc().is_none()
                && resources
                    .filter_crtcs(info.possible_crtcs())
                    .contains(&crtc)
        };
        let chosen = primaries
            .iter()
            .find(|(_, info)| info.crtc() == Some(crtc))
            .or_else(|| {
                crtc_index
                    .and_then(|index| primaries.get(index))
                    .filter(|(_, info)| can_show(info))
            })
            .or_else(|| primaries.iter().find(|(_, info)| can_show(info)));
        if let Some((handle, info)) = chosen {
            let handle = *handle;
            let prop = |name: &str| {
                find_property(self, handle, name)
                    .map(|(prop, _, _)| prop)
                    .ok_or_else(|| format!("the primary plane has no {name} property"))
            };
            let props = PlaneProps {
                fb_id: prop("FB_ID")?,
                crtc_id: prop("CRTC_ID")?,
                src_x: prop("SRC_X")?,
                src_y: prop("SRC_Y")?,
                src_w: prop("SRC_W")?,
                src_h: prop("SRC_H")?,
                crtc_x: prop("CRTC_X")?,
                crtc_y: prop("CRTC_Y")?,
                crtc_w: prop("CRTC_W")?,
                crtc_h: prop("CRTC_H")?,
            };
            let formats = find_property(self, handle, "IN_FORMATS")
                .and_then(|(_, blob, _)| self.get_property_blob(blob).ok())
                .and_then(|blob| parse_in_formats(&blob))
                .map_or_else(
                    || PlaneFormats::Implicit(info.formats().to_vec()),
                    PlaneFormats::Modifiers,
                );
            return Ok(PrimaryPlane {
                handle,
                props,
                formats,
            });
        }
        Err("no primary plane can show this CRTC".to_owned())
    }

    fn commit_plane(
        &self,
        plane: &PrimaryPlane,
        commit: PlaneCommit,
        mode: CommitMode,
    ) -> io::Result<()> {
        let flags = match mode {
            CommitMode::Test => AtomicCommitFlags::TEST_ONLY,
            CommitMode::Flip => AtomicCommitFlags::PAGE_FLIP_EVENT | AtomicCommitFlags::NONBLOCK,
        };
        self.atomic_commit(flags, plane_request(plane, commit))
    }

    fn legacy_flip(&self, crtc: crtc::Handle, framebuffer: framebuffer::Handle) -> io::Result<()> {
        self.page_flip(crtc, framebuffer, PageFlipFlags::EVENT, None)
    }
}

/// A 16.16 fixed-point plane source coordinate. A `u32` shifted by 16 fits
/// a `u64` with room to spare.
fn fixed_16_16(value: u32) -> u64 {
    u64::from(value).wrapping_shl(16)
}

/// The atomic request that shows `commit` on `plane`: the framebuffer on the
/// plane, the plane on the CRTC, covering the whole mode from the
/// framebuffer's origin — the state the legacy modeset and flips leave it
/// in, so only the framebuffer (and with it, possibly, the format) changes.
pub(super) fn plane_request(plane: &PrimaryPlane, commit: PlaneCommit) -> AtomicModeReq {
    use property::Value;
    let PlaneCommit {
        target: FlipTarget {
            crtc,
            size: (width, height),
        },
        framebuffer,
    } = commit;
    let props = plane.props;
    let mut request = AtomicModeReq::new();
    for (prop, value) in [
        (props.fb_id, Value::Framebuffer(Some(framebuffer))),
        (props.crtc_id, Value::CRTC(Some(crtc))),
        (props.src_x, Value::UnsignedRange(0)),
        (props.src_y, Value::UnsignedRange(0)),
        (props.src_w, Value::UnsignedRange(fixed_16_16(width))),
        (props.src_h, Value::UnsignedRange(fixed_16_16(height))),
        (props.crtc_x, Value::SignedRange(0)),
        (props.crtc_y, Value::SignedRange(0)),
        (props.crtc_w, Value::UnsignedRange(u64::from(width))),
        (props.crtc_h, Value::UnsignedRange(u64::from(height))),
    ] {
        request.add_property(plane.handle, prop, value);
    }
    request
}

/// Find the primary plane on `crtc`, enabling atomic modesetting so that
/// framebuffers can be committed or tried on it.
pub(super) fn probe_primary_plane(
    device: &impl FlipDevice,
    crtc: crtc::Handle,
) -> Result<PrimaryPlane, String> {
    device
        .enable_atomic()
        .map_err(|error| format!("atomic modesetting is unavailable: {error}"))?;
    device.primary_plane(crtc)
}

/// Ask KMS, without changing anything, whether `plane` takes `commit` — the
/// very request a flip of it would make.
pub(super) fn test_on_plane(
    device: &impl FlipDevice,
    plane: &PrimaryPlane,
    commit: PlaneCommit,
) -> Result<(), String> {
    device
        .commit_plane(plane, commit, CommitMode::Test)
        .map_err(|error| format!("the primary plane refused the buffer in a test commit: {error}"))
}

/// A flip that may succeed on the next frame: the CRTC still has one
/// queued.
fn transient(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EBUSY | libc::EAGAIN | libc::EINTR)
    )
}

/// The session lost DRM master (a VT switch): every KMS call fails until it
/// is back, atomic or not, so it says nothing about atomic flips.
fn without_master(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EACCES | libc::EPERM))
}

/// How one output's flips reach KMS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FlipApi {
    /// Nonblocking atomic commits of the primary plane.
    Atomic(PrimaryPlane),
    /// `drmModePageFlip`, and why not atomic.
    Legacy(String),
}

/// One output's page flips ([`Self::queue`]).
#[derive(Debug)]
pub(super) struct KmsFlip {
    api: FlipApi,
    target: FlipTarget,
}

impl KmsFlip {
    /// Choose how `target`'s flips reach KMS: atomic when the driver takes
    /// the atomic cap and the primary plane is found, legacy when not or
    /// when `legacy` names a reason to stay legacy ([`KMS_ATOMIC_ENV`], a
    /// connector that does not flip at all) — then the card is not asked
    /// anything.
    pub fn probe(device: &impl FlipDevice, target: FlipTarget, legacy: Option<String>) -> Self {
        let api = match legacy {
            Some(reason) => FlipApi::Legacy(reason),
            None => match probe_primary_plane(device, target.crtc) {
                Ok(plane) => FlipApi::Atomic(plane),
                Err(reason) => FlipApi::Legacy(reason),
            },
        };
        Self { api, target }
    }

    #[cfg(test)]
    pub fn api(&self) -> &FlipApi {
        &self.api
    }

    pub fn plane(&self) -> Option<&PrimaryPlane> {
        match &self.api {
            FlipApi::Atomic(plane) => Some(plane),
            FlipApi::Legacy(_) => None,
        }
    }

    pub fn target(&self) -> FlipTarget {
        self.target
    }

    /// Whether a flip may show a framebuffer of another format than the one
    /// shown: atomic flips can, legacy flips cannot.
    pub fn changes_format(&self) -> bool {
        matches!(self.api, FlipApi::Atomic(_))
    }

    /// One line in the session log naming how `connector`'s flips go.
    pub fn log(&self, connector: u32) {
        match &self.api {
            FlipApi::Atomic(_) => {
                eprintln!(
                    "tilo compositor: DRM scanout: kms_flip_api=atomic connector={connector}"
                );
            }
            FlipApi::Legacy(reason) => {
                eprintln!(
                    "tilo compositor: DRM scanout: kms_flip_api=legacy reason={reason:?} connector={connector}"
                );
            }
        }
    }

    /// Queue a flip onto `framebuffer`, shown at the next vblank with a
    /// page-flip event. The only way a framebuffer reaches the plane after
    /// the modeset.
    ///
    /// `EBUSY`/`EAGAIN` (a flip is still queued) and a lost DRM master are
    /// returned as they are. Any other atomic failure turns this output to
    /// legacy flips for good and retries this flip legacy.
    pub fn queue(
        &mut self,
        device: &impl FlipDevice,
        framebuffer: framebuffer::Handle,
    ) -> io::Result<()> {
        if let FlipApi::Atomic(plane) = &self.api {
            let commit = PlaneCommit {
                target: self.target,
                framebuffer,
            };
            match device.commit_plane(plane, commit, CommitMode::Flip) {
                Ok(()) => return Ok(()),
                Err(error) if transient(&error) || without_master(&error) => return Err(error),
                Err(error) => {
                    let reason = format!("an atomic flip failed: {error}");
                    eprintln!(
                        "tilo compositor: DRM scanout: kms_flip_api=legacy reason={reason:?} crtc={} DRM page flips fall back to legacy",
                        u32::from(self.target.crtc)
                    );
                    self.api = FlipApi::Legacy(reason);
                }
            }
        }
        device.legacy_flip(self.target.crtc, framebuffer)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use drm_re::buffer::DrmFourcc;

    use super::*;

    const XRGB: u32 = DrmFourcc::Xrgb8888 as u32;
    const XBGR: u32 = DrmFourcc::Xbgr8888 as u32;

    fn handle<T: From<drm_re::control::RawResourceHandle>>(id: u32) -> T {
        T::from(drm_re::control::RawResourceHandle::new(id).expect("a non-zero id"))
    }

    fn crtc() -> crtc::Handle {
        handle(40)
    }

    fn fb(id: u32) -> framebuffer::Handle {
        handle(id)
    }

    fn target() -> FlipTarget {
        FlipTarget {
            crtc: crtc(),
            size: (1920, 1080),
        }
    }

    fn primary() -> PrimaryPlane {
        let prop = |id| -> property::Handle { handle(id) };
        PrimaryPlane {
            handle: handle(31),
            props: PlaneProps {
                fb_id: prop(1),
                crtc_id: prop(2),
                src_x: prop(3),
                src_y: prop(4),
                src_w: prop(5),
                src_h: prop(6),
                crtc_x: prop(7),
                crtc_y: prop(8),
                crtc_w: prop(9),
                crtc_h: prop(10),
            },
            formats: PlaneFormats::Implicit(vec![XRGB, XBGR]),
        }
    }

    /// What reached the mock card.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        EnableAtomic,
        Atomic(CommitMode, framebuffer::Handle),
        Legacy(framebuffer::Handle),
    }

    /// A card that keeps the kernel's rules for the calls a flip makes: a
    /// legacy flip refuses a framebuffer of another format than the one
    /// shown (`EINVAL`); an atomic commit takes any format the plane lists.
    /// Every accepted flip retires at once.
    struct MockCard {
        atomic: Result<(), i32>,
        plane: Result<PrimaryPlane, String>,
        /// Fails the next atomic flip with this errno.
        atomic_flip_error: Cell<Option<i32>>,
        formats: HashMap<framebuffer::Handle, u32>,
        shown: Cell<u32>,
        calls: RefCell<Vec<Call>>,
    }

    impl MockCard {
        fn new() -> Self {
            Self {
                atomic: Ok(()),
                plane: Ok(primary()),
                atomic_flip_error: Cell::new(None),
                formats: HashMap::from([(fb(1), XRGB), (fb(2), XRGB), (fb(9), XBGR)]),
                shown: Cell::new(XRGB),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }

        fn format(&self, framebuffer: framebuffer::Handle) -> io::Result<u32> {
            self.formats
                .get(&framebuffer)
                .copied()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
        }
    }

    impl FlipDevice for MockCard {
        fn enable_atomic(&self) -> io::Result<()> {
            self.calls.borrow_mut().push(Call::EnableAtomic);
            self.atomic.map_err(io::Error::from_raw_os_error)
        }

        fn primary_plane(&self, crtc: crtc::Handle) -> Result<PrimaryPlane, String> {
            if crtc != super::tests::crtc() {
                return Err("asked about another CRTC".to_owned());
            }
            self.plane.clone()
        }

        fn commit_plane(
            &self,
            plane: &PrimaryPlane,
            commit: PlaneCommit,
            mode: CommitMode,
        ) -> io::Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::Atomic(mode, commit.framebuffer));
            // The whole mode, on this CRTC: anything else is refused, and
            // the tests expecting success fail.
            if commit.target != target() {
                return Err(io::Error::from_raw_os_error(libc::ERANGE));
            }
            if mode == CommitMode::Flip
                && let Some(errno) = self.atomic_flip_error.take()
            {
                return Err(io::Error::from_raw_os_error(errno));
            }
            let format = self.format(commit.framebuffer)?;
            if !plane.formats.supports(format, 0) {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            if mode == CommitMode::Flip {
                self.shown.set(format);
            }
            Ok(())
        }

        fn legacy_flip(
            &self,
            crtc: crtc::Handle,
            framebuffer: framebuffer::Handle,
        ) -> io::Result<()> {
            self.calls.borrow_mut().push(Call::Legacy(framebuffer));
            if crtc != super::tests::crtc() {
                return Err(io::Error::from_raw_os_error(libc::ERANGE));
            }
            let format = self.format(framebuffer)?;
            if format != self.shown.get() {
                // drm_mode_page_flip_ioctl: "Page flip is not allowed to
                // change frame buffer format."
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            Ok(())
        }
    }

    #[test]
    fn atomic_is_chosen_when_the_driver_accepts_the_cap() {
        let card = MockCard::new();
        let mut flip = KmsFlip::probe(&card, target(), None);
        assert_eq!(flip.api(), &FlipApi::Atomic(primary()));
        assert!(flip.changes_format());
        assert_eq!(flip.plane(), Some(&primary()));

        flip.queue(&card, fb(1)).expect("flipped");
        assert_eq!(
            card.calls(),
            [Call::EnableAtomic, Call::Atomic(CommitMode::Flip, fb(1))],
            "an atomic commit, never drmModePageFlip"
        );
    }

    #[test]
    fn legacy_when_the_cap_is_refused_or_forced_off() {
        // Phase `cap_refused`: an old driver.
        let mut card = MockCard::new();
        card.atomic = Err(libc::EOPNOTSUPP);
        let mut flip = KmsFlip::probe(&card, target(), None);
        assert!(
            matches!(flip.api(), FlipApi::Legacy(reason) if reason.starts_with("atomic modesetting is unavailable"))
        );
        assert!(!flip.changes_format());
        assert_eq!(flip.plane(), None);
        flip.queue(&card, fb(1)).expect("flipped");
        assert_eq!(card.calls(), [Call::EnableAtomic, Call::Legacy(fb(1))]);

        let mut card = MockCard::new();
        card.plane = Err("no primary plane shows this CRTC".to_owned());
        let mut flip = KmsFlip::probe(&card, target(), None);
        assert_eq!(
            flip.api(),
            &FlipApi::Legacy("no primary plane shows this CRTC".to_owned())
        );
        flip.queue(&card, fb(1)).expect("flipped");
        assert_eq!(card.calls(), [Call::EnableAtomic, Call::Legacy(fb(1))]);

        // Phase `forced_off`: TILO_KMS_ATOMIC=0 does not touch the cap.
        let card = MockCard::new();
        let reason = format!("forced by {KMS_ATOMIC_ENV}=0");
        let mut flip = KmsFlip::probe(&card, target(), Some(reason.clone()));
        assert_eq!(flip.api(), &FlipApi::Legacy(reason));
        flip.queue(&card, fb(1)).expect("flipped");
        assert_eq!(card.calls(), [Call::Legacy(fb(1))]);
    }

    #[test]
    fn a_failed_atomic_flip_falls_back_to_legacy_for_good() {
        let card = MockCard::new();
        let mut flip = KmsFlip::probe(&card, target(), None);
        card.atomic_flip_error.set(Some(libc::EINVAL));
        flip.queue(&card, fb(1))
            .expect("the same frame is flipped legacy");
        assert!(
            matches!(flip.api(), FlipApi::Legacy(reason) if reason.starts_with("an atomic flip failed"))
        );
        assert!(!flip.changes_format());
        flip.queue(&card, fb(2)).expect("flipped");
        assert_eq!(
            card.calls(),
            [
                Call::EnableAtomic,
                Call::Atomic(CommitMode::Flip, fb(1)),
                Call::Legacy(fb(1)),
                Call::Legacy(fb(2)),
            ]
        );
    }

    #[test]
    fn a_busy_atomic_flip_stays_atomic() {
        for errno in [libc::EBUSY, libc::EAGAIN, libc::EACCES] {
            let card = MockCard::new();
            let mut flip = KmsFlip::probe(&card, target(), None);
            card.atomic_flip_error.set(Some(errno));
            let error = flip.queue(&card, fb(1)).expect_err("refused");
            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(
                flip.changes_format(),
                "errno {errno} is no reason to give atomic up"
            );
            assert_eq!(
                card.calls(),
                [Call::EnableAtomic, Call::Atomic(CommitMode::Flip, fb(1))],
                "and is not retried legacy on the same frame"
            );
        }
    }

    #[test]
    fn only_atomic_flips_change_format() {
        // An XRGB target, a fullscreen client's XBGR buffer, the next XRGB
        // target when chrome appears over it.
        let card = MockCard::new();
        let mut flip = KmsFlip::probe(&card, target(), None);
        for framebuffer in [fb(1), fb(9), fb(2)] {
            flip.queue(&card, framebuffer)
                .expect("atomic flips change format");
        }
        assert!(flip.changes_format(), "no flip failed");

        let card = MockCard::new();
        let mut flip = KmsFlip::probe(&card, target(), Some("forced".to_owned()));
        flip.queue(&card, fb(1)).expect("same format");
        let error = flip
            .queue(&card, fb(9))
            .expect_err("a legacy flip keeps the format");
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn a_test_commit_asks_with_the_request_a_flip_makes() {
        let card = MockCard::new();
        let plane = primary();
        let commit = PlaneCommit {
            target: target(),
            framebuffer: fb(9),
        };
        test_on_plane(&card, &plane, commit).expect("the plane lists XBGR");
        assert_eq!(card.calls(), [Call::Atomic(CommitMode::Test, fb(9))]);
        assert_eq!(card.shown.get(), XRGB, "a test changes nothing");

        let only_xrgb = PrimaryPlane {
            formats: PlaneFormats::Implicit(vec![XRGB]),
            ..primary()
        };
        assert!(
            test_on_plane(&card, &only_xrgb, commit)
                .is_err_and(|reason| reason.starts_with("the primary plane refused"))
        );
    }

    #[test]
    fn the_flip_request_covers_the_whole_mode() {
        let plane = primary();
        let request = plane_request(
            &plane,
            PlaneCommit {
                target: target(),
                framebuffer: fb(9),
            },
        );
        let mut expected = AtomicModeReq::new();
        let props = plane.props;
        for (prop, value) in [
            (props.fb_id, 9_u64),
            (props.crtc_id, 40),
            (props.src_x, 0),
            (props.src_y, 0),
            (props.src_w, 125_829_120),
            (props.src_h, 70_778_880),
            (props.crtc_x, 0),
            (props.crtc_y, 0),
            (props.crtc_w, 1920),
            (props.crtc_h, 1080),
        ] {
            expected.add_raw_property(plane.handle.into(), prop, value);
        }
        assert_eq!(format!("{request:?}"), format!("{expected:?}"));
    }

    /// Every flip in the shell goes through [`KmsFlip::queue`]: no other
    /// source calls `drmModePageFlip` or commits atomically. A second path
    /// would be one the present gate and the legacy fallback do not know.
    #[test]
    fn every_kms_flip_goes_through_kms_flip() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("readable source tree") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&root, &mut files);
        assert!(files.len() > 100, "the shell's sources were found");
        let this = root.join("compositor/backend/drm/kms_flip.rs");
        // Lighting and blanking an output are atomic commits too, but
        // modesets, not flips: their one home is the output's link.
        let modesets = root.join("compositor/backend/drm/output_link.rs");
        let mut found_this = false;
        for file in files {
            let text = std::fs::read_to_string(&file).expect("readable source");
            let calls = |text: &str| {
                (
                    text.matches("page_flip(").count(),
                    text.matches("atomic_commit(").count(),
                )
            };
            if file == this {
                found_this = true;
                let production = text
                    .split("#[cfg(test)]\nmod tests")
                    .next()
                    .unwrap_or_default();
                assert_eq!(
                    calls(production),
                    (1, 1),
                    "one of each, in FlipDevice for Card"
                );
            } else if file == modesets {
                let production = text
                    .split("#[cfg(test)]\nmod tests")
                    .next()
                    .unwrap_or_default();
                assert_eq!(
                    calls(production),
                    (0, 2),
                    "the modeset commit and its test commit, in LinkTarget and AtomicLink"
                );
            } else {
                assert_eq!(
                    calls(&text),
                    (0, 0),
                    "{} flips without KmsFlip",
                    file.display()
                );
            }
        }
        assert!(found_this, "kms_flip.rs is where the flip lives");
    }
}
