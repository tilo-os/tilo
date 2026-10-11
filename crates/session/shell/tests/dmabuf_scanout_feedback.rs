//! A fullscreen client is told what the display can scan out, and told
//! again when it no longer could.
//!
//! linux-dmabuf v4 lets a compositor send each surface its own feedback. A
//! surface starts on the default — the render node and the formats the
//! compositor can composite. While it is the frame's scanout candidate
//! (fullscreen, alone on top of an output, pixel for pixel), its buffer is
//! opaque and the output's primary plane takes client buffers, its feedback
//! gains a tranche flagged `scanout` on the plane's device, listing what the
//! plane scans out that the compositor can still composite; clients then
//! allocate scanout-capable buffers. The frame anything covers it, it turns
//! translucent, the output has no plane, or it leaves fullscreen, it is sent
//! the default again.
//!
//! A real client binds `zwp_linux_dmabuf_v1` v4 over a socket pair and asks
//! for its surface's feedback; the compositor's own `FrameBuilder` decides
//! the candidate and `State::update_scanout_feedback` is handed what the
//! native loop hands it after every frame
//! (`compositor::scanout_feedback::offer`). The plane is the one thing
//! supplied by the test, standing in for the DRM presenter's report: this
//! machine has no KMS plane to probe, and without one every surface keeps
//! the default — a phase below checks exactly that.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Instant;

use shell::compositor::backend::{Output, OutputId};
use shell::compositor::foreign::store;
use shell::compositor::scanout_feedback::{self, ScanoutFeedback, ScanoutPlane};
use shell::shell::frame::FrameBuilder;
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_region, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const WIDTH: i32 = 64;
const HEIGHT: i32 = 48;
const STRIDE: u32 = WIDTH.unsigned_abs() * 4;
const BUFFER_LEN: u32 = STRIDE * HEIGHT.unsigned_abs();
const APP_ID: &str = "org.tilo.test.dmabuf-scanout-feedback";
/// The KMS device the stand-in plane is on: `makedev(226, 1)`.
const SCANOUT_DEVICE: u64 = (226 << 8) | 1;
/// An Intel tiled modifier: a plane takes it, the compositor's importer
/// does not, so it must never be offered.
const TILED: u64 = 0x0100_0000_0000_0001;
/// The on-screen keyboard's surface, published directly.
const KEYBOARD: u64 = 9_907_731;

/// One tranche of a feedback batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Tranche {
    device: u64,
    scanout: bool,
    formats: Vec<(u32, u64)>,
}

/// One `zwp_linux_dmabuf_feedback_v1` batch, up to its `done`.
#[derive(Clone, Debug, Default)]
struct Batch {
    main_device: u64,
    tranches: Vec<Tranche>,
}

impl Batch {
    fn scanout_tranche(&self) -> Option<&Tranche> {
        self.tranches.iter().find(|tranche| tranche.scanout)
    }
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configured: bool,
    /// The format table, `(fourcc, modifier)` per index.
    table: Vec<(u32, u64)>,
    batch: Batch,
    tranche: Tranche,
    /// Every batch received, in order.
    batches: Vec<Batch>,
}

crate::support::wayland::test_client!(Client: registry, ping);

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

fn dev_t(bytes: &[u8]) -> u64 {
    let mut buf = [0_u8; 8];
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_ne_bytes(buf)
}

impl Dispatch<zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_linux_dmabuf_feedback_v1::{Event, TrancheFlags};
        match event {
            Event::MainDevice { device } => state.batch.main_device = dev_t(&device),
            Event::FormatTable { fd, size } => {
                // A sealed memfd of 16-byte `{u32 format, u32 pad, u64 modifier}`.
                let len = size as usize;
                // SAFETY: a read-only private mapping of the table the
                // compositor sent, unmapped below.
                let map = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE,
                        fd.as_raw_fd(),
                        0,
                    )
                };
                assert_ne!(map, libc::MAP_FAILED, "the format table maps");
                // SAFETY: `map` is `len` readable bytes until the munmap.
                let bytes = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), len) };
                state.table = bytes
                    .chunks_exact(16)
                    .map(|record| {
                        (
                            u32::from_ne_bytes(record[0..4].try_into().unwrap()),
                            u64::from_ne_bytes(record[8..16].try_into().unwrap()),
                        )
                    })
                    .collect();
                // SAFETY: unmapping the mapping made above.
                unsafe {
                    libc::munmap(map, len);
                }
            }
            Event::TrancheTargetDevice { device } => state.tranche.device = dev_t(&device),
            Event::TrancheFlags { flags } => {
                state.tranche.scanout =
                    matches!(flags, WEnum::Value(flags) if flags.contains(TrancheFlags::Scanout));
            }
            Event::TrancheFormats { indices } => {
                for index in indices.chunks_exact(2) {
                    let index = usize::from(u16::from_ne_bytes([index[0], index[1]]));
                    state.tranche.formats.push(state.table[index]);
                }
            }
            Event::TrancheDone => {
                let tranche = std::mem::take(&mut state.tranche);
                state.batch.tranches.push(tranche);
            }
            Event::Done => {
                let batch = std::mem::take(&mut state.batch);
                state.batches.push(batch);
            }
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_region::WlRegion);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1);
delegate_noop!(Client: ignore zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1);

type Harness = crate::support::wayland::Harness<Client>;

/// A linear single-plane buffer of opaque grey, as a memfd the compositor's
/// CPU importer reads like a linear PRIME buffer.
fn grey_memfd() -> OwnedFd {
    use std::io::Write as _;
    // SAFETY: memfd_create with a static nul-terminated name and MFD_CLOEXEC.
    let raw = unsafe { libc::memfd_create(c"dmabuf-scanout-feedback".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: memfd_create returned a new descriptor that nothing else owns.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(&[0x40, 0x40, 0x40, 0xFF].repeat((BUFFER_LEN / 4) as usize))
        .expect("fill memfd");
    OwnedFd::from(file)
}

/// What a commit does to the surface's opaque region.
#[derive(Clone, Copy)]
enum Opaque {
    /// Covers the whole surface.
    Whole,
    /// None: the whole surface may be translucent.
    None,
}

/// The DRM presenter's report for an output whose primary plane scans out
/// `XRGB8888` linear and tiled, and `ARGB8888` linear.
fn stand_in_plane() -> Arc<ScanoutPlane> {
    let format = |code, modifier| Format { code, modifier };
    Arc::new(ScanoutPlane {
        device: SCANOUT_DEVICE,
        formats: vec![
            format(Fourcc::Xrgb8888, Modifier::Linear),
            format(Fourcc::Argb8888, Modifier::Linear),
            format(Fourcc::Xrgb8888, Modifier::from(TILED)),
        ],
    })
}

/// A fullscreen client with surface feedback, and the frame builder that
/// decides each frame's scanout candidate.
struct Fixture {
    h: Harness,
    dmabuf: zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    compositor: wl_compositor::WlCompositor,
    surface: wl_surface::WlSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    surface_id: u64,
    memfd: OwnedFd,
    frames: FrameBuilder,
    outputs: Vec<Output>,
    plane: Arc<ScanoutPlane>,
    /// The formats the default feedback offers.
    importable: Vec<(u32, u64)>,
}

impl Fixture {
    fn new() -> Self {
        let mut h = Harness::new();
        // A machine without a render node advertises no dmabuf global; the
        // feedback under test does not depend on which GPU the client
        // allocates on, so one is created here as `State::init_dmabuf` would.
        if h.state.dmabuf_global.is_none() {
            let formats: Vec<Format> = [Fourcc::Argb8888, Fourcc::Xrgb8888]
                .into_iter()
                .map(|code| Format {
                    code,
                    modifier: Modifier::Linear,
                })
                .collect();
            let feedback = DmabufFeedbackBuilder::new(0, formats.iter().copied())
                .build()
                .expect("dmabuf feedback");
            let display = h.display.handle();
            let global = h
                .state
                .dmabuf_state
                .create_global_with_default_feedback::<shell::compositor::state::State>(
                    &display, &feedback,
                );
            h.state.dmabuf_global = Some(global);
            h.state.scanout_feedback = Some(ScanoutFeedback::new(0, formats, feedback));
            h.pump();
        }
        assert!(
            h.state.scanout_feedback.is_some(),
            "a dmabuf global always comes with its feedback"
        );
        let dmabuf = h.bind::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>("zwp_linux_dmabuf_v1", 4);
        assert_eq!(
            wayland_client::Proxy::version(&dmabuf),
            4,
            "surface feedback needs version 4"
        );
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm_base = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        h.pump();

        let qh = h.queue.handle();
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id(APP_ID.into());
        let feedback = dmabuf.get_surface_feedback(&surface, &qh, ());
        surface.commit();
        h.pump();
        let surface_id = h.foreign_id(APP_ID);
        store().note_placement(surface_id, Rect::new(0.0, 0.0, 640.0, 480.0));
        shell::compositor::foreign::flush_configures(&mut h.state);
        h.pump();
        assert!(
            h.client.configured,
            "the toplevel was never configured, so a buffer commit would be a protocol error rather than the path under test"
        );
        std::mem::forget((xdg_surface, wm_base, feedback));

        let size = (WIDTH.unsigned_abs(), HEIGHT.unsigned_abs());
        let mut frames = FrameBuilder::new(size);
        frames.set_fullscreen_surface(Some((
            surface_id,
            "Fullscreen".to_owned(),
            Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1)),
        )));
        let first = h
            .client
            .batches
            .first()
            .cloned()
            .expect("the surface's first feedback");
        let importable = first
            .tranches
            .iter()
            .flat_map(|tranche| tranche.formats.iter().copied())
            .collect();
        Self {
            h,
            dmabuf,
            compositor,
            surface,
            toplevel,
            surface_id,
            memfd: grey_memfd(),
            frames,
            outputs: vec![Output::new(OutputId(0), size, 1.0)],
            plane: stand_in_plane(),
            importable,
        }
    }

    /// Commit a fresh `fourcc` buffer with `opaque` as its opaque region and
    /// wait until the compositor captured it.
    fn commit(&mut self, fourcc: Fourcc, opaque: Opaque) {
        let before = store().frame_seq(self.surface_id);
        let qh = self.h.queue.handle();
        let params = self.dmabuf.create_params(&qh, ());
        params.add(self.memfd.as_fd(), 0, 0, STRIDE, 0, 0);
        let buffer = params.create_immed(
            WIDTH,
            HEIGHT,
            fourcc as u32,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &qh,
            (),
        );
        match opaque {
            Opaque::Whole => {
                let region = self.compositor.create_region(&qh, ());
                region.add(0, 0, WIDTH, HEIGHT);
                self.surface.set_opaque_region(Some(&region));
                region.destroy();
            }
            Opaque::None => self.surface.set_opaque_region(None),
        }
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, WIDTH, HEIGHT);
        self.surface.commit();
        std::mem::forget((buffer, params));

        let display = self.h.display.handle();
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("the hang bound fits in an Instant");
        loop {
            self.h.pump();
            self.h.state.pump_foreign_capture_barriers(&display);
            if store().frame_seq(self.surface_id) != before {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the {fourcc:?} commit was never captured"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Build a frame and hand the compositor its offer as the native loop
    /// does, with `plane` standing in for the backend's
    /// `Backend::scanout_plane`; returns whether the frame had a candidate.
    fn frame(&mut self, plane: Option<&Arc<ScanoutPlane>>) -> bool {
        let frame = self.frames.paint_with_damage();
        let candidate = frame.scanout_candidate();
        if let Some(candidate) = candidate {
            assert_eq!(
                candidate.surface, self.surface_id,
                "the fullscreen client's"
            );
        }
        let had_candidate = candidate.is_some();
        let offer = scanout_feedback::offer(candidate, &self.outputs, |_| plane.cloned());
        self.h.state.update_scanout_feedback(offer);
        self.h.pump();
        had_candidate
    }

    fn batches(&self) -> usize {
        self.h.client.batches.len()
    }

    fn latest(&self) -> &Batch {
        self.h.client.batches.last().expect("a feedback batch")
    }

    /// The latest batch is the default: no scanout tranche, every
    /// importable format on the main device.
    fn assert_default(&self, what: &str) {
        let batch = self.latest();
        assert_eq!(batch.scanout_tranche(), None, "{what}: no scanout tranche");
        let mut offered: Vec<(u32, u64)> = batch
            .tranches
            .iter()
            .flat_map(|tranche| tranche.formats.iter().copied())
            .collect();
        offered.sort_unstable();
        let mut importable = self.importable.clone();
        importable.sort_unstable();
        assert_eq!(offered, importable, "{what}: the default formats");
    }

    /// The latest batch carries the plane's scanout tranche before the
    /// default one.
    fn assert_scanout(&self, what: &str) {
        let batch = self.latest();
        let tranche = batch
            .scanout_tranche()
            .unwrap_or_else(|| panic!("{what}: a scanout tranche, got {batch:?}"));
        assert_eq!(
            batch.tranches.first(),
            Some(tranche),
            "{what}: the scanout tranche is preferred"
        );
        assert_eq!(
            tranche.device, SCANOUT_DEVICE,
            "{what}: on the plane's device"
        );
        let mut formats = tranche.formats.clone();
        formats.sort_unstable();
        let mut expected = vec![(Fourcc::Argb8888 as u32, 0), (Fourcc::Xrgb8888 as u32, 0)];
        expected.sort_unstable();
        assert_eq!(
            formats, expected,
            "{what}: what the plane scans out that the compositor composites — never the tiled modifier"
        );
        let main = batch
            .tranches
            .iter()
            .find(|other| !other.scanout)
            .unwrap_or_else(|| panic!("{what}: the default tranche stays"));
        assert_eq!(main.device, batch.main_device, "{what}: the main tranche");
        let mut defaults = main.formats.clone();
        defaults.sort_unstable();
        let mut importable = self.importable.clone();
        importable.sort_unstable();
        assert_eq!(
            defaults, importable,
            "{what}: every default format stays offered"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario walks one client through every transition of its feedback"
)]
fn a_fullscreen_opaque_client_gets_a_scanout_tranche_until_it_could_not_be_scanned_out() {
    let mut fixture = Fixture::new();
    let plane = Arc::clone(&fixture.plane);

    // Phase `default_first`: a surface that is not fullscreen gets the
    // default feedback.
    assert_eq!(fixture.batches(), 1, "one batch when the surface asked");
    fixture.assert_default("a windowed surface");

    // Phase `fullscreen_but_translucent`: an ARGB buffer without an opaque
    // region would be refused by the presenter, so it is not offered the
    // plane.
    fixture.toplevel.set_fullscreen(None);
    fixture.h.pump();
    fixture.commit(Fourcc::Argb8888, Opaque::None);
    assert!(fixture.frame(Some(&plane)), "the client is the candidate");
    assert_eq!(fixture.batches(), 1, "nothing to tell a translucent client");

    // Phase `fullscreen_and_opaque`: the same buffer declared opaque.
    fixture.commit(Fourcc::Argb8888, Opaque::Whole);
    assert!(fixture.frame(Some(&plane)));
    assert_eq!(fixture.batches(), 2, "the client is told");
    fixture.assert_scanout("a fullscreen, opaque client");

    // Phase `unchanged_frames_resend_nothing`.
    assert!(fixture.frame(Some(&plane)));
    assert_eq!(fixture.batches(), 2, "the same offer is not resent");

    // Phase `chrome_over_it`: the on-screen keyboard rises over the client.
    store().publish_rgba(KEYBOARD, &[0, 0, 0, 255], 1, 1);
    fixture
        .frames
        .set_osk_surface(Some(KEYBOARD), None, None, true);
    assert!(!fixture.frame(Some(&plane)), "chrome voids the candidate");
    assert_eq!(fixture.batches(), 3);
    fixture.assert_default("chrome over the client");

    // Phase `chrome_gone`.
    fixture
        .frames
        .set_osk_surface(Some(KEYBOARD), None, None, false);
    assert!(fixture.frame(Some(&plane)));
    assert_eq!(fixture.batches(), 4);
    fixture.assert_scanout("the keyboard down again");

    // Phase `a_popup_over_it`.
    let popup = 9_907_732;
    store().publish_rgba(popup, &[0, 0, 0, 255], 1, 1);
    fixture
        .frames
        .set_transient_surfaces(&[(popup, Rect::new(4.0, 4.0, 20.0, 20.0))]);
    assert!(!fixture.frame(Some(&plane)));
    fixture.assert_default("a popup over the client");
    fixture.frames.set_transient_surfaces(&[]);
    assert!(fixture.frame(Some(&plane)));
    fixture.assert_scanout("the popup closed");

    // Phase `turned_translucent`: its next buffer drops the region.
    fixture.commit(Fourcc::Argb8888, Opaque::None);
    assert!(fixture.frame(Some(&plane)));
    fixture.assert_default("a translucent buffer");

    // Phase `xrgb_is_opaque_by_format`.
    fixture.commit(Fourcc::Xrgb8888, Opaque::None);
    assert!(fixture.frame(Some(&plane)));
    fixture.assert_scanout("an XRGB buffer");

    // Phase `no_scanout_plane`: headless, lavapipe, a rotated or calibrated
    // output — the backend reports no plane, and the client falls back.
    let before = fixture.batches();
    assert!(fixture.frame(None), "still the candidate");
    assert_eq!(fixture.batches(), before + 1);
    fixture.assert_default("an output without a scanout plane");

    // Phase `a_plane_that_scans_out_nothing_importable`: only a tiled
    // modifier — offering it would hand the client a buffer the compositor
    // cannot draw once anything covers it.
    let tiled_only = Arc::new(ScanoutPlane {
        device: SCANOUT_DEVICE,
        formats: vec![Format {
            code: Fourcc::Xrgb8888,
            modifier: Modifier::from(TILED),
        }],
    });
    let before = fixture.batches();
    assert!(fixture.frame(Some(&tiled_only)));
    assert_eq!(
        fixture.batches(),
        before,
        "still the default, nothing resent"
    );
    assert!(fixture.frame(Some(&plane)));
    fixture.assert_scanout("a plane with linear formats again");

    // Phase `left_fullscreen`: the frame still draws the surface on top (the
    // shell leaves fullscreen on its next pass), but it is no longer the
    // fullscreen client, so it is not offered the plane.
    fixture.toplevel.unset_fullscreen();
    fixture.h.pump();
    assert!(fixture.frame(Some(&plane)));
    fixture.assert_default("out of fullscreen");

    store().forget(KEYBOARD);
    store().forget(popup);
    store().forget(fixture.surface_id);
}
