//! Document-scanning workflow shared by Camera's Scanner surface.
//!
//! QR detection remains instant in the viewfinder; document capture is a
//! deliberate multi-page session. Every page retains its source image and
//! structured OCR so crop/rotation or language changes can be revisited.

use tilo_ocr::Document;
use ui::image_codec::DecodedImage;
use ui::widget::CropSelection;

/// Translate the unstructured SANE/process boundary into short recovery copy.
///
/// Scanner backends vary widely in wording, so this deliberately recognizes
/// broad, stable phrases and never repeats their raw diagnostic in the UI.
#[must_use]
pub fn capture_error_message(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("permission denied")
        || error.contains("access denied")
        || error.contains("not authorized")
    {
        "Camera does not have permission to use this scanner. Check scanner permissions and try again."
    } else if error.contains("busy") || error.contains("in use") {
        "This scanner is busy. Wait for its current task to finish, then try again."
    } else if error.contains("paper jam") || error.contains("document jam") {
        "Clear the scanner's paper jam, then try this page again."
    } else if error.contains("no document")
        || error.contains("out of documents")
        || error.contains("feeder empty")
    {
        "Add a page to the scanner feeder, then try again."
    } else if error.contains("not found")
        || error.contains("no device")
        || error.contains("disconnected")
        || error.contains("cannot connect")
        || error.contains("could not start scanner")
    {
        "Camera cannot reach this scanner. Check its connection, then try again."
    } else {
        "The page could not be scanned. Check the scanner and try again."
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Clockwise,
    UpsideDown,
    CounterClockwise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct Page {
    original: DecodedImage,
    pub crop: Crop,
    pub rotation: Rotation,
    pub ocr: Option<Document>,
    /// The automatic correction used for the retained OCR result. Kept next
    /// to it so status/preview UX can be honest and edits invalidate both.
    pub ocr_deskew_degrees: Option<f64>,
}

impl Page {
    #[must_use]
    pub fn new(original: DecodedImage) -> Self {
        let (width, height) = (original.width, original.height);
        Self {
            original,
            crop: Crop {
                x: 0,
                y: 0,
                width,
                height,
            },
            rotation: Rotation::None,
            ocr: None,
            ocr_deskew_degrees: None,
        }
    }

    /// Replaces the crop rectangle and invalidates recognition derived from
    /// the previous image bounds.
    pub fn set_crop(&mut self, crop: Crop) -> Result<(), &'static str> {
        if crop.width == 0
            || crop.height == 0
            || crop
                .x
                .checked_add(crop.width)
                .is_none_or(|right| right > self.original.width)
            || crop
                .y
                .checked_add(crop.height)
                .is_none_or(|bottom| bottom > self.original.height)
        {
            return Err("crop must remain inside the captured page");
        }
        self.crop = crop;
        self.ocr = None;
        self.ocr_deskew_degrees = None;
        Ok(())
    }

    pub fn rotate_clockwise(&mut self) {
        self.rotation = match self.rotation {
            Rotation::None => Rotation::Clockwise,
            Rotation::Clockwise => Rotation::UpsideDown,
            Rotation::UpsideDown => Rotation::CounterClockwise,
            Rotation::CounterClockwise => Rotation::None,
        };
        self.ocr = None;
        self.ocr_deskew_degrees = None;
    }

    #[must_use]
    pub fn crop_editor_source(&self) -> (std::sync::Arc<[u8]>, u32, u32, CropSelection) {
        let (width, height) = (self.original.width, self.original.height);
        (
            std::sync::Arc::from(self.original.rgba.as_slice()),
            width,
            height,
            CropSelection {
                left: f64::from(self.crop.x) / f64::from(width.max(1)),
                top: f64::from(self.crop.y) / f64::from(height.max(1)),
                right: f64::from(self.crop.x.saturating_add(self.crop.width))
                    / f64::from(width.max(1)),
                bottom: f64::from(self.crop.y.saturating_add(self.crop.height))
                    / f64::from(height.max(1)),
            },
        )
    }

    /// Applies shared crop-editor coordinates to this page.
    pub fn set_normalized_crop(&mut self, selection: CropSelection) -> Result<(), &'static str> {
        let width = self.original.width;
        let height = self.original.height;
        let x = (selection.left.clamp(0.0, 1.0) * f64::from(width)).round() as u32;
        let y = (selection.top.clamp(0.0, 1.0) * f64::from(height)).round() as u32;
        let right = (selection.right.clamp(0.0, 1.0) * f64::from(width)).round() as u32;
        let bottom = (selection.bottom.clamp(0.0, 1.0) * f64::from(height)).round() as u32;
        self.set_crop(Crop {
            x: x.min(width.saturating_sub(1)),
            y: y.min(height.saturating_sub(1)),
            width: right.saturating_sub(x).max(1),
            height: bottom.saturating_sub(y).max(1),
        })
    }

    #[must_use]
    pub fn rendered(&self) -> DecodedImage {
        let cropped = self
            .original
            .cropped(self.crop.x, self.crop.y, self.crop.width, self.crop.height)
            .unwrap_or_else(|| self.original.clone());
        match self.rotation {
            Rotation::None => cropped,
            Rotation::Clockwise => cropped.turned(1),
            Rotation::UpsideDown => cropped.turned(2),
            Rotation::CounterClockwise => cropped.turned(3),
        }
    }
}

/// The page a scanner handed back as `bytes`, decoded by the toolkit's
/// codec: the browser's own decoder on the web, where a page decodes at all.
/// The caller is the capture worker, which may wait for the decode.
///
/// # Errors
///
/// Why the bytes are no image the codec reads.
pub fn scanned_page(bytes: &[u8]) -> Result<DecodedImage, String> {
    ui::rt::block_on(ui::image_codec::decode(bytes, None))
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[derive(Default)]
pub struct ScanSession {
    pub pages: Vec<Page>,
    pub selected: usize,
}

impl ScanSession {
    pub fn add_page(&mut self, image: DecodedImage) {
        self.pages.push(Page::new(image));
        self.selected = self.pages.len().saturating_sub(1);
    }
    pub fn move_page(&mut self, from: usize, to: usize) -> bool {
        if from >= self.pages.len() || to >= self.pages.len() || from == to {
            return false;
        }
        let page = self.pages.remove(from);
        self.pages.insert(to, page);
        self.selected = to;
        true
    }
    pub fn remove_page(&mut self, index: usize) -> bool {
        if index >= self.pages.len() {
            return false;
        }
        self.pages.remove(index);
        self.selected = self.selected.min(self.pages.len().saturating_sub(1));
        true
    }

    /// Render the current page order, edits, and OCR layers into a searchable
    /// PDF. Pages without recognized text remain valid image-only pages; the
    /// UI can therefore let a user export immediately and update OCR later.
    #[must_use]
    pub fn searchable_pdf(&self) -> Vec<u8> {
        let pages = self
            .pages
            .iter()
            .map(|page| {
                let image = page.rendered();
                tilo_doc_pdf::ScannedPdfPage {
                    width: image.width,
                    height: image.height,
                    rgb: image.pixels().flat_map(|[r, g, b, _]| [r, g, b]).collect(),
                    recognized_text: page
                        .ocr
                        .as_ref()
                        .map_or_else(String::new, Document::plain_text),
                }
            })
            .collect::<Vec<_>>();
        tilo_doc_pdf::render_scanned_pdf(&pages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scanner's page is decoded by the toolkit's codec, whatever the
    /// scanner wrote: a PNG and a JPEG of the same size both come back as
    /// the page's pixels, and bytes that are no image are refused.
    #[test]
    fn a_scanned_page_is_decoded_by_the_toolkits_codec() {
        let pixels = [30_u8, 60, 90, 255].repeat(12 * 8);
        let png = ui::png::encode_rgba8(12, 8, &pixels).expect("the fixture encodes");
        let page = scanned_page(&png).expect("a scanned PNG decodes");
        assert_eq!((page.width, page.height), (12, 8));
        assert_eq!(page.rgba, pixels);

        let jpeg = ui::jpeg::encode_rgba8_opaque(12, 8, &pixels, 95).expect("the fixture encodes");
        let page = scanned_page(&jpeg).expect("a scanned JPEG decodes");
        assert_eq!((page.width, page.height), (12, 8));

        let refused = scanned_page(b"not an image").expect_err("the scanner wrote no image");
        assert!(!refused.is_empty(), "{refused}");
    }

    #[test]
    fn capture_errors_become_actionable_without_leaking_backend_output() {
        {
            assert_eq!(
                capture_error_message("scanimage: open of device failed: Device busy"),
                "This scanner is busy. Wait for its current task to finish, then try again."
            );
            assert_eq!(
                capture_error_message("sane_start: Document feeder out of documents"),
                "Add a page to the scanner feeder, then try again."
            );
            let raw = "scanner failed: vendor-secret 0x9af2 internal transport panic";
            let message = capture_error_message(raw);
            assert_eq!(
                message,
                "The page could not be scanned. Check the scanner and try again."
            );
            assert!(!message.contains("vendor-secret"));
            assert!(!message.contains("0x9af2"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn page_edits_invalidate_stale_ocr_and_export_a_searchable_pdf() {
        {
            let mut page = Page::new(DecodedImage::filled(100, 80, [0, 0, 0, 255]));
            page.ocr = Some(Document {
                words: Vec::new(),
                language: "eng".into(),
            });
            page.set_crop(Crop {
                x: 5,
                y: 5,
                width: 90,
                height: 70,
            })
            .unwrap();
            assert!(page.ocr.is_none());
        }
        {
            let mut page = Page::new(DecodedImage::filled(100, 80, [0, 0, 0, 255]));
            page.ocr = Some(Document {
                words: Vec::new(),
                language: "eng".into(),
            });
            page.rotate_clockwise();
            assert_eq!(page.rotation, Rotation::Clockwise);
            assert_eq!((page.rendered().width, page.rendered().height), (80, 100));
            assert!(page.ocr.is_none());
            for _ in 0..3 {
                page.rotate_clockwise();
            }
            assert_eq!(page.rotation, Rotation::None);
        }
        {
            let mut page = Page::new(DecodedImage::filled(200, 100, [0, 0, 0, 255]));
            page.set_normalized_crop(CropSelection {
                left: 0.1,
                top: 0.2,
                right: 0.9,
                bottom: 0.8,
            })
            .unwrap();
            assert_eq!(
                page.crop,
                Crop {
                    x: 20,
                    y: 20,
                    width: 160,
                    height: 60,
                }
            );
            let (_, width, height, selection) = page.crop_editor_source();
            assert_eq!((width, height), (200, 100));
            assert!((selection.left - 0.1).abs() < f64::EPSILON);
            assert!((selection.bottom - 0.8).abs() < f64::EPSILON);
        }
        {
            let mut scan = ScanSession::default();
            scan.add_page(DecodedImage::filled(10, 20, [0, 0, 0, 255]));
            scan.add_page(DecodedImage::filled(30, 40, [0, 0, 0, 255]));
            assert!(scan.move_page(1, 0));
            assert_eq!(scan.pages[0].rendered().width, 30);
        }
        {
            let mut scan = ScanSession::default();
            scan.add_page(DecodedImage::filled(2, 1, [0, 0, 0, 255]));
            scan.add_page(DecodedImage::filled(1, 2, [0, 0, 0, 255]));
            scan.pages[0].ocr = Some(Document {
                words: vec![tilo_ocr::Word {
                    text: "First receipt".into(),
                    confidence: 98.0,
                    bounds: tilo_ocr::Bounds {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    page: 1,
                    block: 1,
                    paragraph: 1,
                    line: 1,
                    position: 1,
                }],
                language: "eng".into(),
            });
            scan.pages[1].ocr = Some(Document {
                words: vec![tilo_ocr::Word {
                    text: "Second receipt".into(),
                    confidence: 97.0,
                    bounds: tilo_ocr::Bounds {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 2,
                    },
                    page: 1,
                    block: 1,
                    paragraph: 1,
                    line: 1,
                    position: 1,
                }],
                language: "eng".into(),
            });

            let pdf = scan.searchable_pdf();
            let text = String::from_utf8_lossy(&pdf);
            assert!(text.starts_with("%PDF-1.7"));
            assert!(text.contains("/Count 2"));
            assert!(text.contains("First receipt"));
            assert!(text.contains("Second receipt"));
            assert!(text.contains("3 Tr"), "OCR layer must be invisible");
        }
    }
}
