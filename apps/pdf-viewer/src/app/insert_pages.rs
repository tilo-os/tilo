//! Inserting another file's pages, whoever asks: the page rail's Add, a
//! dropped file and the insert-pages effect all start one import
//! ([`PdfApp::import_from`]); the PDF owner reads the file's pages and their
//! sizes off the UI thread, and [`PdfApp::imported`] puts them into the
//! shared page plan. An effect waiting on the import is answered there.

use ui::framework::{EffectAnswer, EffectReply, Effected};
use ui::tilo_resource::AbsolutePath;

use super::PdfApp;
use crate::markup::{ImportedPage, MAX_IMPORTED_PAGES, PagePlanEntry};
use crate::source::ImportRequest;

/// An insert-pages effect waiting on its import.
pub(super) struct AskedImport {
    request: ImportRequest,
    /// The file's pages it asked for, 1-based, in order; every page when
    /// `None`.
    pages: Option<Vec<usize>>,
    reply: EffectReply,
}

impl PdfApp {
    /// Insert pages `pages` (1-based; every page when `None`) of the PDF or
    /// picture at `path`, the first at position `at` (1-based; after the
    /// last page when `None`), and answer once they are in the page plan.
    pub(crate) fn insert_pages(
        &mut self,
        path: &AbsolutePath,
        at: Option<usize>,
        pages: Option<Vec<usize>>,
        reply: impl FnOnce() -> EffectReply,
    ) -> EffectAnswer {
        if self.raster_document.is_none() {
            return Err("this window shows no PDF to insert pages into: open one first".into());
        }
        if self.pending_import.is_some() {
            return Err("another page import is under way".into());
        }
        let count = self.resource_page_ids.len();
        let at = at.unwrap_or(count + 1);
        if at == 0 || at > count + 1 {
            return Err(format!("the position is 1 to {}", count + 1));
        }
        if let Some(pages) = &pages
            && (pages.is_empty() || pages.len() > MAX_IMPORTED_PAGES || pages.contains(&0))
        {
            return Err(format!(
                "name 1 to {MAX_IMPORTED_PAGES} pages, counting from 1"
            ));
        }
        let path = path.to_path_buf();
        let started = self.import_from(at - 1, |handle, document, at, after| {
            handle.import_path(document, &path, at, after)
        });
        let Some(request) = self.pending_import.clone().filter(|_| started) else {
            return Err(format!("{} could not be imported", path.display()));
        };
        self.asked_import = Some(AskedImport {
            request,
            pages,
            reply: reply(),
        });
        Ok(Effected::Pending)
    }

    /// Put the pages the PDF owner read for `request` (`sizes`, in points,
    /// of the file `hash` holds as `kind`) into the page plan, and answer
    /// the effect that asked for them with their ids.
    pub(super) fn imported(
        &mut self,
        state: &automerge::AutoCommit,
        request: &ImportRequest,
        hash: &str,
        kind: &str,
        sizes: &[(f64, f64)],
    ) {
        self.dirty = true;
        let asked = self
            .asked_import
            .take()
            .filter(|asked| asked.request == *request);
        let chosen = asked.as_ref().and_then(|asked| asked.pages.as_deref());
        let pages = match imported_pages(hash, kind, sizes, chosen) {
            Ok(pages) => pages,
            Err(error) => {
                if let Some(asked) = asked {
                    asked.reply.finish(Err(error));
                }
                return;
            }
        };
        let pages_before = self.doc.page_count();
        let ids: Vec<String> = pages.iter().map(|page| page.id.clone()).collect();
        self.submit(crate::actions::insert_pages_action(
            request.after.as_deref(),
            pages,
        ));
        self.reconcile_shared_page_plan(state);
        self.go_to_page(request.at + 1);
        self.status_note.show(crate::t_args(
            "pdf-note-imported",
            &[("count", ids.len().into())],
        ));
        self.pages_dirty = self.doc.page_count() != pages_before;
        if let Some(asked) = asked {
            let answer = serde_json::json!({
                "pages": ids,
                "at": request.at + 1,
                "page_count": pages_before + ids.len(),
            });
            asked.reply.finish(Ok(ui::tilo_actions::ActionOutput {
                value: Some(answer),
                resources: Vec::new(),
            }));
        }
    }

    /// The import for `request` failed with `error`: the effect waiting on
    /// it is told why.
    pub(super) fn import_failed(&mut self, request: &ImportRequest, error: &str) {
        if let Some(asked) = self
            .asked_import
            .take()
            .filter(|asked| asked.request == *request)
        {
            asked
                .reply
                .finish(Err(format!("the pages could not be imported: {error}")));
        }
    }
}

/// The page-plan entries for pages `chosen` (1-based, in order; every page
/// when `None`) of a file whose pages measure `sizes`, each with an id of
/// its own.
fn imported_pages(
    hash: &str,
    kind: &str,
    sizes: &[(f64, f64)],
    chosen: Option<&[usize]>,
) -> Result<Vec<PagePlanEntry>, String> {
    let all: Vec<usize> = (1..=sizes.len()).collect();
    let chosen = chosen.unwrap_or(&all);
    chosen
        .iter()
        .map(|&page| {
            let (width, height) = page
                .checked_sub(1)
                .and_then(|index| sizes.get(index))
                .ok_or_else(|| format!("the file has {} pages", sizes.len()))?;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a page side in thousandths of a point"
            )]
            let milli = |side: f64| (side * 1000.0).round() as u32;
            Ok(PagePlanEntry {
                id: format!("import-{}", uuid::Uuid::new_v4()),
                source_page: page,
                imported: Some(ImportedPage {
                    hash: hash.to_owned(),
                    kind: kind.to_owned(),
                    page,
                    width_milli: milli(*width),
                    height_milli: milli(*height),
                }),
                rotation: 0,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use ui::framework::testing::TestApp;

    use crate::PdfViewerShell;
    use crate::actions::PdfEffect;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("OUT_DIR")).join(name)
    }

    fn insert(
        app: &mut TestApp<PdfViewerShell>,
        path: &std::path::Path,
        at: Option<usize>,
        pages: Option<Vec<usize>>,
    ) -> Result<serde_json::Value, ui::tilo_actions::DoorError> {
        let effect = PdfEffect::InsertPages {
            path: ui::tilo_resource::AbsolutePath::new(path).expect("an absolute path"),
            at,
            pages,
        };
        let answer = app.effect(&effect)?.value.expect("an insert answers");
        app.settle();
        Ok(answer)
    }

    /// A caller with no rail combines PDFs by path alone: chosen pages of
    /// another PDF go in first in the order named, every page of it goes
    /// after the last, each with its size read from the file, and the
    /// answer names the new pages.
    #[serial(pdfium)]
    #[test]
    fn an_agent_inserts_another_pdfs_pages_by_path_where_it_says() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = dir.path().join("contract.pdf");
        let other = dir.path().join("annex.pdf");
        std::fs::copy(fixture("sample.pdf"), &base).expect("copy the base PDF");
        std::fs::copy(fixture("multipage.pdf"), &other).expect("copy the annex");
        let mut app = crate::app::tests::launched(&base);
        let original = app.state().page_plan();
        let n = original.len();

        let answer = insert(&mut app, &other, Some(1), Some(vec![3, 1])).expect("pages go in");
        let plan = app.state().page_plan();
        assert_eq!(plan.len(), n + 2, "{answer}");
        assert_eq!(answer["page_count"], n + 2);
        assert_eq!(answer["at"], 1);
        let ids: Vec<&str> = plan[..2].iter().map(|page| page.id.as_str()).collect();
        assert_eq!(answer["pages"], serde_json::json!(ids));
        for (entry, page) in plan[..2].iter().zip([3, 1]) {
            let imported = entry.imported.as_ref().expect("an imported page");
            assert_eq!(imported.page, page);
            assert_eq!(imported.kind, "pdf");
            // The annex's pages are 300 by 200 points.
            assert_eq!(
                (imported.width_milli, imported.height_milli),
                (300_000, 200_000)
            );
        }
        assert_eq!(plan[2..], original[..], "the document's own pages follow");

        let answer = insert(&mut app, &other, None, None).expect("every page goes in");
        let plan = app.state().page_plan();
        assert_eq!(plan.len(), n + 5, "{answer}");
        assert_eq!(answer["at"], n + 3);
        let appended: Vec<usize> = plan[n + 2..]
            .iter()
            .map(|entry| entry.imported.as_ref().expect("an imported page").page)
            .collect();
        assert_eq!(
            appended,
            [1, 2, 3],
            "appended after the last page, in order"
        );
        assert_eq!(plan[2..n + 2], original[..]);
    }

    /// A page the file lacks, a position past the end and a path with no
    /// file are refused, and the pages stay as they were.
    #[serial(pdfium)]
    #[test]
    fn inserting_pages_the_file_lacks_or_out_of_place_changes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = dir.path().join("contract.pdf");
        std::fs::copy(fixture("sample.pdf"), &base).expect("copy the base PDF");
        let mut app = crate::app::tests::launched(&base);
        let before = app.state().page_plan();
        let other = fixture("multipage.pdf");

        let refused = insert(&mut app, &other, None, Some(vec![4])).expect_err("no page 4");
        assert!(
            refused.to_string().contains("the file has 3 pages"),
            "{refused}"
        );
        assert!(insert(&mut app, &other, Some(0), None).is_err());
        assert!(insert(&mut app, &other, Some(before.len() + 2), None).is_err());
        assert!(insert(&mut app, &other, None, Some(Vec::new())).is_err());
        assert!(insert(&mut app, &dir.path().join("missing.pdf"), None, None).is_err());
        assert_eq!(app.state().page_plan(), before);
    }
}
