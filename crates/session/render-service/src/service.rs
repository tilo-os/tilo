//! The service's loop: the broker's socket and every app's channel, one
//! thread, frames drawn as they arrive.
//!
//! Each app connection is a [`Connection`]: its ring, its fonts and images,
//! its surfaces. A frame is read out of the ring once, validated, and
//! drawn; the copy into a buffer is submitted for every surface that got a
//! frame in this pass, then the service waits for the GPU once and tells
//! each app which buffer to attach. An app whose frame takes the GPU longer
//! than its budget has its following frames dropped until it has waited out
//! the overrun, so one app cannot take the room's GPU from the others.
//!
//! Nothing an app sends can stop the loop: a refusal is a message back, a
//! protocol violation closes that app's connection, and a panic while
//! drawing one app's frame (which the validator exists to prevent) closes
//! that connection and rebuilds the renderer.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use tilo_render_protocol::Limits;
use tilo_render_protocol::channel::{Channel, ChannelError, Received};
use tilo_render_protocol::control::{AppMessage, Plane, Refusal, Renderer, ServiceMessage};
use tilo_render_protocol::frame;
use tilo_render_protocol::shm::Mapping;
use tilo_render_protocol::{PROTOCOL_VERSION, SurfaceId};

use crate::broker::{FromService, ToService};
use crate::gpu::Gpu;
use crate::registry::{FontFiles, Registry};
use crate::surface::{Region, Surface};

/// Smallest ring an app may hand over.
const MIN_RING: usize = 64 << 10;

struct Connection {
    app_id: String,
    channel: Channel,
    ring: Option<Mapping>,
    registry: Registry,
    surfaces: HashMap<SurfaceId, Surface>,
    /// Frames are dropped until then: the app overran its GPU budget.
    throttled_until: Option<Instant>,
    /// Frames drawn, for the session log when the app leaves.
    frames: u64,
}

/// What became of a frame.
enum Drawn {
    Done {
        surface: SurfaceId,
        damage: Vec<Region>,
        gpu_us: u32,
    },
    Dropped(Refusal),
    Panicked(String),
}

/// A frame delivered in this pass, told to its app after the GPU finished.
struct Ready {
    connection: u64,
    surface: SurfaceId,
    sequence: u64,
    delivered: crate::surface::Delivered,
    gpu_us: u32,
}

/// The service's state.
pub struct Service {
    gpu: Option<Gpu>,
    fonts: FontFiles,
    limits: Limits,
    connections: HashMap<u64, Connection>,
    /// The connection the renderer last drew for.
    last_tenant: Option<u64>,
    ready: Vec<Ready>,
}

impl Service {
    pub fn new(gpu: Option<Gpu>, fonts: FontFiles, limits: Limits) -> Self {
        Self {
            gpu,
            fonts,
            limits,
            connections: HashMap::new(),
            last_tenant: None,
            ready: Vec::new(),
        }
    }

    /// Serve until the broker goes away.
    pub fn run(mut self, broker: &Channel) -> Result<(), ChannelError> {
        let device = self
            .gpu
            .as_ref()
            .map_or_else(|| "software".to_owned(), |gpu| gpu.adapter.name.clone());
        broker.send(
            &FromService::Ready {
                device,
                gpu: self.gpu.as_ref().is_some_and(Gpu::dmabufs),
            },
            &[],
        )?;
        loop {
            let ids: Vec<u64> = self.connections.keys().copied().collect();
            let mut fds = Vec::with_capacity(ids.len().saturating_add(1));
            fds.push(rustix::event::PollFd::new(
                broker,
                rustix::event::PollFlags::IN,
            ));
            for id in &ids {
                if let Some(connection) = self.connections.get(id) {
                    fds.push(rustix::event::PollFd::new(
                        &connection.channel,
                        rustix::event::PollFlags::IN,
                    ));
                }
            }
            match rustix::event::poll(&mut fds, None) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(ChannelError::Io(error.into())),
            }
            let readable: Vec<bool> = fds.iter().map(|fd| !fd.revents().is_empty()).collect();
            drop(fds);
            if readable.first().copied().unwrap_or(false) {
                match self.serve_broker(broker) {
                    // The broker went away: the session is over.
                    Err(ChannelError::Closed) => return Ok(()),
                    other => other?,
                }
            }
            for (id, ready) in ids.iter().zip(readable.iter().skip(1)) {
                if *ready {
                    self.serve_connection(*id);
                }
            }
            self.finish_pass();
            if let Some(gpu) = &self.gpu
                && let Some(reason) = gpu.lost()
            {
                return Err(ChannelError::Io(std::io::Error::other(format!(
                    "the GPU device was lost: {reason}"
                ))));
            }
        }
    }

    fn serve_broker(&mut self, broker: &Channel) -> Result<(), ChannelError> {
        let Received { message, fds } = broker.recv::<ToService>()?;
        match message {
            ToService::Connect {
                connection,
                app_id,
                room,
            } => {
                let Some(socket) = fds.into_iter().next() else {
                    return Ok(());
                };
                match Channel::from_socket(socket) {
                    Ok(channel) => {
                        let _ = channel.set_nonblocking(true);
                        tracing::info!(app = %app_id, %room, connection, "app connected");
                        self.connections.insert(
                            connection,
                            Connection {
                                app_id,
                                channel,
                                ring: None,
                                registry: Registry::default(),
                                surfaces: HashMap::new(),
                                throttled_until: None,
                                frames: 0,
                            },
                        );
                    }
                    Err(error) => tracing::warn!(%error, "broker sent a channel that is not one"),
                }
            }
            ToService::Drop { connection } => self.close(connection, "the broker dropped it"),
        }
        Ok(())
    }

    fn close(&mut self, id: u64, why: &str) {
        if let Some(connection) = self.connections.remove(&id) {
            tracing::info!(app = %connection.app_id, frames = connection.frames, why, "app disconnected");
        }
        self.ready.retain(|ready| ready.connection != id);
        if self.last_tenant == Some(id) {
            self.last_tenant = None;
        }
    }

    /// Read everything `id` sent, until it would block.
    fn serve_connection(&mut self, id: u64) {
        loop {
            let Some(connection) = self.connections.get(&id) else {
                return;
            };
            match connection.channel.recv::<AppMessage>() {
                Ok(Received { message, fds }) => {
                    if let Err(why) = self.handle(id, message, fds) {
                        self.close(id, &why);
                        return;
                    }
                }
                Err(ChannelError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return;
                }
                Err(ChannelError::Closed) => {
                    self.close(id, "the app closed its channel");
                    return;
                }
                Err(error) => {
                    self.close(id, &error.to_string());
                    return;
                }
            }
        }
    }

    /// One message. `Err` closes the connection.
    fn handle(&mut self, id: u64, message: AppMessage, fds: Vec<OwnedFd>) -> Result<(), String> {
        let limits = self.limits;
        let connection = self.connections.get_mut(&id).ok_or("gone")?;
        let mut fds = fds.into_iter();
        match message {
            AppMessage::Hello { version } => {
                let ring = fds.next().ok_or("a hello without its ring")?;
                let mapping =
                    Mapping::map_sealed(&ring, false, MIN_RING, limits.ring_bytes as usize)
                        .map_err(|error| format!("ring: {error}"))?;
                connection.ring = Some(mapping);
                let font_roots = self
                    .fonts
                    .roots()
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect();
                send(
                    connection.peer(),
                    &ServiceMessage::Welcome {
                        version: version.min(PROTOCOL_VERSION),
                        limits,
                        font_roots,
                        renderer: renderer(self.gpu.as_ref()),
                    },
                    &[],
                );
            }
            AppMessage::Ring => {
                // The app sends it with no frame outstanding; a frame still
                // in the old ring has been answered already, since frames
                // are handled in order.
                let ring = fds.next().ok_or("a ring without its memfd")?;
                let mapping =
                    Mapping::map_sealed(&ring, false, MIN_RING, limits.ring_bytes as usize)
                        .map_err(|error| format!("ring: {error}"))?;
                connection.ring = Some(mapping);
            }
            AppMessage::CreateSurface {
                surface,
                opaque,
                formats,
            } => {
                if connection.surfaces.len() >= limits.surfaces as usize
                    && !connection.surfaces.contains_key(&surface)
                {
                    refuse(connection.peer(), Refusal::OverLimit("surfaces".to_owned()));
                } else {
                    let on_cpu = self.gpu.is_none();
                    connection
                        .surfaces
                        .insert(surface, Surface::new(opaque, formats, on_cpu));
                }
            }
            AppMessage::DestroySurface { surface } => {
                connection.surfaces.remove(&surface);
            }
            AppMessage::Released {
                surface,
                generation,
                buffer,
            } => {
                if let Some(state) = connection.surfaces.get_mut(&surface) {
                    state.release(generation, buffer);
                }
                self.deliver_pending(id, surface);
            }
            registration @ (AppMessage::RegisterImage { .. }
            | AppMessage::DropImage { .. }
            | AppMessage::RegisterFontFile { .. }
            | AppMessage::RegisterFontData { .. }
            | AppMessage::DropFont { .. }) => return self.register(id, registration, fds.next()),
            AppMessage::Frame {
                sequence,
                offset,
                len,
            } => self.frame(id, sequence, (offset, len)),
            AppMessage::Presented {
                surface,
                sequence,
                presented_ns,
                refresh_ns,
                discarded,
            } => {
                tracing::trace!(
                    app = %connection.app_id,
                    surface = surface.0,
                    sequence,
                    presented_ns,
                    refresh_ns,
                    discarded,
                    "presented"
                );
            }
        }
        Ok(())
    }

    /// A font or image registration. `Err` closes the connection.
    fn register(
        &mut self,
        id: u64,
        message: AppMessage,
        fd: Option<OwnedFd>,
    ) -> Result<(), String> {
        let limits = self.limits;
        let connection = self.connections.get_mut(&id).ok_or("gone")?;
        let mut fds = fd.into_iter();
        match message {
            AppMessage::RegisterImage {
                image,
                width,
                height,
                format,
                alpha,
            } => {
                let fd = fds.next().ok_or("an image without its memfd")?;
                if let Err(error) = connection.registry.add_image(
                    image,
                    &fd,
                    (width, height, format, alpha),
                    &limits,
                ) {
                    refuse(connection.peer(), error.into());
                }
            }
            AppMessage::DropImage { image } => connection.registry.drop_image(image),
            AppMessage::RegisterFontFile { font, path, index } => {
                let result = self
                    .fonts
                    .open(&path, index)
                    .and_then(|data| connection.registry.add_font(font, data, &limits));
                if let Err(error) = result {
                    refuse(connection.peer(), error.into());
                }
            }
            AppMessage::RegisterFontData { font, index } => {
                let fd = fds.next().ok_or("a font without its memfd")?;
                if let Err(error) = connection
                    .registry
                    .add_font_data(font, (&fd, index), &limits)
                {
                    refuse(connection.peer(), error.into());
                }
            }
            AppMessage::DropFont { font } => connection.registry.drop_font(font),
            _ => {}
        }
        Ok(())
    }

    /// Read, validate and draw one frame of connection `id`.
    fn frame(&mut self, id: u64, sequence: u64, (offset, len): (u64, u32)) {
        let started = Instant::now();
        match self.draw(id, (offset, len), started) {
            Drawn::Done {
                surface,
                damage,
                gpu_us,
            } => {
                self.last_tenant = Some(id);
                self.after_draw(id, (surface, sequence), damage, gpu_us);
            }
            Drawn::Dropped(why) => {
                if let Some(connection) = self.connections.get(&id) {
                    drop_frame(connection.peer(), sequence, why);
                }
            }
            Drawn::Panicked(message) => {
                tracing::error!(connection = id, %message, "drawing a frame panicked");
                self.close(id, "drawing its frame panicked");
                self.rebuild_renderer();
            }
        }
    }

    fn draw(&mut self, id: u64, (offset, len): (u64, u32), started: Instant) -> Drawn {
        let limits = self.limits;
        let tenant_changed = self.last_tenant != Some(id);
        let Some(connection) = self.connections.get_mut(&id) else {
            return Drawn::Dropped(Refusal::Invalid("gone".to_owned()));
        };
        if connection
            .throttled_until
            .is_some_and(|until| started < until)
        {
            return Drawn::Dropped(Refusal::Throttled);
        }
        let record = connection
            .ring
            .as_ref()
            .and_then(|ring| ring.get(usize::try_from(offset).ok()?, len as usize));
        let Some(record) = record else {
            return Drawn::Dropped(Refusal::Invalid("frame outside the ring".to_owned()));
        };
        let mut frame = match frame::read(record, &connection.registry, &limits) {
            Ok(checked) => checked.frame,
            Err(error) => return Drawn::Dropped(error.into()),
        };
        let surface_id = frame.surface;
        let others: u64 = connection
            .surfaces
            .iter()
            .filter(|(key, _)| **key != surface_id)
            .map(|(_, surface)| surface.pixels())
            .sum();
        let pixels = u64::from(frame.width).saturating_mul(u64::from(frame.height));
        if others.saturating_add(pixels) > limits.surface_pixels {
            return Drawn::Dropped(Refusal::OverLimit("surface pixels".to_owned()));
        }
        let Some(surface) = connection.surfaces.get_mut(&surface_id) else {
            return Drawn::Dropped(Refusal::Invalid("an unknown surface".to_owned()));
        };
        if let Err(error) = surface.apply_layers(&mut frame, limits.layers) {
            return Drawn::Dropped(error.into());
        }
        let registry = &connection.registry;
        let gpu = self.gpu.as_mut();
        let drawn = tilo_rt::panic_boundary::isolate(|| {
            surface.rasterize(gpu, &mut frame, registry, tenant_changed)
        });
        match drawn {
            Ok(Ok(damage)) => Drawn::Done {
                surface: surface_id,
                damage,
                gpu_us: u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX),
            },
            Ok(Err(error)) => Drawn::Dropped(Refusal::RenderFailed(error)),
            Err(panic) => Drawn::Panicked(tilo_rt::panic_boundary::message(&*panic).to_owned()),
        }
    }

    /// Hand the drawn frame over in a buffer, or keep it until the app
    /// releases one.
    fn after_draw(
        &mut self,
        id: u64,
        (surface_id, sequence): (SurfaceId, u64),
        damage: Vec<Region>,
        gpu_us: u32,
    ) {
        let Some(connection) = self.connections.get_mut(&id) else {
            return;
        };
        let peer = (&connection.channel, connection.app_id.as_str());
        let Some(surface) = connection.surfaces.get_mut(&surface_id) else {
            return;
        };
        match surface.ensure_buffers(self.gpu.as_ref()) {
            Ok(false) => {}
            Ok(true) => {
                if let Some(set) = &surface.buffers {
                    let fds: Vec<_> = set.buffers.iter().map(crate::buffers::Buffer::fd).collect();
                    let planes: Vec<Plane> = set.planes.clone();
                    let message = ServiceMessage::Buffers {
                        surface: surface_id,
                        generation: set.generation,
                        width: set.width,
                        height: set.height,
                        kind: set.kind,
                        planes,
                    };
                    if peer.0.send(&message, &fds).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                drop_frame(peer, sequence, Refusal::RenderFailed(error.to_string()));
                return;
            }
        }
        match surface.deliver(self.gpu.as_ref(), &damage) {
            Some(delivered) => self.ready.push(Ready {
                connection: id,
                surface: surface_id,
                sequence,
                delivered,
                gpu_us,
            }),
            None => {
                surface.pending = Some(crate::surface::Pending {
                    sequence,
                    damage,
                    gpu_us,
                });
            }
        }
    }

    /// A frame waiting for a buffer gets the one the app just released.
    fn deliver_pending(&mut self, id: u64, surface_id: SurfaceId) {
        let Some(surface) = self
            .connections
            .get_mut(&id)
            .and_then(|connection| connection.surfaces.get_mut(&surface_id))
        else {
            return;
        };
        let Some(pending) = surface.pending.take() else {
            return;
        };
        match surface.deliver(self.gpu.as_ref(), &pending.damage) {
            Some(delivered) => self.ready.push(Ready {
                connection: id,
                surface: surface_id,
                sequence: pending.sequence,
                delivered,
                gpu_us: pending.gpu_us,
            }),
            None => surface.pending = Some(pending),
        }
    }

    /// Wait for the GPU once, then tell every app of this pass which buffer
    /// to attach, and charge each its GPU time.
    fn finish_pass(&mut self) {
        if self.ready.is_empty() {
            return;
        }
        let started = Instant::now();
        if let Some(gpu) = &self.gpu
            && !gpu.finish()
        {
            // Nothing of this pass was drawn: the GPU stopped or was lost, and `run` ends the
            // service for the broker to start another; apps send their frames again.
            self.ready.clear();
            return;
        }
        let waited = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);
        let limits = self.limits;
        for ready in std::mem::take(&mut self.ready) {
            let Some(connection) = self.connections.get_mut(&ready.connection) else {
                continue;
            };
            let gpu_us = ready.gpu_us.saturating_add(waited);
            connection.frames = connection.frames.saturating_add(1);
            if gpu_us > limits.gpu_time_us {
                let overrun =
                    Duration::from_micros(u64::from(gpu_us.saturating_sub(limits.gpu_time_us)));
                connection.throttled_until = Instant::now().checked_add(overrun);
                tracing::warn!(app = %connection.app_id, gpu_us, "a frame overran its GPU budget");
            }
            let message = ServiceMessage::FrameReady {
                surface: ready.surface,
                sequence: ready.sequence,
                generation: ready.delivered.generation,
                buffer: ready.delivered.buffer,
                damage: ready
                    .delivered
                    .damage
                    .iter()
                    .map(|region| region.as_wire())
                    .collect(),
                gpu_us,
            };
            send(connection.peer(), &message, &[]);
        }
    }

    /// After a panic while drawing, vello's state is not trusted: build the
    /// renderer again (its pipelines are cached by the driver).
    fn rebuild_renderer(&mut self) {
        if let Some(gpu) = self.gpu.as_mut()
            && let Err(error) = gpu.rebuild_renderer()
        {
            tracing::error!(%error, "the renderer could not be rebuilt; drawing on the CPU");
            self.gpu = None;
        }
        self.last_tenant = None;
    }
}

/// What draws the frames: the adapter in wgpu's words, or the CPU rasteriser
/// into shared memory when the service holds no device.
fn renderer(gpu: Option<&Gpu>) -> Renderer {
    gpu.map_or_else(
        || Renderer {
            name: "CPU rasteriser".to_owned(),
            device_type: "Cpu".to_owned(),
            backend: "Shm".to_owned(),
            driver: "render service".to_owned(),
        },
        |gpu| Renderer {
            name: gpu.adapter.name.clone(),
            device_type: format!("{:?}", gpu.adapter.device_type),
            backend: format!("{:?}", gpu.adapter.backend),
            driver: gpu.adapter.driver.clone(),
        },
    )
}
fn send(
    (channel, app): (&Channel, &str),
    message: &ServiceMessage,
    fds: &[std::os::fd::BorrowedFd<'_>],
) {
    if let Err(error) = channel.send(message, fds) {
        tracing::debug!(app, %error, "could not answer the app");
    }
}

fn refuse(to: (&Channel, &str), why: Refusal) {
    tracing::info!(app = to.1, ?why, "request refused");
    send(to, &ServiceMessage::Refused { why }, &[]);
}

fn drop_frame(to: (&Channel, &str), sequence: u64, why: Refusal) {
    tracing::info!(app = to.1, sequence, ?why, "frame dropped");
    send(to, &ServiceMessage::FrameDropped { sequence, why }, &[]);
}

impl Connection {
    fn peer(&self) -> (&Channel, &str) {
        (&self.channel, &self.app_id)
    }
}
