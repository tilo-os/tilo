//! Fonts and images, as the service holds them for its apps.
//!
//! **Fonts from the shared view** are mapped once per path and shared by
//! every app of the room: they are public, read-only files, so sharing them
//! leaks nothing, and the glyph outlines vello caches for one app serve all.
//! A path is accepted only inside the font directories the service was
//! started with (the same read-only view apps see), and only after the file
//! parses as a font.
//!
//! **Fonts and images an app uploads** come in sealed, immutable memfds the
//! app created, so the app's cgroup pays for them; the service maps them
//! read-only, counts them against the app's [`Limits`], and drops them with
//! the connection.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_render_protocol::control::{AlphaType, ImageFormat};
use tilo_render_protocol::scene::Resources;
use tilo_render_protocol::shm::Mapping;
use tilo_render_protocol::{FontId, ImageId, Limits, WireError};
use vello::peniko::{Blob, FontData, ImageAlphaType, ImageData};

/// Largest font file the service maps from the shared view.
const MAX_FONT_FILE: usize = 128 << 20;

/// Fonts mapped from the shared read-only view, by canonical path.
pub struct FontFiles {
    roots: Vec<PathBuf>,
    mapped: HashMap<PathBuf, Blob<u8>>,
}

impl FontFiles {
    /// Fonts may come from beneath `roots` only. Roots that do not exist are
    /// dropped; the rest are canonicalized once, now.
    pub fn new(roots: &[PathBuf]) -> Self {
        Self {
            roots: roots
                .iter()
                .filter_map(|root| std::fs::canonicalize(root).ok())
                .collect(),
            mapped: HashMap::new(),
        }
    }

    /// The directories fonts are read from, for the service's confinement.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn open(&mut self, path: &str, index: u32) -> Result<FontData, WireError> {
        if path.len() > tilo_render_protocol::control::MAX_FONT_PATH || !path.starts_with('/') {
            return Err(WireError::Invalid("font path"));
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| WireError::Invalid("font path"))?;
        if !self.roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(WireError::Invalid(
                "a font outside the shared font directories",
            ));
        }
        let blob = if let Some(blob) = self.mapped.get(&canonical) {
            blob.clone()
        } else {
            let blob = map_file(&canonical)?;
            self.mapped.insert(canonical, blob.clone());
            blob
        };
        checked_font(blob, index)
    }
}

fn map_file(path: &Path) -> Result<Blob<u8>, WireError> {
    let file = std::fs::File::open(path).map_err(|_| WireError::Invalid("font file"))?;
    let fd = OwnedFd::from(file);
    let mapping =
        Mapping::map_file(&fd, MAX_FONT_FILE).map_err(|_| WireError::Invalid("font file"))?;
    Ok(Blob::new(Arc::new(mapping)))
}

/// `blob` as face `index`, if it parses as one: glyph outlines are later
/// read from it on the service's side for every app.
fn checked_font(blob: Blob<u8>, index: u32) -> Result<FontData, WireError> {
    skrifa::FontRef::from_index(blob.data(), index)
        .map_err(|_| WireError::Invalid("not a font"))?;
    Ok(FontData::new(blob, index))
}

/// One connection's fonts and images.
#[derive(Default)]
pub struct Registry {
    fonts: HashMap<FontId, FontData>,
    images: HashMap<ImageId, ImageData>,
    image_bytes: u64,
    font_bytes: u64,
}

impl Registry {
    /// Register `font` under `id`.
    ///
    /// # Errors
    ///
    /// Over the connection's font limit.
    pub fn add_font(
        &mut self,
        id: FontId,
        font: FontData,
        limits: &Limits,
    ) -> Result<(), WireError> {
        if !self.fonts.contains_key(&id) && self.fonts.len() >= limits.fonts as usize {
            return Err(WireError::OverLimit("fonts"));
        }
        self.fonts.insert(id, font);
        Ok(())
    }

    /// Register a font the app uploads itself, in the sealed memfd `fd`.
    pub fn add_font_data(
        &mut self,
        id: FontId,
        (fd, index): (&OwnedFd, u32),
        limits: &Limits,
    ) -> Result<(), WireError> {
        let budget = limits.font_bytes.saturating_sub(self.font_bytes);
        let mapping =
            Mapping::map_sealed(fd, true, 1, usize::try_from(budget).unwrap_or(usize::MAX))
                .map_err(|_| WireError::OverLimit("font bytes"))?;
        let bytes = mapping.len() as u64;
        let font = checked_font(Blob::new(Arc::new(mapping)), index)?;
        self.add_font(id, font, limits)?;
        self.font_bytes = self.font_bytes.saturating_add(bytes);
        Ok(())
    }

    pub fn drop_font(&mut self, id: FontId) {
        self.fonts.remove(&id);
    }

    /// Register an image whose tightly packed pixels are in the sealed memfd
    /// `fd`.
    pub fn add_image(
        &mut self,
        id: ImageId,
        fd: &OwnedFd,
        (width, height, format, alpha): (u32, u32, ImageFormat, AlphaType),
        limits: &Limits,
    ) -> Result<(), WireError> {
        if self.images.contains_key(&id) {
            return Err(WireError::Invalid("an image id registered twice"));
        }
        if self.images.len() >= limits.images as usize {
            return Err(WireError::OverLimit("images"));
        }
        if width == 0 || height == 0 || width > limits.surface_edge || height > limits.surface_edge
        {
            return Err(WireError::OverLimit("image size"));
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(WireError::Overflow)?;
        if self.image_bytes.saturating_add(pixels) > limits.image_bytes {
            return Err(WireError::OverLimit("image bytes"));
        }
        let length = usize::try_from(pixels).map_err(|_| WireError::Overflow)?;
        // Page rounding is the only slack a tightly packed image has.
        let mapping = Mapping::map_sealed(fd, true, length, length.saturating_add(1 << 16))
            .map_err(|_| WireError::Invalid("image memfd"))?;
        let image = ImageData {
            data: Blob::new(Arc::new(mapping)),
            format: match format {
                ImageFormat::Rgba8 => vello::peniko::ImageFormat::Rgba8,
                ImageFormat::Bgra8 => vello::peniko::ImageFormat::Bgra8,
            },
            alpha_type: match alpha {
                AlphaType::Straight => ImageAlphaType::Alpha,
                AlphaType::Premultiplied => ImageAlphaType::AlphaPremultiplied,
            },
            width,
            height,
        };
        self.images.insert(id, image);
        self.image_bytes = self.image_bytes.saturating_add(pixels);
        Ok(())
    }

    pub fn drop_image(&mut self, id: ImageId) {
        if let Some(image) = self.images.remove(&id) {
            let pixels = u64::from(image.width)
                .saturating_mul(u64::from(image.height))
                .saturating_mul(4);
            self.image_bytes = self.image_bytes.saturating_sub(pixels);
        }
    }
}

impl Resources for Registry {
    fn font(&self, id: FontId) -> Option<FontData> {
        self.fonts.get(&id).cloned()
    }

    fn image(&self, id: ImageId) -> Option<ImageData> {
        self.images.get(&id).cloned()
    }
}
