//! Opening a PDF the room keeps as content in this record: how an agent,
//! which has no files and no picker, gives a markup record its document.
//!
//! The record names its document the way a launch on a file does: by
//! pinning the bytes' content hash as its source ([`PdfMarkup::pin_source`]).
//! The bytes stay in the content store; only the hash, size and name enter
//! the record, so every replica reads the same document through its own
//! window's store.

use ui::framework::EffectCx;
use ui::tilo_resource::Invoke;

use super::PdfApp;
use crate::markup::PdfMarkup;

/// Why a record with no PDF reads nothing, and how an agent gives it one.
pub(super) const NO_PDF: &str =
    "this record holds no PDF to read yet: open one the room keeps with import-pdf-content";

/// The name a PDF opened from content without one is shown under.
const UNNAMED: &str = "Document.pdf";

/// The content hash `address` names: a `tilo.content:<hash>` address or the
/// bare hash, lowercased.
fn content_hash(address: &str) -> Result<String, String> {
    let address = address.trim();
    let hash = ui::tilo_actions::content_hash(address)
        .unwrap_or(address)
        .to_ascii_lowercase();
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(hash)
    } else {
        Err(format!(
            "`{address}` names no content: give the PDF's `tilo.content:<hash>`"
        ))
    }
}

impl PdfApp {
    /// Pin the PDF `address` names in the content store as the document the
    /// record marks up, under `name`, and answer with its name and page
    /// count. The PDF the record already holds answers again unchanged;
    /// another is refused rather than ignored, since a record never
    /// re-targets its markup.
    ///
    /// The page plan is made with the pin: a window with no PDF worker (an
    /// agent's) never opens the document to make it, and markup is placed
    /// on its page ids.
    pub(crate) fn import_pdf_content(
        &self,
        address: &str,
        name: Option<&str>,
        cx: &EffectCx<PdfMarkup, Self>,
    ) -> Result<serde_json::Value, String> {
        let hash = content_hash(address)?;
        let held = cx.live().get().pinned_source();
        if let Some(held) = &held
            && held.hash != hash
        {
            let name = if held.name.is_empty() {
                "another PDF"
            } else {
                held.name.as_str()
            };
            return Err(format!(
                "this record already marks up {name}: open the PDF in a record of its own"
            ));
        }
        let bytes = self
            .content
            .read(&hash)
            .ok_or_else(|| format!("content `{hash}` is not in this room's store"))?;
        // The portable engine is memory-safe and runs in process, as an
        // agent's confined window needs.
        let document = tilo_pdf::Engine::portable()
            .open_bytes(&bytes)
            .map_err(|error| format!("content `{hash}` is not a PDF this reader opens: {error}"))?;
        let pages = document.page_count();
        let name = if let Some(held) = &held {
            held.name.clone()
        } else {
            let name = name.map_or("", str::trim);
            let name = if name.is_empty() { UNNAMED } else { name };
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            cx.submit(Invoke::group([
                crate::actions::pin_source_action(&hash, len, name),
                PdfMarkup::START_PAGE_PLAN.with(pages),
            ]));
            name.to_owned()
        };
        Ok(serde_json::json!({
            "content": ui::tilo_actions::content_resource(&hash),
            "name": name,
            "page_count": pages,
            "already_held": held.is_some(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use ui::framework::testing::TestApp;

    use crate::PdfViewerShell;
    use crate::actions::PdfEffect;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("OUT_DIR")).join(name)).expect("the fixture")
    }

    fn import(content: &str, name: Option<&str>) -> PdfEffect {
        PdfEffect::ImportPdfContent {
            content: content.to_owned(),
            name: name.map(str::to_owned),
        }
    }

    /// An agent's record with no file opens a PDF from the room's content:
    /// the record pins it with its page plan, and read-text reads it.
    #[serial(pdfium)]
    #[test]
    fn a_pdf_from_content_is_pinned_with_its_pages_and_reads() {
        let mut app = TestApp::<PdfViewerShell>::new().expect("the reader opens");
        let hash = app.content().put(&fixture("sample.pdf")).expect("stored");
        let address = ui::tilo_actions::content_resource(&hash).expect("an address");
        let answer = app
            .effect(&import(&address, Some(" report.pdf ")))
            .expect("the PDF opens")
            .value
            .expect("the import answers");
        assert_eq!(answer["name"], "report.pdf");
        assert_eq!(answer["already_held"], false);
        let pages = answer["page_count"].as_u64().expect("a page count");
        assert!(pages >= 1, "{answer}");
        app.settle();
        let pinned = app
            .state()
            .pinned_source()
            .expect("the record pins the PDF");
        assert_eq!(pinned.hash, hash);
        assert_eq!(pinned.name, "report.pdf");
        assert_eq!(
            app.state().page_plan().len(),
            usize::try_from(pages).expect("few pages")
        );
        let read = app
            .effect(&PdfEffect::ReadText {
                from_page: None,
                to_page: Some(1),
            })
            .expect("the pinned PDF reads")
            .value
            .expect("a read answers");
        assert_eq!(read["file"], "report.pdf");
    }

    /// Opening the PDF the record holds again answers it unchanged; another
    /// PDF is refused, naming the one held.
    #[serial(pdfium)]
    #[test]
    fn a_record_holding_a_pdf_refuses_another_and_answers_its_own_again() {
        let mut app = TestApp::<PdfViewerShell>::new().expect("the reader opens");
        let first = app.content().put(&fixture("sample.pdf")).expect("stored");
        let second = app.content().put(&fixture("latin.pdf")).expect("stored");
        app.effect(&import(&first, None))
            .expect("the first PDF opens");
        app.settle();
        assert_eq!(
            app.state().pinned_source().map(|source| source.name),
            Some("Document.pdf".to_owned())
        );
        let again = app
            .effect(&import(&first, Some("renamed.pdf")))
            .expect("the held PDF answers again")
            .value
            .expect("an answer");
        assert_eq!(again["already_held"], true);
        assert_eq!(again["name"], "Document.pdf");
        let refused = app
            .effect(&import(&second, None))
            .expect_err("another PDF is refused");
        assert!(refused.to_string().contains("Document.pdf"), "{refused}");
        assert_eq!(
            app.state().pinned_source().map(|source| source.hash),
            Some(first)
        );
    }

    /// Content the store lacks, bytes that are no PDF, and an address that
    /// names no content are refused, and nothing is pinned.
    #[serial(pdfium)]
    #[test]
    fn missing_content_or_bytes_that_are_no_pdf_pin_nothing() {
        let mut app = TestApp::<PdfViewerShell>::new().expect("the reader opens");
        let text = app.content().put(b"plain words, no PDF").expect("stored");
        assert!(app.effect(&import(&text, None)).is_err());
        assert!(app.effect(&import(&"ab".repeat(32), None)).is_err());
        assert!(app.effect(&import("report.pdf", None)).is_err());
        app.settle();
        assert!(app.state().pinned_source().is_none());
    }
}
