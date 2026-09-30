//! Document-scanning workflow shared by Camera's Scanner surface.
//!
//! QR detection remains instant in the viewfinder; document capture is a
//! deliberate multi-page session. Every page retains its source image and
//! structured OCR so crop/rotation or language changes can be revisited.

use image::{DynamicImage, GenericImageView, imageops};
use tilo_ocr::Document;
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
        "Camera cannot reach this scanner. Check its connection, then choose it again."
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
    original: DynamicImage,
    pub crop: Crop,
    pub rotation: Rotation,
    pub ocr: Option<Document>,
    /// The automatic correction used for the retained OCR result. Kept next
    /// to it so status/preview UX can be honest and edits invalidate both.
    pub ocr_deskew_degrees: Option<f64>,
}

impl Page {
    #[must_use]
    pub fn new(original: DynamicImage) -> Self {
        let (width, height) = original.dimensions();
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
    ///
    /// # Errors
    ///
    /// Returns an error when the crop is empty, overflows, or lies outside
    /// the captured page.
    pub fn set_crop(&mut self, crop: Crop) -> Result<(), &'static str> {
        if crop.width == 0
            || crop.height == 0
            || crop
                .x
                .checked_add(crop.width)
                .is_none_or(|right| right > self.original.width())
            || crop
                .y
                .checked_add(crop.height)
                .is_none_or(|bottom| bottom > self.original.height())
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
        let rgba = self.original.to_rgba8();
        let (width, height) = rgba.dimensions();
        (
            std::sync::Arc::from(rgba.into_raw()),
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
    ///
    /// # Errors
    ///
    /// Returns an error if the converted pixel crop is not valid for the
    /// captured page.
    pub fn set_normalized_crop(&mut self, selection: CropSelection) -> Result<(), &'static str> {
        let width = self.original.width();
        let height = self.original.height();
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
    pub fn rendered(&self) -> DynamicImage {
        let cropped =
            self.original
                .crop_imm(self.crop.x, self.crop.y, self.crop.width, self.crop.height);
        match self.rotation {
            Rotation::None => cropped,
            Rotation::Clockwise => DynamicImage::ImageRgba8(imageops::rotate90(&cropped)),
            Rotation::UpsideDown => DynamicImage::ImageRgba8(imageops::rotate180(&cropped)),
            Rotation::CounterClockwise => DynamicImage::ImageRgba8(imageops::rotate270(&cropped)),
        }
    }

    /// Runs OCR on the current rendered page and retains the result.
    ///
    /// # Errors
    ///
    /// Propagates OCR engine or decoding errors.
    pub fn recognize(&mut self, language: &str) -> Result<&Document, tilo_ocr::Error> {
        let recognition = tilo_ocr::recognize_detailed(&self.rendered(), language)?;
        self.ocr = Some(recognition.document);
        self.ocr_deskew_degrees = recognition.deskew_degrees;
        // Set immediately above and retained by `self`.
        self.ocr
            .as_ref()
            .ok_or_else(|| tilo_ocr::Error::Engine("OCR result was lost".into()))
    }
}

#[derive(Default)]
pub struct ScanSession {
    pub pages: Vec<Page>,
    pub selected: usize,
}

impl ScanSession {
    pub fn add_page(&mut self, image: DynamicImage) {
        self.pages.push(Page::new(image));
        self.selected = self.pages.len().saturating_sub(1);
    }
    /// Capture from the system's typed SANE boundary and append only after the
    /// complete PNG has decoded. A failed page never corrupts the session.
    ///
    /// # Errors
    ///
    /// Returns an error when scanning fails or the scanner response is not a
    /// valid PNG image.
    pub fn capture_from_sane(
        &mut self,
        device: &str,
        settings: tilo_capture::scanner::ScanSettings,
    ) -> Result<(), String> {
        let bytes = tilo_capture::scanner::scan_page(device, settings)?;
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|error| format!("scanner returned an invalid PNG: {error}"))?;
        self.add_page(image);
        Ok(())
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
    #[must_use]
    pub fn searchable_text(&self) -> String {
        self.pages
            .iter()
            .filter_map(|page| page.ocr.as_ref())
            .map(Document::plain_text)
            .collect::<Vec<_>>()
            .join("\n\n")
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
                let image = page.rendered().to_rgb8();
                tilo_doc_pdf::ScannedPdfPage {
                    width: image.width(),
                    height: image.height(),
                    rgb: image.into_raw(),
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

    #[test]
    fn capture_errors_become_actionable_without_leaking_backend_output() {
        // Phase `capture_errors_become_actionable_without_leaking_backend_output`.
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
        // Phase `editing_a_page_invalidates_only_its_ocr`.
        {
            let mut page = Page::new(DynamicImage::new_rgb8(100, 80));
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
        // Phase `rotation_is_reversible_and_invalidates_stale_ocr`.
        {
            let mut page = Page::new(DynamicImage::new_rgb8(100, 80));
            page.ocr = Some(Document {
                words: Vec::new(),
                language: "eng".into(),
            });
            page.rotate_clockwise();
            assert_eq!(page.rotation, Rotation::Clockwise);
            assert_eq!(
                (page.rendered().width(), page.rendered().height()),
                (80, 100)
            );
            assert!(page.ocr.is_none());
            for _ in 0..3 {
                page.rotate_clockwise();
            }
            assert_eq!(page.rotation, Rotation::None);
        }
        // Phase `normalized_crop_round_trips_through_shared_editor_coordinates`.
        {
            let mut page = Page::new(DynamicImage::new_rgb8(200, 100));
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
        // Phase `pages_can_be_reordered_after_capture`.
        {
            let mut scan = ScanSession::default();
            scan.add_page(DynamicImage::new_rgb8(10, 20));
            scan.add_page(DynamicImage::new_rgb8(30, 40));
            assert!(scan.move_page(1, 0));
            assert_eq!(scan.pages[0].rendered().width(), 30);
        }
        // Phase `searchable_pdf_preserves_page_order_and_recognized_text`.
        {
            let mut scan = ScanSession::default();
            scan.add_page(DynamicImage::new_rgb8(2, 1));
            scan.add_page(DynamicImage::new_rgb8(1, 2));
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
