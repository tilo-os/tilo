//! An `ARGB8888` client that declares its whole surface opaque is drawn —
//! and may be scanned out — as `XRGB8888`; one that does not keeps its alpha.
//!
//! Browsers and GPU toolkits submit `ARGB8888` buffers even when every pixel
//! is opaque, and say so with `wl_surface.set_opaque_region`. The compositor
//! reads that region in the commit's pre-commit hook
//! (`State::prepare_pending_foreign_capture` →
//! `compositor::opaque_region::covers`) and hands the verdict to the capture
//! with the buffer (`DmabufCommit::opaque`). On the GPU route it becomes
//! `GpuFrame::opaque`, which the DRM presenter turns into an `XRGB8888`
//! framebuffer for direct scanout (`client_scanout::scanout_fourcc`); on the
//! readback route it makes the frame's alpha opaque. A real client commits
//! linear memfd dma-bufs over a socket pair here, so the frame this machine
//! captures — read back, as this test process has no presenting GPU — shows
//! which verdict each commit got.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::Instant;

use shell::compositor::foreign::store;
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use vello::kurbo::Rect;
use wayland_client::delegate_noop;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_region, wl_surface};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

const WIDTH: i32 = 64;
const HEIGHT: i32 = 48;
const STRIDE: u32 = WIDTH.unsigned_abs() * 4;
const BUFFER_LEN: u32 = STRIDE * HEIGHT.unsigned_abs();
/// A premultiplied, half-covering texel as the buffer holds it: `[B, G, R, A]`.
const TEXEL: [u8; 4] = [0x10, 0x20, 0x40, 0x80];

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configured: bool,
}

crate::support::wayland::test_client!(Client: registry, ping, configured);

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_region::WlRegion);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1);
delegate_noop!(Client: ignore zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1);

type Harness = crate::support::wayland::Harness<Client>;

/// What a commit does to the surface's opaque region.
#[derive(Clone, Copy)]
enum Opaque {
    /// Sets it to this rectangle, `x, y, width, height`.
    Set(i32, i32, i32, i32),
    /// Unsets it: the whole surface may be translucent.
    Unset,
    /// Leaves the one an earlier commit set.
    Keep,
}

/// A linear single-plane buffer holding `TEXEL` everywhere, as a memfd the
/// compositor's CPU importer reads like a linear PRIME buffer.
fn texel_memfd() -> OwnedFd {
    use std::io::Write as _;
    // SAFETY: memfd_create with a static nul-terminated name and MFD_CLOEXEC.
    let raw = unsafe { libc::memfd_create(c"argb-opaque-region".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: memfd_create returned a new descriptor that nothing else owns.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(&TEXEL.repeat((BUFFER_LEN / 4) as usize))
        .expect("fill memfd");
    OwnedFd::from(file)
}

/// The compositor with a `zwp_linux_dmabuf_v1` global and a configured,
/// adopted toplevel. A machine without a render node advertises no dmabuf
/// global; one is created here for the client, as the commit path under test
/// does not depend on which GPU the client allocated on.
struct Fixture {
    h: Harness,
    dmabuf: zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    compositor: wl_compositor::WlCompositor,
    surface: wl_surface::WlSurface,
    surface_id: u64,
    memfd: OwnedFd,
}

impl Fixture {
    fn new() -> Self {
        let mut h = Harness::new();
        if h.state.dmabuf_global.is_none() {
            let formats = [Fourcc::Argb8888, Fourcc::Xrgb8888].map(|code| Format {
                code,
                modifier: Modifier::Linear,
            });
            let feedback = DmabufFeedbackBuilder::new(0, formats)
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
            h.pump();
        }
        let dmabuf = h.bind::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>("zwp_linux_dmabuf_v1", 4);
        let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm_base = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        h.pump();

        let qh = h.queue.handle();
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id("org.tilo.test.argb-opaque-region".into());
        surface.commit();
        h.pump();
        let toplevels = h.state.xdg_shell_state.toplevel_surfaces().to_vec();
        assert_eq!(toplevels.len(), 1, "the client's toplevel");
        let surface_id = shell::compositor::state::surface_id(toplevels[0].wl_surface());
        store().note_placement(surface_id, Rect::new(0.0, 0.0, 640.0, 480.0));
        shell::compositor::foreign::flush_configures(&mut h.state);
        h.pump();
        assert!(
            h.client.configured,
            "the toplevel was never configured, so a buffer commit would be a protocol error rather than the path under test"
        );
        std::mem::forget((xdg_surface, toplevel, wm_base));
        Self {
            h,
            dmabuf,
            compositor,
            surface,
            surface_id,
            memfd: texel_memfd(),
        }
    }

    /// Commit a fresh `fourcc` buffer over the texel memfd, doing `opaque`
    /// to the surface's opaque region, and return the frame the compositor
    /// captured for that commit, tightly packed RGBA.
    fn commit(&mut self, fourcc: Fourcc, opaque: Opaque) -> Vec<u8> {
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
            Opaque::Set(x, y, width, height) => {
                let region = self.compositor.create_region(&qh, ());
                region.add(x, y, width, height);
                self.surface.set_opaque_region(Some(&region));
                region.destroy();
            }
            Opaque::Unset => self.surface.set_opaque_region(None),
            Opaque::Keep => {}
        }
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, WIDTH, HEIGHT);
        self.surface.commit();
        std::mem::forget((buffer, params));

        // The capture runs on a worker; the commit applies once it is done.
        let display = self.h.display.handle();
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("the hang bound fits in an Instant");
        loop {
            self.h.pump();
            self.h.state.pump_foreign_capture_barriers(&display);
            if let Some(frame) = store().snapshot(self.surface_id)
                && frame.seq != before
            {
                assert_eq!(
                    (frame.width, frame.height),
                    (WIDTH.unsigned_abs(), HEIGHT.unsigned_abs())
                );
                return frame.rgba;
            }
            assert!(
                Instant::now() < deadline,
                "the {fourcc:?} commit was never captured"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// Every texel's alpha, which must all be `alpha`, in the colour the buffer
/// holds.
fn assert_every_texel(rgba: &[u8], alpha: u8, what: &str) {
    let [b, g, r, _] = TEXEL;
    for (index, texel) in rgba.chunks_exact(4).enumerate() {
        assert_eq!(texel, [r, g, b, alpha], "{what}: texel {index}");
    }
}

#[test]
fn an_argb_commit_is_opaque_exactly_when_its_opaque_region_covers_the_surface() {
    let mut fixture = Fixture::new();

    // Phase `argb_with_an_opaque_region_covering_the_surface`: a fullscreen
    // browser. Its alpha is padding, so it is drawn opaque — and on DRM
    // scanned out as XRGB8888.
    let frame = fixture.commit(Fourcc::Argb8888, Opaque::Set(0, 0, WIDTH, HEIGHT));
    assert_every_texel(&frame, 0xFF, "a covering opaque region");

    // Phase `the_region_persists`: a later commit that leaves the region
    // alone keeps the one set before, as `wl_surface` state does.
    let frame = fixture.commit(Fourcc::Argb8888, Opaque::Keep);
    assert_every_texel(&frame, 0xFF, "the region an earlier commit set");

    // Phase `argb_partly_translucent`: an opaque region that leaves a strip
    // out (rounded corners, a shadow) keeps the alpha, so the frame is
    // composited over what is below.
    let frame = fixture.commit(Fourcc::Argb8888, Opaque::Set(0, 0, WIDTH, HEIGHT - 1));
    assert_every_texel(&frame, 0x80, "a partial opaque region");

    // Phase `argb_without_an_opaque_region`.
    let frame = fixture.commit(Fourcc::Argb8888, Opaque::Unset);
    assert_every_texel(&frame, 0x80, "no opaque region");

    // Phase `xrgb_unchanged`: its alpha byte is padding with or without a
    // region.
    let frame = fixture.commit(Fourcc::Xrgb8888, Opaque::Unset);
    assert_every_texel(&frame, 0xFF, "an XRGB buffer");
}
