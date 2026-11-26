//! The host calls on documents a page reads as PDF, answered through the device's
//! [`crate::pdf::PdfHost`].

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

impl Core {
    /// `pdf.*`: the device's engine reads the bytes of the files the call names.
    pub(crate) fn pdf_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::PdfSave => {
                let files = self.file_list(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let sources: Vec<&[u8]> = files.iter().map(Vec::as_slice).collect();
                match self.pdf.borrow().save(&sources, &request) {
                    Ok(bytes) => {
                        let file = self
                            .keep_bytes("annotated.pdf", "application/pdf", &bytes)
                            .or_else(trap)?;
                        self.result(Ok(Value::file(file)))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::PdfPicture => {
                let bytes = self.pdf_bytes(a.h(0)?)?;
                let page = a.long(1)?;
                let width = a.long(2)?;
                match self.pdf.borrow().picture(&bytes, page, width) {
                    Ok(png) => {
                        let file = self
                            .keep_bytes("page.png", "image/png", &png)
                            .or_else(trap)?;
                        self.result(Ok(Value::file(file)))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            _ => {
                let bytes = self.pdf_bytes(a.h(0)?)?;
                let page = if matches!(imp, Imp::PdfText | Imp::PdfLinks) {
                    a.long(1)?
                } else {
                    0
                };
                let host = self.pdf.borrow();
                let answer = match imp {
                    Imp::PdfPages => host.pages(&bytes),
                    Imp::PdfOutline => host.outline(&bytes),
                    Imp::PdfText => host.text(&bytes, page),
                    Imp::PdfLinks => host.links(&bytes, page),
                    Imp::PdfFields => host.fields(&bytes),
                    Imp::PdfMarkup => host.markup(&bytes),
                    Imp::PdfImport => host.import(&bytes),
                    _ => return trap("not a pdf import"),
                };
                drop(host);
                self.result(answer.map(Value::text))
            }
        }
    }

    /// The bytes of the file behind `h`, when this device holds them.
    fn pdf_bytes(&self, h: i32) -> HResult<Vec<u8>> {
        let file = self.file_arg(h)?;
        self.file_bytes(&file).or_else(trap)
    }

    /// The bytes of each file in the list behind `h`.
    fn file_list(&self, h: i32) -> HResult<Vec<Vec<u8>>> {
        let files = self.val_whole(h)?;
        let Value::List(items) = files else {
            return trap("a list of files was expected");
        };
        items
            .iter()
            .map(|item| match item {
                Value::File(file) => self.file_bytes(file).or_else(trap),
                other => trap(format!("expected a file, got {}", other.kind_name())),
            })
            .collect()
    }
}
