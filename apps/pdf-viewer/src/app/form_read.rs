//! The PDF's form, read for a caller with no screen: every field an agent
//! fills with a field edit on `form_values/<name>`, with what that edit
//! takes.
//!
//! The fields come from the PDF the record marks up (they are the file's,
//! not the record's); their values are the record's where it holds one,
//! else the file's own, as the page draws them.

use std::collections::BTreeMap;

use tilo_pdf::{FieldKind, FormField};

use super::PdfApp;
use crate::markup::{PagePlanEntry, PdfMarkup};
use crate::model::{FormEntry, FormFieldKind, FormFieldRow, FormFields};

impl PdfApp {
    /// The form fields of the PDF the record marks up, with the values
    /// `markup` holds for them: the fields this window's PDF owner read
    /// when it opened the document, the ones its pages draw; else what the
    /// in-process engine reads of the record's PDF
    /// ([`Self::reading_document`]), so a caller with no screen gets the
    /// form too. A PDF whose form lists no field is refused, never answered
    /// as having none.
    pub(crate) fn read_form_fields(&self, markup: &PdfMarkup) -> Result<serde_json::Value, String> {
        let shown = self.raster_document.is_some() && !self.form_fields.is_empty();
        let (file, fields) = if shown {
            let file = self
                .record_source
                .as_ref()
                .map(|source| source.name.clone())
                .or_else(|| {
                    self.source_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                })
                .unwrap_or_default();
            (file, self.form_fields.clone())
        } else {
            let (file, document) = self.reading_document()?;
            let fields = document
                .form_fields()
                .map_err(|error| format!("the form cannot be read: {error}"))?;
            if fields.is_empty() && document.info().has_form {
                return Err(format!("the form in {file} names no field to read"));
            }
            (file, fields)
        };
        let plan = markup.page_plan();
        let fields: Vec<FormFieldRow> = fields
            .iter()
            .filter_map(|field| field_row(field, &markup.form_values, &plan))
            .collect();
        serde_json::to_value(FormFields { file, fields }).map_err(|error| error.to_string())
    }
}

/// What a caller is told of `field`: `None` for a field no value fills (a
/// button, a signature field) or one with no name to address it by.
fn field_row(
    field: &FormField,
    values: &BTreeMap<String, FormEntry>,
    plan: &[PagePlanEntry],
) -> Option<FormFieldRow> {
    if field.name.is_empty() {
        return None;
    }
    let checked = field.widgets.iter().find(|widget| widget.checked);
    let (kind, in_file, options, export) = match field.kind {
        FieldKind::Text => (
            FormFieldKind::Text,
            FormEntry::Text(field.value.clone()),
            Vec::new(),
            None,
        ),
        FieldKind::CheckBox => (
            FormFieldKind::Checkbox,
            FormEntry::Checked {
                on: checked.is_some(),
                export: None,
            },
            Vec::new(),
            field
                .widgets
                .first()
                .map(|widget| widget.export_value.clone()),
        ),
        FieldKind::RadioButton => {
            let mut options: Vec<String> = Vec::new();
            for widget in &field.widgets {
                if !options.contains(&widget.export_value) {
                    options.push(widget.export_value.clone());
                }
            }
            let value = FormEntry::Checked {
                on: checked.is_some(),
                export: checked.map(|widget| widget.export_value.clone()),
            };
            (FormFieldKind::Radio, value, options, None)
        }
        FieldKind::ComboBox | FieldKind::ListBox => (
            FormFieldKind::Choice,
            FormEntry::Choice(field.value.clone()),
            field.options.clone(),
            None,
        ),
        FieldKind::PushButton | FieldKind::Signature | FieldKind::Unknown => return None,
    };
    let value = values.get(&field.name).cloned().unwrap_or(in_file);
    let edit = PdfMarkup::paths()
        .form_values
        .key(&field.name)
        .set(value.clone());
    let path = serde_json::to_value(&edit).ok().and_then(|edit| {
        edit.get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    let page = field
        .widgets
        .iter()
        .find_map(|widget| shown_page(plan, widget.page));
    Some(FormFieldRow {
        name: field.name.clone(),
        kind,
        value,
        path,
        options,
        export,
        page,
        read_only: field.read_only,
        required: field.required,
    })
}

/// The 1-based page of the document now showing the file's 0-based page
/// `source`: its first occurrence in the page plan, `None` when it was
/// deleted, the file's own number before the record has a plan.
fn shown_page(plan: &[PagePlanEntry], source: usize) -> Option<usize> {
    if plan.is_empty() {
        return Some(source + 1);
    }
    plan.iter()
        .position(|entry| entry.imported.is_none() && entry.source_page == source + 1)
        .map(|index| index + 1)
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use ui::framework::testing::TestApp;

    use crate::PdfViewerShell;
    use crate::actions::PdfEffect;

    /// The engine's form fixture: a text field `name` holding "Ada", a
    /// checkbox `agree` that is off and a choice `colour` of Red, Green and
    /// Blue holding Red, all on its one page.
    fn form_pdf() -> std::path::PathBuf {
        std::path::Path::new(env!("OUT_DIR")).join("form.pdf")
    }

    fn read_fields(app: &mut TestApp<PdfViewerShell>) -> Vec<serde_json::Value> {
        let read = app
            .effect(&PdfEffect::ReadFormFields)
            .expect("the form reads")
            .value
            .expect("a read answers");
        read["fields"].as_array().expect("a list of fields").clone()
    }

    fn field<'a>(fields: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
        fields
            .iter()
            .find(|field| field["name"] == name)
            .unwrap_or_else(|| panic!("no field {name} in {fields:?}"))
    }

    /// A one-page PDF whose form dictionary lists no field.
    fn empty_form_pdf() -> Vec<u8> {
        use std::fmt::Write as _;
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [] >> >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (number, object) in (1..).zip(objects) {
            offsets.push(pdf.len());
            let _ = write!(pdf, "{number} 0 obj\n{object}\nendobj\n");
        }
        let xref = pdf.len();
        let size = offsets.len() + 1;
        let _ = write!(pdf, "xref\n0 {size}\n0000000000 65535 f \n");
        for offset in offsets {
            let _ = writeln!(pdf, "{offset:010} 00000 n ");
        }
        let _ = write!(
            pdf,
            "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        );
        pdf.into_bytes()
    }

    /// A caller with no screen learns every field of a PDF's form, fills
    /// them with the field edit each one names, and reads them back filled.
    #[serial(pdfium)]
    #[test]
    fn an_agent_lists_a_forms_fields_and_fills_them_by_the_edit_they_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("form.pdf");
        std::fs::copy(form_pdf(), &path).expect("copy the form");
        let mut app = crate::app::tests::launched(&path);

        let fields = read_fields(&mut app);
        assert_eq!(fields.len(), 3, "{fields:?}");
        let name = field(&fields, "name");
        assert_eq!(name["kind"], "text");
        assert_eq!(
            name["value"],
            serde_json::json!({ "kind": "text", "value": "Ada" })
        );
        assert_eq!(name["page"], 1);
        assert_eq!(name["read_only"], false);
        assert_eq!(name["required"], false);
        let agree = field(&fields, "agree");
        assert_eq!(agree["kind"], "checkbox");
        assert_eq!(
            agree["value"],
            serde_json::json!({ "kind": "checked", "value": { "on": false, "export": null } })
        );
        assert_eq!(agree["export"], "Yes");
        let colour = field(&fields, "colour");
        assert_eq!(colour["kind"], "choice");
        assert_eq!(
            colour["options"],
            serde_json::json!(["Red", "Green", "Blue"])
        );
        assert_eq!(
            colour["value"],
            serde_json::json!({ "kind": "choice", "value": "Red" })
        );

        // Each field is filled by the published field edit, on the path and
        // in the value shape its row gave.
        for (row, value) in [
            (
                name,
                serde_json::json!({ "kind": "text", "value": "Grace Hopper" }),
            ),
            (
                agree,
                serde_json::json!({ "kind": "checked", "value": { "on": true, "export": null } }),
            ),
            (
                colour,
                serde_json::json!({ "kind": "choice", "value": "Blue" }),
            ),
        ] {
            let edit = serde_json::json!({ "path": row["path"], "op": "set", "value": value });
            app.call(
                ui::tilo_resource::FIELD_EDIT,
                serde_json::json!({ "edit": edit }),
            )
            .unwrap_or_else(|error| panic!("{edit} fills the field: {error}"));
        }
        app.settle();

        let filled = read_fields(&mut app);
        assert_eq!(
            field(&filled, "name")["value"],
            serde_json::json!({ "kind": "text", "value": "Grace Hopper" })
        );
        assert_eq!(
            field(&filled, "agree")["value"]["value"]["on"],
            serde_json::json!(true)
        );
        assert_eq!(
            field(&filled, "colour")["value"],
            serde_json::json!({ "kind": "choice", "value": "Blue" })
        );
        assert_eq!(
            app.state().form_values.get("name"),
            Some(&crate::model::FormEntry::Text("Grace Hopper".into()))
        );
    }

    /// A window with no PDF owner (an agent's, on a PDF pinned from content)
    /// reads a form in process: the same three fields a window showing the
    /// PDF reads, with no screen involved.
    #[test]
    fn a_window_with_no_pdf_owner_reads_the_form() {
        let content = tilo_content::ContentStore::memory();
        let mut app = crate::app::PdfApp::empty_with_content(content.clone());
        let pin = |app: &mut crate::app::PdfApp, fixture: &str| {
            let bytes = std::fs::read(std::path::Path::new(env!("OUT_DIR")).join(fixture))
                .expect("the fixture");
            app.record_source = Some(crate::markup::PinnedSource {
                hash: content.put(&bytes).expect("stored"),
                len: u64::try_from(bytes.len()).expect("small"),
                name: fixture.into(),
            });
        };
        let markup = crate::markup::PdfMarkup::default();
        pin(&mut app, "sample.pdf");
        let read = app
            .read_form_fields(&markup)
            .expect("a PDF without a form reads");
        assert_eq!(read["fields"], serde_json::json!([]));
        pin(&mut app, "form.pdf");
        let read = app
            .read_form_fields(&markup)
            .expect("a form reads where no window shows it");
        assert_eq!(read["file"], "form.pdf");
        let fields = read["fields"].as_array().expect("a list of fields");
        let names: Vec<&str> = fields
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect();
        assert_eq!(names, vec!["name", "agree", "colour"], "{read}");
        let name = field(fields, "name");
        assert_eq!(name["kind"], "text");
        assert_eq!(
            name["value"],
            serde_json::json!({ "kind": "text", "value": "Ada" })
        );
        assert_eq!(name["page"], 1);
        let agree = field(fields, "agree");
        assert_eq!(agree["kind"], "checkbox");
        assert_eq!(
            agree["value"],
            serde_json::json!({ "kind": "checked", "value": { "on": false, "export": null } })
        );
        let colour = field(fields, "colour");
        assert_eq!(colour["kind"], "choice");
        assert_eq!(
            colour["options"],
            serde_json::json!(["Red", "Green", "Blue"])
        );
        assert_eq!(
            colour["value"],
            serde_json::json!({ "kind": "choice", "value": "Red" })
        );
    }

    /// A form dictionary that lists no field is refused, not answered as a
    /// PDF without fields: a caller is told the form holds nothing to fill.
    #[test]
    fn a_form_listing_no_field_is_refused() {
        let content = tilo_content::ContentStore::memory();
        let mut app = crate::app::PdfApp::empty_with_content(content.clone());
        let bytes = empty_form_pdf();
        app.record_source = Some(crate::markup::PinnedSource {
            hash: content.put(&bytes).expect("stored"),
            len: u64::try_from(bytes.len()).expect("small"),
            name: "empty-form.pdf".into(),
        });
        let refused = app
            .read_form_fields(&crate::markup::PdfMarkup::default())
            .expect_err("a form is not answered as empty");
        assert!(refused.contains("names no field"), "{refused}");
    }

    /// A field's page follows its page through the plan: moved later, it is
    /// on the page it moved to.
    #[test]
    fn a_fields_page_is_where_its_page_now_shows() {
        let entry = |source_page: usize| crate::markup::PagePlanEntry {
            id: format!("source-{source_page}"),
            source_page,
            imported: None,
            rotation: 0,
        };
        assert_eq!(super::shown_page(&[], 0), Some(1));
        assert_eq!(super::shown_page(&[entry(2), entry(1)], 0), Some(2));
        assert_eq!(super::shown_page(&[entry(2)], 0), None);
    }
}
