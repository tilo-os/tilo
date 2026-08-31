//! First-page rasterisation of a PDF via pdfium (feature `pdf`).
//!
//! pdfium's `FPDF_InitLibrary` must run once per process and its default
//! marshalling takes a never-released global lock on that first init, so a
//! *second* `Pdfium` deadlocks. We bind once behind a `OnceLock` and leak the
//! handle (it lives the whole process), exactly as `apps/pdf_viewer` does, and
//! serialise every render behind a global `Mutex` because the shared handle is
//! `!Sync` and thumbnail workers are multi-threaded. When no `libpdfium` is on
//! the host the binding fails and we return `None` — the caller draws a badge.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;

use crate::generate::{RgbaThumb, rgba_to_thumb};

/// Serialises all pdfium access — the shared library handle is `!Sync` and
/// callers may drive several worker threads.
static PDF_LOCK: Mutex<()> = Mutex::new(());

/// The address of a leaked, process-lifetime `Pdfium`, or `0` for "no pdfium
/// available". A `usize` because `Pdfium` is `!Sync` and cannot sit in a static
/// directly (see `apps/pdf_viewer/src/source.rs` for the full rationale).
fn shared_pdfium() -> Option<&'static Pdfium> {
    static ADDR: OnceLock<usize> = OnceLock::new();
    let addr = *ADDR.get_or_init(|| match find_pdfium() {
        Some(bindings) => {
            let handle: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
            std::ptr::from_ref::<Pdfium>(handle) as usize
        }
        None => 0,
    });
    if addr == 0 {
        None
    } else {
        // SAFETY: `addr` is a `Box::leak`'d `Pdfium` valid for the whole process.
        Some(unsafe { &*(addr as *const Pdfium) })
    }
}

/// Locate a usable pdfium without ever bundling one (same search order the PDF
/// viewer uses: `QOS_PDFIUM_LIB`, common system paths, `~/.local/lib/pdfium`).
fn find_pdfium() -> Option<Box<dyn PdfiumLibraryBindings>> {
    if let Ok(path) = std::env::var("QOS_PDFIUM_LIB")
        && !path.is_empty()
        && let Ok(b) = Pdfium::bind_to_library(&path)
    {
        return Some(b);
    }
    let mut candidates: Vec<String> = vec![
        "/usr/lib64/libpdfium.so".into(),
        "/usr/lib/libpdfium.so".into(),
        "/usr/lib/x86_64-linux-gnu/libpdfium.so".into(),
        "/app/lib/libpdfium.so".into(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(format!("{home}/.local/lib/pdfium/libpdfium.so"));
    }
    for cand in candidates {
        if Path::new(&cand).exists()
            && let Ok(b) = Pdfium::bind_to_library(&cand)
        {
            return Some(b);
        }
    }
    Pdfium::bind_to_system_library().ok()
}

/// Whether a pdfium library is available on this host (so a caller can decide
/// up front whether PDF previews are real or badge-only).
pub fn available() -> bool {
    shared_pdfium().is_some()
}

/// Rasterise the first page of `pdf` to an RGBA thumbnail no wider than
/// `max_edge`. `None` when pdfium is unavailable or the document is unreadable.
pub fn pdf_thumb(pdf: &[u8], max_edge: u32) -> Option<RgbaThumb> {
    let pdfium = shared_pdfium()?;
    let _guard = PDF_LOCK.lock().ok()?;
    let doc = pdfium.load_pdf_from_byte_vec(pdf.to_vec(), None).ok()?;
    let page = doc.pages().get(0).ok()?;
    // Render at the target width; PDFs are portrait-ish so width drives the size.
    let config =
        PdfRenderConfig::new().set_target_width(Pixels::try_from(max_edge).unwrap_or(Pixels::MAX));
    let bitmap = page.render_with_config(&config).ok()?;
    // `.max(0)` already rules out the sign `.try_from` would otherwise catch;
    // `.unwrap_or(0)` just keeps the fallback consistent with that proof.
    let w = u32::try_from(bitmap.width().max(0)).unwrap_or(0);
    let h = u32::try_from(bitmap.height().max(0)).unwrap_or(0);
    let rgba = bitmap.as_rgba_bytes();
    if w == 0 || h == 0 || rgba.len() != (w as usize).saturating_mul(h as usize).saturating_mul(4) {
        return None;
    }
    rgba_to_thumb(rgba, w, h, max_edge)
}
