//! Tilo's PDF engine, as an engine library beside the shared runtime
//! (`libtilo_pdf_engine.so`).
//!
//! `tilo-pdf` carries two PDF engines' worth of code: hayro, the in-process
//! rasteriser with its own font stack (an older `skrifa` and `read-fonts`
//! than the toolkit's), CPU renderer (`vello_cpu`), character-map tables and
//! JPEG 2000 decoder, and the pdfium helper's client, plus PDF export
//! (`pdf-writer`, `subsetter`). None of it is on any app's way to its first frame, and only
//! the apps that read PDFs reach it at all: PDF Viewer, Files and Print
//! Centre (Writer writes its PDFs through `tilo-doc-pdf`). Inside `libruntime_dylib.so` every app process
//! mapped and relocated it. Here it is a library of its own in the runtime
//! bundle that only those apps name `DT_NEEDED`.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides, which
//! records it as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_pdf;

/// Run the engine's own code: parse a one-page document and rasterise it in
/// process (hayro, the engine every target has), checking its geometry and
/// the colour it painted.
///
/// The engine's check binary and the apps' `--probe-pdf-engine` call it, so
/// a release proves the library loads and runs against the runtime it ships
/// with, and that the apps load it themselves.
///
/// # Errors
/// Names what the engine read or drew wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use tilo_pdf::{Engine, RenderOptions};

    let bytes = one_page_document();
    let document = Engine::portable()
        .open_bytes(&bytes)
        .map_err(|error| format!("hayro could not open the document: {error}"))?;
    if document.page_count() != 1 {
        return Err(format!("{} pages, not 1", document.page_count()));
    }
    let size = document
        .page_size(0)
        .ok_or_else(|| "the page has no size".to_string())?;
    if (size.width - 72.0).abs() > 0.5 || (size.height - 36.0).abs() > 0.5 {
        return Err(format!("the page is {size:?}, not 72 x 36 pt"));
    }
    let bitmap = document
        .render(0, &RenderOptions::default())
        .map_err(|error| format!("hayro could not render the page: {error}"))?;
    // The page is one red rectangle: its centre pixel is red, opaque.
    let (width, height) = (bitmap.width(), bitmap.height());
    let centre = usize::try_from(
        u64::from(height / 2)
            .saturating_mul(u64::from(width))
            .saturating_add(u64::from(width / 2))
            .saturating_mul(4),
    )
    .map_err(|_| "the bitmap is larger than memory".to_string())?;
    let pixel = bitmap
        .pixels()
        .get(centre..centre.saturating_add(4))
        .ok_or_else(|| format!("a {width} x {height} bitmap without its centre pixel"))?;
    if pixel.first().copied().unwrap_or(0) < 200 || pixel.get(1).copied().unwrap_or(255) > 50 {
        return Err(format!("the page's centre is {pixel:?}, not red"));
    }
    Ok(format!(
        "1 page of {}x{} pt rendered to {width}x{height} pixels",
        size.width, size.height
    ))
}

/// A PDF of one 72 x 36 pt page filled red, with its cross-reference table
/// laid out for the offsets it actually has.
#[cfg(feature = "thin")]
fn one_page_document() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 36] /Contents 4 0 R >>".to_string(),
        {
            let content = "1 0 0 rg 0 0 72 36 re f";
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
        },
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!("{} 0 obj\n{body}\nendobj\n", index.saturating_add(1)).as_bytes(),
        );
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len().saturating_add(1)
        )
        .as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len().saturating_add(1)
        )
        .as_bytes(),
    );
    pdf
}
