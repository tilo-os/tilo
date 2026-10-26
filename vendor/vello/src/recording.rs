// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use peniko::ImageData;
use smallvec::SmallVec;

pub type ResourceBindings = SmallVec<[ResourceProxy; 8]>;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ShaderId(pub usize);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceId(pub NonZeroU64);

impl ResourceId {
    pub fn next() -> Self {
        // We initialize with 1 so that the conversion below succeeds
        static ID_COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(NonZeroU64::new(ID_COUNTER.fetch_add(1, Ordering::Relaxed)).unwrap())
    }
}

/// List of [`Command`]s for an engine to execute in order.
#[derive(Default)]
pub struct Recording {
    pub commands: Vec<Command>,
}

/// Proxy used as a handle to a buffer.
#[derive(Clone, Copy)]
pub struct BufferProxy {
    pub size: u64,
    pub id: ResourceId,
    pub name: &'static str,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ImageFormat {
    Rgba8,
    Bgra8,
}

/// Proxy used as a handle to an image.
#[derive(Clone, Copy)]
pub struct ImageProxy {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub id: ResourceId,
}

#[derive(Clone, Copy)]
pub enum ResourceProxy {
    Buffer(BufferProxy),
    BufferRange {
        proxy: BufferProxy,
        offset: u64,
        size: u64,
    },
    Image(ImageProxy),
}

/// Single command inside a [`Recording`] to get executed by an engine.
pub enum Command {
    /// Commands the data to be uploaded to the given buffer.
    Upload(BufferProxy, Vec<u8>),
    /// Commands the data to be uploaded to the given buffer as a uniform.
    UploadUniform(BufferProxy, Vec<u8>),
    /// Commands the data to be uploaded to the given image.
    UploadImage(ImageProxy, Vec<u8>),
    WriteImage(ImageProxy, [u32; 2], ImageData),
    /// Writes one rect (x, y, width, height) of an image's pixels to where
    /// the image is in the given image (Tilo patch).
    WriteImageRegion(ImageProxy, [u32; 2], ImageData, [u32; 4]),
    Download(BufferProxy),
    /// Commands to clear the buffer from an offset on for a length of the given size.
    /// If the size is [None], it clears until the end.
    Clear(BufferProxy, u64, Option<u64>),
    /// Commands to free the buffer.
    FreeBuffer(BufferProxy),
    /// Commands to free the image.
    FreeImage(ImageProxy),
    // Discussion question: third argument is vec of resources?
    // Maybe use tricks to make more ergonomic?
    // Alternative: provide bufs & images as separate sequences
    Dispatch(ShaderId, (u32, u32, u32), ResourceBindings),
    DispatchIndirect(ShaderId, BufferProxy, u64, ResourceBindings),
    #[cfg(feature = "debug_layers")]
    Draw(DrawParams),
}

/// The type of resource that will be bound to a slot in a shader.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindType {
    /// A storage buffer with read/write access.
    Buffer,
    /// A storage buffer with read only access.
    BufReadOnly,
    /// A small storage buffer to be used as uniforms.
    Uniform,
    /// A storage image.
    Image(ImageFormat),
    /// A storage image with read only access.
    ImageRead(ImageFormat),
    // TODO: Uniform, Sampler, maybe others
}

#[cfg(feature = "debug_layers")]
pub struct DrawParams {
    pub shader_id: ShaderId,
    pub instance_count: u32,
    pub vertex_count: u32,
    pub vertex_buffer: Option<BufferProxy>,
    pub resources: Vec<ResourceProxy>,
    pub target: ImageProxy,
    pub clear_color: Option<[f32; 4]>,
}

impl Recording {
    /// Clear the command stream while retaining both its command capacity and
    /// the previous frame's packed scene upload for the next resolve pass.
    pub(crate) fn reset_reusing_scene_upload(&mut self, scene_upload: &mut Vec<u8>) {
        for command in self.commands.drain(..) {
            if let Command::Upload(proxy, mut data) = command
                && proxy.name == "vello.scene"
            {
                data.clear();
                *scene_upload = data;
            }
        }
    }

    /// Appends a [`Command`] to the back of the [`Recording`].
    pub fn push(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    /// Commands to upload the given data to a new buffer with the given name.
    /// Returns a [`BufferProxy`] to the buffer.
    pub fn upload(&mut self, name: &'static str, data: impl Into<Vec<u8>>) -> BufferProxy {
        let data = data.into();
        let buf_proxy = BufferProxy::new(data.len() as u64, name);
        self.push(Command::Upload(buf_proxy, data));
        buf_proxy
    }

    /// Commands to upload the given data to a new buffer as a uniform with the given name.
    /// Returns a [`BufferProxy`] to the buffer.
    pub fn upload_uniform(&mut self, name: &'static str, data: impl Into<Vec<u8>>) -> BufferProxy {
        let data = data.into();
        let buf_proxy = BufferProxy::new(data.len() as u64, name);
        self.push(Command::UploadUniform(buf_proxy, data));
        buf_proxy
    }

    /// Commands to upload the given data to a new image with the given dimensions and format.
    /// Returns an [`ImageProxy`] to the buffer.
    pub fn upload_image(
        &mut self,
        width: u32,
        height: u32,
        format: ImageFormat,
        data: impl Into<Vec<u8>>,
    ) -> ImageProxy {
        let data = data.into();
        let image_proxy = ImageProxy::new(width, height, format);
        self.push(Command::UploadImage(image_proxy, data));
        image_proxy
    }

    pub fn write_image(&mut self, proxy: ImageProxy, x: u32, y: u32, image: ImageData) {
        self.push(Command::WriteImage(proxy, [x, y], image));
    }

    /// Writes the rect `region` (x, y, width, height) of `image`, which is
    /// at `x`, `y` in `proxy` (Tilo patch).
    pub fn write_image_region(
        &mut self,
        proxy: ImageProxy,
        [x, y]: [u32; 2],
        image: ImageData,
        region: [u32; 4],
    ) {
        self.push(Command::WriteImageRegion(proxy, [x, y], image, region));
    }

    pub fn dispatch<R>(&mut self, shader: ShaderId, wg_size: (u32, u32, u32), resources: R)
    where
        R: IntoIterator,
        R::Item: Into<ResourceProxy>,
    {
        let r = resources.into_iter().map(|r| r.into()).collect();
        self.push(Command::Dispatch(shader, wg_size, r));
    }

    /// Do an indirect dispatch.
    ///
    /// Dispatch a compute shader where the size is determined dynamically.
    /// The `buf` argument contains the dispatch size, 3 `u32` values beginning
    /// at the given byte `offset`.
    pub fn dispatch_indirect<R>(
        &mut self,
        shader: ShaderId,
        buf: BufferProxy,
        offset: u64,
        resources: R,
    ) where
        R: IntoIterator,
        R::Item: Into<ResourceProxy>,
    {
        let r = resources.into_iter().map(|r| r.into()).collect();
        self.push(Command::DispatchIndirect(shader, buf, offset, r));
    }

    #[cfg(feature = "debug_layers")]
    /// Issue a draw call
    pub fn draw(&mut self, params: DrawParams) {
        self.push(Command::Draw(params));
    }

    /// Prepare a buffer for downloading.
    ///
    /// Currently this copies to a download buffer. The original buffer can be freed
    /// immediately after.
    pub fn download(&mut self, buf: BufferProxy) {
        self.push(Command::Download(buf));
    }

    /// Commands to clear the whole buffer.
    pub fn clear_all(&mut self, buf: BufferProxy) {
        self.push(Command::Clear(buf, 0, None));
    }

    /// Commands to free the given buffer.
    pub fn free_buffer(&mut self, buf: BufferProxy) {
        self.push(Command::FreeBuffer(buf));
    }

    /// Commands to free the given image.
    pub fn free_image(&mut self, image: ImageProxy) {
        self.push(Command::FreeImage(image));
    }

    /// Commands to free the given resource.
    pub fn free_resource(&mut self, resource: ResourceProxy) {
        match resource {
            ResourceProxy::Buffer(buf) => self.free_buffer(buf),
            ResourceProxy::BufferRange {
                proxy,
                offset: _,
                size: _,
            } => self.free_buffer(proxy),
            ResourceProxy::Image(image) => self.free_image(image),
        }
    }

    /// Returns a [`Vec`] containing all the [`Command`]s in order.
    pub fn into_commands(self) -> Vec<Command> {
        self.commands
    }
}

impl BufferProxy {
    pub fn new(size: u64, name: &'static str) -> Self {
        let id = ResourceId::next();
        debug_assert!(size > 0);
        Self { id, size, name }
    }
}

impl ImageFormat {
    #[cfg(feature = "wgpu")]
    pub fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
            Self::Bgra8 => wgpu::TextureFormat::Bgra8Unorm,
        }
    }

    #[cfg(feature = "wgpu")]
    pub fn from_wgpu(format: wgpu::TextureFormat) -> Option<Self> {
        match format {
            wgpu::TextureFormat::Rgba8Unorm => Some(Self::Rgba8),
            wgpu::TextureFormat::Bgra8Unorm => Some(Self::Bgra8),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Recording};

    #[test]
    fn reset_retains_commands_and_recycles_the_packed_scene_upload() {
        let mut recording = Recording::default();
        let scene_bytes = vec![7_u8; 4096];
        let scene_ptr = scene_bytes.as_ptr();
        let scene_capacity = scene_bytes.capacity();
        recording.upload("vello.scene", scene_bytes);
        recording.upload_uniform("vello.config", vec![1_u8; 64]);
        let command_capacity = recording.commands.capacity();

        let mut recycled = Vec::new();
        recording.reset_reusing_scene_upload(&mut recycled);

        assert!(recording.commands.is_empty());
        assert!(recording.commands.capacity() >= command_capacity);
        assert!(recycled.is_empty());
        assert_eq!(recycled.capacity(), scene_capacity);
        assert_eq!(recycled.as_ptr(), scene_ptr);

        recycled.resize(4096, 9);
        recording.upload("vello.scene", std::mem::take(&mut recycled));
        let Some(Command::Upload(_, bytes)) = recording.commands.first() else {
            panic!("scene upload should remain an upload command");
        };
        assert_eq!(bytes.as_ptr(), scene_ptr);
    }
}

impl ImageProxy {
    pub fn new(width: u32, height: u32, format: ImageFormat) -> Self {
        let id = ResourceId::next();
        Self {
            width,
            height,
            format,
            id,
        }
    }
}

impl ResourceProxy {
    pub fn new_buf(size: u64, name: &'static str) -> Self {
        Self::Buffer(BufferProxy::new(size, name))
    }

    pub fn new_image(width: u32, height: u32, format: ImageFormat) -> Self {
        Self::Image(ImageProxy::new(width, height, format))
    }

    pub fn as_buf(&self) -> Option<&BufferProxy> {
        match self {
            Self::Buffer(proxy) => Some(proxy),
            _ => None,
        }
    }

    pub fn as_image(&self) -> Option<&ImageProxy> {
        match self {
            Self::Image(proxy) => Some(proxy),
            _ => None,
        }
    }
}

impl From<BufferProxy> for ResourceProxy {
    fn from(value: BufferProxy) -> Self {
        Self::Buffer(value)
    }
}

impl From<ImageProxy> for ResourceProxy {
    fn from(value: ImageProxy) -> Self {
        Self::Image(value)
    }
}
