//! Real Vulkan WSI presentation hints received by the production Wayland server.
use std::collections::HashSet;
use std::os::unix::net::UnixStream;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use shell::compositor::foreign::store;
use shell::compositor::state::{ClientState, State, surface_id};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::compositor::{self, BufferAssignment, Damage, SurfaceAttributes};
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

#[derive(Clone, Debug)]
struct Commit {
    ordinal: usize,
    surface: u64,
    rectangles: Vec<(i32, i32, i32, i32)>,
    buffer_coordinates: bool,
}

struct Server {
    stop: Arc<AtomicBool>,
    commits: Arc<Mutex<Vec<Commit>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(socket: UnixStream) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let commits = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = Arc::clone(&stop);
        let worker_commits = Arc::clone(&commits);
        let thread = std::thread::spawn(move || {
            let event_loop = EventLoop::try_new().expect("calloop");
            let mut display = Display::new().expect("Wayland display");
            let mut state = State::new(event_loop.handle(), display.handle());
            display
                .handle()
                .insert_client(socket, Arc::new(ClientState::default()))
                .expect("WSI client");
            let mut observed = HashSet::new();
            let deadline = tilo_test_env::hang_deadline();
            while !worker_stop.load(Ordering::Acquire) {
                assert!(
                    Instant::now() < deadline,
                    "WSI server exceeded its deadline"
                );
                display
                    .dispatch_clients(&mut state)
                    .expect("dispatch WSI requests");
                for role in state.xdg_shell_state.toplevel_surfaces().to_vec() {
                    let surface = role.wl_surface();
                    let id = surface_id(surface);
                    store().note_placement(id, vello::kurbo::Rect::new(0.0, 0.0, 64.0, 64.0));
                    if observed.insert(id) {
                        let recorded = Arc::clone(&worker_commits);
                        compositor::add_pre_commit_hook::<State, _>(
                            surface,
                            move |_, _, surface| {
                                compositor::with_states(surface, |states| {
                                    let (rectangles, buffer_coordinates) = {
                                        let mut attributes =
                                            states.cached_state.get::<SurfaceAttributes>();
                                        let pending = attributes.pending();
                                        if !matches!(
                                            pending.buffer,
                                            Some(BufferAssignment::NewBuffer(_))
                                        ) {
                                            return;
                                        }
                                        let mut buffer_coordinates = true;
                                        let rectangles = pending
                                            .damage
                                            .iter()
                                            .map(|damage| match damage {
                                                Damage::Buffer(rect) => (
                                                    rect.loc.x,
                                                    rect.loc.y,
                                                    rect.size.w,
                                                    rect.size.h,
                                                ),
                                                Damage::Surface(rect) => {
                                                    buffer_coordinates = false;
                                                    (
                                                        rect.loc.x,
                                                        rect.loc.y,
                                                        rect.size.w,
                                                        rect.size.h,
                                                    )
                                                }
                                            })
                                            .collect();
                                        drop(attributes);
                                        (rectangles, buffer_coordinates)
                                    };
                                    let mut recorded = recorded.lock().unwrap();
                                    let ordinal = recorded.len().saturating_add(1);
                                    recorded.push(Commit {
                                        ordinal,
                                        surface: surface_id(surface),
                                        rectangles,
                                        buffer_coordinates,
                                    });
                                });
                            },
                        );
                    }
                }
                shell::shell::foreign::flush_configures(&mut state);
                // This fixture has no paint thread. Use the same production
                // throttle release as an idle desktop so Mesa's FIFO WSI can
                // finish presenting instead of waiting for a synthetic paint.
                shell::shell::foreign::release_idle_frame_callbacks(&mut state);
                display.flush_clients().expect("flush WSI events");
                std::thread::sleep(Duration::from_millis(1));
            }
            for id in observed {
                store().forget(id);
            }
        });
        Self {
            stop,
            commits,
            thread: Some(thread),
        }
    }

    fn captured(
        &self,
        previous: u64,
        previous_commit: usize,
        mut checkpoint: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    ) -> (Commit, shell::shell::foreign::FrameSnapshot) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            // Keep damage records stable while observing the production frame.
            // The checkpoint releases the lock to force the old reader race.
            let mut commits = self.commits.lock().unwrap();
            let latest = commits.last().cloned();
            if let Some((read, resume)) = checkpoint.take() {
                drop(commits);
                read.send(()).expect("metadata read checkpoint");
                resume
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("resume capture after presentation");
                commits = self.commits.lock().unwrap();
            }
            if let Some(commit) = latest
                && commit.ordinal > previous_commit
                && let Some(frame) = store().snapshot(commit.surface)
                && frame.seq > previous
            {
                return (commit, frame);
            }
            drop(commits);
            assert!(
                Instant::now() < deadline,
                "WSI pixels never reached capture"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("WSI server");
        }
    }
}

#[derive(Default)]
struct Peer {
    compositor: Option<wl_compositor::WlCompositor>,
    desktop: Option<xdg_wm_base::XdgWmBase>,
    configured: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        peer: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => peer.compositor = Some(registry.bind(name, 4, qh, ())),
                "xdg_wm_base" => peer.desktop = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}
crate::support::wayland::test_client!(Peer: ping, configured);
delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
delegate_noop!(Peer: ignore wl_surface::WlSurface);
delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);

fn paint(
    surface: &wgpu::Surface<'_>,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    changed: bool,
) {
    let frame = match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame) => frame,
        other => panic!("real WSI acquisition failed: {other:?}"),
    };
    let mut pixels = if matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        [0, 0, 255, 255].repeat(4096)
    } else {
        [255, 0, 0, 255].repeat(4096)
    };
    if changed {
        for y in 10_usize..14 {
            for x in 10_usize..14 {
                let offset = y.saturating_mul(64).saturating_add(x).saturating_mul(4);
                pixels[offset..offset.saturating_add(4)].copy_from_slice(&[0, 255, 0, 255]);
            }
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &frame.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(256),
            rows_per_image: Some(64),
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([]);
    frame.present();
}

fn gpu_surface<'a>(
    connection: &'a Connection,
    native: &'a wl_surface::WlSurface,
) -> (
    wgpu::Surface<'a>,
    wgpu::Device,
    wgpu::Queue,
    wgpu::TextureFormat,
) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let display = wgpu::rwh::WaylandDisplayHandle::new(
        NonNull::new(connection.backend().display_ptr().cast()).expect("native display"),
    );
    let window = wgpu::rwh::WaylandWindowHandle::new(
        NonNull::new(native.id().as_ptr().cast()).expect("native surface"),
    );
    // SAFETY: the connection and wl_surface outlive the Vulkan surface below;
    // both pointers come from the matching live libwayland backend.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(wgpu::rwh::RawDisplayHandle::Wayland(display)),
            raw_window_handle: wgpu::rwh::RawWindowHandle::Wayland(window),
        })
    }
    .expect("native Vulkan WSI surface");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("Vulkan WSI adapter");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("Vulkan WSI device");
    let capabilities = surface.get_capabilities(&adapter);
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| {
            matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm
                    | wgpu::TextureFormat::Bgra8UnormSrgb
                    | wgpu::TextureFormat::Rgba8Unorm
                    | wgpu::TextureFormat::Rgba8UnormSrgb
            )
        })
        .expect("eight-bit WSI format");
    surface.configure(
        &device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::COPY_DST,
            format,
            width: 64,
            height: 64,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
        },
    );
    (surface, device, queue, format)
}

#[test]
fn vulkan_regions_reach_wayland_with_exact_captured_pixels() {
    let (socket, peer_socket) = UnixStream::pair().expect("isolated Wayland socket");
    let server = Server::start(socket);
    let connection = Connection::from_socket(peer_socket).expect("WSI connection");
    let mut events = connection.new_event_queue();
    let qh = events.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut peer = Peer::default();
    events.roundtrip(&mut peer).expect("WSI globals");
    let native = peer.compositor.as_ref().unwrap().create_surface(&qh, ());
    let role = peer
        .desktop
        .as_ref()
        .unwrap()
        .get_xdg_surface(&native, &qh, ());
    let toplevel = role.get_toplevel(&qh, ());
    toplevel.set_app_id("org.tilo.test.presentation-damage".into());
    native.commit();
    let deadline = tilo_test_env::hang_deadline();
    while !peer.configured {
        assert!(
            Instant::now() < deadline,
            "initial WSI configure was not received"
        );
        events.roundtrip(&mut peer).expect("initial configure");
    }
    connection.flush().expect("configure acknowledgement");
    let (surface, device, queue, format) = gpu_surface(&connection, &native);
    paint(&surface, &queue, format, false);
    let (first, pixels) = server.captured(0, 0, None);
    assert_eq!((pixels.width, pixels.height), (64, 64));
    assert_eq!(&pixels.rgba[..4], &[255, 0, 0, 255]);
    let previous = pixels.seq;
    // SAFETY: only borrow the configured HAL surface to set the checked hint;
    // no native handles escape the guard or have their ownership changed.
    assert!(
        unsafe { surface.as_hal::<wgpu::hal::vulkan::Api>() }
            .unwrap()
            .set_next_present_damage(&[wgpu::hal::vulkan::PresentDamageRect {
                x: 10,
                y: 10,
                width: 4,
                height: 4,
            }]),
        "this native WSI must support incremental presentation"
    );
    // Force the server to publish the second presentation after the reader
    // took its metadata snapshot. The metadata and pixel cursors must both
    // advance: the old full-damage commit cannot describe the new pixels.
    let (read, observed) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let (second, pixels) = std::thread::scope(|scope| {
        let server = &server;
        let ordinal = first.ordinal;
        let reader = scope.spawn(move || server.captured(previous, ordinal, Some((read, resumed))));
        let deadline = tilo_test_env::hang_deadline();
        observed
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("capture reader took its metadata snapshot");
        paint(&surface, &queue, format, true);
        while store().frame_seq(first.surface) <= previous {
            assert!(
                Instant::now() < deadline,
                "second presentation was not captured"
            );
            std::thread::yield_now();
        }
        resume.send(()).expect("release capture reader");
        reader.join().expect("capture reader")
    });
    assert!(
        second.buffer_coordinates,
        "driver damage must be in buffer pixels"
    );
    assert_eq!(second.rectangles, vec![(10, 10, 4, 4)]);
    assert_eq!(&pixels.rgba[..4], &[255, 0, 0, 255]);
    let inside = 10_usize
        .saturating_mul(64)
        .saturating_add(10)
        .saturating_mul(4);
    assert_eq!(
        &pixels.rgba[inside..inside.saturating_add(4)],
        &[0, 255, 0, 255]
    );
    drop(surface);
    drop(queue);
    drop(device);
    toplevel.destroy();
    role.destroy();
    native.destroy();
    connection.flush().expect("destroy WSI role");
    drop(server);
}
