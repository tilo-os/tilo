//! Opening a PDF the workspace keeps as content in this record: how an agent,
//! which has no files and no picker, gives a markup record its document.
//!
//! The record names its document the way a launch on a file does: by
//! pinning the bytes as its source ([`PdfMarkup::pin_source`]). The bytes
//! stay in the content store; the reference that opens them and the name
//! enter the record, so every replica reads the same document through its
//! own window's store.

use ui::framework::EffectCx;
use ui::tilo_resource::{Bytes, Invoke};

use super::PdfApp;
use crate::markup::PdfMarkup;

/// Why a record with no PDF reads nothing, and how an agent gives it one.
pub(super) const NO_PDF: &str =
    "this record holds no PDF to read yet: open one the workspace keeps with import-pdf-content";

/// The name a PDF opened from content without one is shown under.
const UNNAMED: &str = "Document.pdf";

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
        cx: &EffectCx<PdfMarkup>,
    ) -> Result<serde_json::Value, String> {
        // What the caller named: the content's reference, as it is (a
        // reference's key is case-sensitive), or the bare reference a plan
        // that carried the content gives.
        let reference = ui::tilo_actions::named_reference(address).ok_or_else(|| {
            format!("`{address}` names no content: give the PDF's `tilo.content:<reference>`")
        })?;
        let bytes = self
            .content
            .read(reference)
            .ok_or_else(|| format!("content `{reference}` is not in this workspace's store"))?;
        // The bytes themselves name the document: the reference that opens
        // them may differ between devices.
        let digest = tilo_content::hash_bytes(&bytes);
        let held = cx.live().get().pinned_source();
        if let Some(held) = &held
            && held.hash() != digest
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
        // A Word file sent under a PDF's name: say what opens it, rather than
        // that this reader does not.
        if tilo_content::document_format(&bytes).is_none_or(|format| format.extension != "pdf")
            && let Some(what) = tilo_content::opened_by(&bytes)
        {
            return Err(format!("content `{reference}` is not a PDF: {what}"));
        }
        // The portable engine is memory-safe and runs in process, as an
        // agent's confined window needs.
        let document = tilo_pdf::Engine::portable()
            .open_bytes(&bytes)
            .map_err(|error| {
                format!("content `{reference}` is not a PDF this reader opens: {error}")
            })?;
        let pages = document.page_count();
        let name = if let Some(held) = &held {
            held.name.clone()
        } else {
            let name = name.map_or("", str::trim);
            let name = if name.is_empty() { UNNAMED } else { name };
            // The bytes are already where the workspace keeps content under
            // this reference: the record pins exactly that, sealed as it is.
            cx.submit(Invoke::group([
                crate::actions::pin_source_action(&Bytes::kept_as(&bytes, reference), name),
                PdfMarkup::START_PAGE_PLAN.with(pages),
            ]));
            name.to_owned()
        };
        Ok(serde_json::json!({
            "content": ui::tilo_actions::content_resource(reference),
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

    /// An agent's record with no file opens a PDF from the workspace's content:
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
        assert_eq!(pinned.hash(), hash);
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
            app.state().pinned_source().map(|source| source.hash()),
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

    /// The workspace keeps its content sealed: import-content takes the
    /// reference that opens the PDF, reads the document through it and pins
    /// that same reference, so the record carries what opens it. A reference
    /// this workspace does not hold is refused for what it is, never for how
    /// it is spelled.
    #[serial(pdfium)]
    #[test]
    fn a_sealed_reference_is_imported_as_the_document_it_opens() {
        let mut app = TestApp::<PdfViewerShell>::new().expect("the reader opens");
        let bytes = fixture("sample.pdf");
        let writer = app.content().sealing();
        let reference = writer.put(&bytes).expect("stored");
        assert!(
            tilo_content::is_sealed(&reference),
            "the window's store seals: {reference}"
        );
        let address = tilo_content::content_address(&reference).expect("a reference");
        assert_ne!(
            app.content().read(&address).as_deref(),
            Some(bytes.as_slice()),
            "the store keeps ciphertext under the address"
        );
        let answer = app
            .effect(&import(&reference, Some("sealed.pdf")))
            .expect("the PDF opens")
            .value
            .expect("the import answers");
        assert_eq!(answer["name"], "sealed.pdf");
        app.settle();
        let pinned = app
            .state()
            .pinned_source()
            .expect("the record pins the PDF");
        assert_eq!(pinned.hash(), tilo_content::hash_bytes(&bytes));
        assert_eq!(
            pinned.source.reference().as_deref(),
            Some(reference.as_str()),
            "pinned by the reference that opens it"
        );

        // A workspace holding none of it answers so, rather than that the
        // reference is no content.
        let mut elsewhere = TestApp::<PdfViewerShell>::new().expect("the reader opens");
        let refused = elsewhere
            .effect(&import(&reference, None))
            .expect_err("not held here")
            .to_string();
        assert!(
            refused.contains("is not in this workspace's store"),
            "{refused}"
        );
    }

    /// A Word file whose package holds only its main part, stored.
    fn word_file() -> Vec<u8> {
        let (name, body) = (b"word/document.xml", b"<w:document/>");
        let mut file = b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0\0\0\0\0".to_vec();
        file.extend([13, 0, 0, 0, 13, 0, 0, 0, 17, 0, 0, 0]);
        file.extend(name);
        file.extend(body);
        let directory = file.len();
        file.extend(b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0\0\0\0\0\0\0");
        file.extend([
            13, 0, 0, 0, 13, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        file.extend([0, 0, 0, 0]);
        file.extend(name);
        let size = file.len().saturating_sub(directory);
        file.extend(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
        file.extend(u32::try_from(size).expect("small").to_le_bytes());
        file.extend(u32::try_from(directory).expect("small").to_le_bytes());
        file.extend([0, 0]);
        file
    }

    /// A Word file sent under a PDF's name is refused with what it is and
    /// the action that opens it, and nothing is pinned.
    #[serial(pdfium)]
    #[test]
    fn a_word_file_is_refused_naming_what_opens_it() {
        let mut app = TestApp::<PdfViewerShell>::new().expect("the reader opens");
        let docx = app.content().put(&word_file()).expect("stored");
        let refused = app
            .effect(&import(&docx, Some("payslips.pdf")))
            .expect_err("a Word file is no PDF")
            .to_string();
        assert!(
            refused.contains(
                "is not a PDF: this is a Word document; use word-processor import-docx-content \
                 or read-content"
            ),
            "{refused}"
        );
        app.settle();
        assert!(app.state().pinned_source().is_none());
    }

    /// A PDF the workspace keeps sealed pins the reference that opens it: the
    /// store holds ciphertext, the record's markup is made against those bytes,
    /// and a second member whose device holds only the ciphertext reads the same
    /// document. A record pinned by a plain hash, as earlier versions pinned it,
    /// still reads.
    #[test]
    fn a_sealed_pdf_is_pinned_by_the_reference_that_opens_it() {
        use super::{PdfApp, PdfMarkup};
        use ui::tilo_resource::{Bytes, Replica};

        let bytes = fixture("sample.pdf");
        let writer = tilo_content::ContentStore::memory_in_workspace().sealing();
        let reference = writer.put(&bytes).expect("the PDF is kept");
        assert!(tilo_content::is_sealed(&reference), "{reference}");
        assert!(
            !writer.has(&tilo_content::hash_bytes(&bytes)),
            "the store holds no plaintext"
        );

        // The record pinned in the workspace: the reference, as it was kept.
        let mut replica = Replica::<PdfMarkup>::new();
        replica
            .apply(
                PdfMarkup::PIN_SOURCE
                    .with((Bytes::kept_as(&bytes, &reference), "report.pdf".to_owned())),
            )
            .expect("the record pins the PDF");
        let pinned = PdfMarkup::of(replica.doc())
            .pinned_source()
            .expect("the record's source");
        assert_eq!(pinned.hash(), tilo_content::hash_bytes(&bytes));

        let mut app = PdfApp::empty_with_content(writer.clone());
        app.record_source = Some(pinned.clone());
        let read = app
            .read_text(None, Some(1))
            .expect("the pinned PDF reads on the device it was kept on");
        assert_eq!(read["file"], "report.pdf");

        // A member's device holds the ciphertext alone, under its address, and
        // opens the document by the reference the record names.
        let address = tilo_content::content_address(&reference).expect("a reference");
        let carried = writer.read(&address).expect("the ciphertext");
        assert_ne!(carried, bytes, "the ciphertext is not the PDF");
        let member = tilo_content::ContentStore::memory_in_workspace();
        member.put_public(&carried).expect("kept as carried");
        let mut reader = PdfApp::empty_with_content(member);
        reader.record_source = Some(pinned);
        let read = reader
            .read_text(None, Some(1))
            .expect("the member opens it");
        assert_eq!(read["file"], "report.pdf");

        // A source pinned by its plain hash still reads where the bytes were
        // kept so.
        let plain = tilo_content::ContentStore::memory_in_workspace();
        let hash = plain.put(&bytes).expect("kept in the clear");
        let mut earlier = PdfApp::empty_with_content(plain);
        earlier.record_source = Some(crate::markup::PinnedSource {
            source: Bytes::kept_as(&bytes, &hash),
            name: "report.pdf".into(),
        });
        let read = earlier
            .read_text(None, Some(1))
            .expect("an earlier pin reads");
        assert_eq!(read["file"], "report.pdf");
    }
}
