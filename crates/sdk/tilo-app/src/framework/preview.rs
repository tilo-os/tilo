//! History mode for every framework app: past versions rendered by the app's
//! own view, read-only.
//!
//! A view is a function of the resource state, so a historical version needs
//! no separate renderer: the framework reads the resource from the frozen
//! document, builds the view again with editing disabled, and lays it out in
//! History mode's preview pane. Restoring a version is equally generic: the
//! restore schema comes from the resource's stored [`Shape`].

use std::any::TypeId;

use tilo_resource::{Reader, Resource, Shape};
use ui_core::State;
use ui_core::Theme;
use ui_core::automerge::AutoCommit;
use ui_core::history_mode::restore::{
    HistoryMapEntries, HistoryMapField, HistoryMapOverlay, HistoryMembership, HistoryRestoreNode,
    HistoryRestoreSchema, HistoryRootPattern, HistoryRootSelector, PreservedOnCreate,
    RestoreValueType,
};
use ui_core::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui_core::launch::ShotFrame;
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Rect, Vec2};
use ui_core::widget::Widget;

use super::app::{Editing, ResourceApp, build_root, leaked};
use super::root::ResourceRoot;
use ui_core::storage::AppStorage;

struct HistoryKey<A>(std::marker::PhantomData<A>);

struct FileWitnessKey<A>(std::marker::PhantomData<A>);

/// The file scope of `A`'s History alone: what its root declares it owns, so
/// the runner can tell the file source it was handed is the root's.
pub(crate) fn file_history_witness<A: ResourceApp>() -> &'static HistoryDefinition {
    // The definition table is not reentrant: read the whole definition first.
    let definition = history_definition::<A>();
    leaked(TypeId::of::<FileWitnessKey<A>>(), || {
        let scopes: &'static [HistoryScopeDefinition] = Box::leak(
            definition
                .scopes
                .iter()
                .filter(|scope| scope.key == FILE_SCOPE)
                .map(|scope| HistoryScopeDefinition {
                    key: scope.key,
                    title: scope.title,
                    presentation: match scope.presentation {
                        HistoryPresentation::CheckedFiles { create } => {
                            HistoryPresentation::CheckedFiles { create }
                        }
                        _ => unreachable_scope(),
                    },
                    content_schema: None,
                })
                .collect::<Box<[_]>>(),
        );
        HistoryDefinition {
            app_id: A::ID,
            scopes,
        }
    })
}

#[allow(
    clippy::panic,
    reason = "the file scope is built as a checked-file scope above"
)]
fn unreachable_scope() -> ! {
    panic!("the file History scope is a checked-file scope")
}

/// The History scope of the file an app of
/// [`ResourceApp::FILE_HISTORY`](super::ResourceApp::FILE_HISTORY) shows.
const FILE_SCOPE: &str = "file";

/// The History mode registration of `A`: one scope, previewed by its view.
pub(crate) fn history_definition<A: ResourceApp>() -> &'static HistoryDefinition {
    leaked(TypeId::of::<HistoryKey<A>>(), || {
        // A file of the mounted filesystem the app shows is a scope beside
        // the resource's; History moves to it when the app opens a file's.
        let file = A::FILE_HISTORY.map(|file| HistoryScopeDefinition {
            key: FILE_SCOPE,
            title: file.title,
            presentation: HistoryPresentation::CheckedFiles {
                create: file.preview,
            },
            content_schema: None,
        });
        let resource = HistoryScopeDefinition {
            key: "resource",
            title: <A::Resource as Resource>::LABEL,
            presentation: HistoryPresentation::Domain {
                create: preview::<A>,
            },
            content_schema: restore_schema::<A::Resource>(),
        };
        let scopes: &'static [HistoryScopeDefinition] =
            Box::leak(std::iter::once(resource).chain(file).collect::<Box<[_]>>());
        HistoryDefinition {
            app_id: A::ID,
            scopes,
        }
    })
}

/// The History restore schema of `R`, derived from its stored layout:
/// restoring a version writes it back field by field and collection item by
/// item, the way actions write it, so unchanged content keeps its identity and
/// a collaborator's concurrent edits to other items survive. Every field is
/// optional (a record reads absent fields as their empty value) and whatever a
/// field holds is accepted by type; the resource's own `read` is the validator,
/// so a version is restorable exactly when the app can open it.
fn restore_schema<R: Resource>() -> Option<&'static HistoryRestoreSchema> {
    let Some(Shape::Record(fields)) = R::shape() else {
        return None;
    };
    if fields.is_empty() {
        return None;
    }
    let selectors = fields
        .iter()
        .map(|field| HistoryRootSelector {
            pattern: HistoryRootPattern::Exact(field.key),
            required: false,
            node: restore_node(field.shape),
        })
        .collect::<Vec<_>>();
    Some(Box::leak(Box::new(HistoryRestoreSchema {
        id: R::KIND,
        version: 1,
        roots: &[],
        selectors: Box::leak(selectors.into_boxed_slice()),
        preserved_roots: &[],
        preserved: &[],
        validate: readable::<R>,
        validate_result: readable::<R>,
    })))
}

fn restore_node(shape: Shape) -> HistoryRestoreNode {
    let whole = HistoryRestoreNode::Whole {
        expected_type: RestoreValueType::Any,
    };
    let overlay = |fields: Vec<HistoryMapField>, entries: Option<&'static HistoryMapEntries>| {
        HistoryRestoreNode::MapOverlay(Box::leak(Box::new(HistoryMapOverlay {
            fields: Box::leak(fields.into_boxed_slice()),
            entries,
        })))
    };
    match shape {
        Shape::Record(fields) if !fields.is_empty() => overlay(
            fields
                .iter()
                .map(|field| HistoryMapField {
                    key: field.key,
                    required: false,
                    node: restore_node(field.shape),
                })
                .collect(),
            None,
        ),
        Shape::Record(_) | Shape::Value | Shape::Map(_) => whole,
        Shape::Keyed(item) => overlay(
            Vec::new(),
            Some(Box::leak(Box::new(HistoryMapEntries {
                key_limit_bytes: 1024,
                node: restore_node(*item),
                membership: HistoryMembership::TargetAuthoritative,
                on_create: PreservedOnCreate::Omit,
            }))),
        ),
        Shape::Collection(item) => {
            let entry = overlay(
                vec![
                    HistoryMapField {
                        key: tilo_resource::shape::ENTRY_ORDER_KEY,
                        required: false,
                        node: whole,
                    },
                    HistoryMapField {
                        key: tilo_resource::shape::ENTRY_VALUE_KEY,
                        required: false,
                        node: restore_node(*item),
                    },
                ],
                None,
            );
            overlay(
                Vec::new(),
                Some(Box::leak(Box::new(HistoryMapEntries {
                    key_limit_bytes: 1024,
                    node: entry,
                    membership: HistoryMembership::TargetAuthoritative,
                    on_create: PreservedOnCreate::Omit,
                }))),
            )
        }
    }
}

/// A version is valid content exactly when the resource reads from it.
fn readable<R: Resource>(doc: &AutoCommit) -> Result<(), String> {
    R::read(doc).map(drop).map_err(|error| error.to_string())
}

fn preview<A: ResourceApp>(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|error| error.to_string())?;
    let reader = Reader::<A::Resource>::read(&doc).map_err(|error| error.to_string())?;
    Ok(Box::new(ViewPreview {
        root: build_root::<A>(
            reader,
            Editing::ReadOnly,
            AppStorage::for_app(A::ID),
            super::settings::Hosted::default(),
        ),
        doc: Some(doc),
        bounds: Rect::ZERO,
        view: input.initial_view,
    }))
}

/// A past version drawn by the app's own view, exactly as History mode's
/// preview draws it: read-only, over the frozen document. Built by
/// [`HeadlessApp::past_version`](super::testing::HeadlessApp::past_version).
pub struct PastVersion<R: Resource> {
    preview: ViewPreview<R>,
}

impl<R: Resource> PastVersion<R> {
    /// Every name and value the drawn view exposes to assistive technology,
    /// depth first: what a reader of the past version would hear.
    pub fn texts(&self) -> Vec<String> {
        ui_core::a11y::texts(&self.preview.root)
    }

    /// The state the view was presented.
    pub fn state(&self) -> std::rc::Rc<R::State> {
        self.preview.root.live().get()
    }

    /// The drawn size of the view.
    pub fn size(&self) -> Rect {
        self.preview.root.get_rect()
    }
}

/// Lay out and draw `A`'s view over `doc`, a frozen past version, in a
/// `size` pane.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn render_past<A: ResourceApp>(
    doc: AutoCommit,
    size: (u32, u32),
) -> Result<PastVersion<A::Resource>, String> {
    let reader = Reader::<A::Resource>::read(&doc).map_err(|error| error.to_string())?;
    let mut preview = ViewPreview {
        root: build_root::<A>(
            reader,
            Editing::ReadOnly,
            AppStorage::for_app(A::ID),
            super::settings::Hosted::default(),
        ),
        doc: Some(doc),
        bounds: Rect::ZERO,
        view: PreviewView::default(),
    };
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let bounds = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
    preview.layout(bounds, &mut text, &theme)?;
    let mut scene = Scene::new();
    preview.draw(&mut scene, Affine::IDENTITY, &mut text, &theme);
    Ok(PastVersion { preview })
}

/// Lay out `A`'s view over `doc`, a frozen past version, in `bounds` and
/// draw it: what `render` answers for a version.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn draw_past<A: ResourceApp>(
    doc: AutoCommit,
    bounds: Rect,
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
) -> Result<(), String> {
    let reader = Reader::<A::Resource>::read(&doc).map_err(|error| error.to_string())?;
    let mut preview = ViewPreview {
        root: build_root::<A>(
            reader,
            Editing::ReadOnly,
            AppStorage::for_app(A::ID),
            super::settings::Hosted::default(),
        ),
        doc: Some(doc),
        bounds: Rect::ZERO,
        view: PreviewView::default(),
    };
    let mut text = TextRenderer::new();
    preview.layout(bounds, &mut text, theme)?;
    preview.draw(scene, transform, &mut text, theme);
    Ok(())
}

struct ViewPreview<R: Resource> {
    root: ResourceRoot<R>,
    doc: Option<AutoCommit>,
    bounds: Rect,
    view: PreviewView,
}

impl<R: Resource> HistoryPreview for ViewPreview<R> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a preview pane is a few thousand logical pixels"
    )]
    fn layout(
        &mut self,
        bounds: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        let size = (
            bounds.width().max(1.0) as u32,
            bounds.height().max(1.0) as u32,
        );
        let mut frame = ShotFrame::new(size, theme.clone());
        if let Some(doc) = self.doc.take() {
            frame = frame.with_state(State::from_doc(doc));
        }
        // The view is measured by the renderer that draws it — the live
        // window's, with its fonts and text scale — or a label laid out by
        // another renderer can come out too narrow and collapse to "…".
        std::mem::swap(&mut frame.text, text);
        frame.sync(&mut self.root);
        let laid_out = frame.layout(&mut self.root);
        std::mem::swap(&mut frame.text, text);
        laid_out.map_err(|error| error.to_string())?;
        self.doc = Some(std::mem::take(&mut frame.state.doc));
        Ok(())
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let offset = Vec2::new(self.bounds.x0, self.bounds.y0 - self.view.scroll.1);
        let transform = transform.pre_translate(offset);
        self.root
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
    }

    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        match command {
            PreviewNavigation::Scroll { dy, .. } => {
                let content = self.root.get_rect().height();
                let limit = (content - self.bounds.height()).max(0.0);
                let next = (self.view.scroll.1 + dy).clamp(0.0, limit);
                let changed = (next - self.view.scroll.1).abs() > f64::EPSILON;
                self.view.scroll.1 = next;
                changed
            }
            _ => false,
        }
    }

    fn view(&self) -> PreviewView {
        self.view.clone()
    }

    fn accessibility(&self) -> PreviewAccessibility {
        // A screen-reader user hears what the past version says, not only
        // that one is showing.
        let heading = ui_core::i18n::I18nManager::format(
            "history-preview-description",
            &[("resource", R::LABEL.into())],
        );
        let description = std::iter::once(heading)
            .chain(ui_core::a11y::texts(&self.root))
            .collect::<Vec<_>>()
            .join("\n");
        PreviewAccessibility {
            description,
            pages: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    #[derive(Model, Default)]
    pub struct Page {
        pub heading: String,
    }

    #[resource(kind = "test.page", label = "Page")]
    impl Page {
        /// Set the heading.
        #[action]
        pub fn set_heading(&mut self, heading: String) {
            self.heading = heading;
        }
    }

    struct PageApp;

    impl ResourceApp for PageApp {
        type Resource = Page;
        const ID: &'static str = "so.tilo.test-page";
        const TITLE: &'static str = "Page";

        fn view(cx: &mut ViewCx<Page>) -> Element {
            Box::new(Column::new().push(Box::new(
                cx.text(|page| page.heading.clone()).with_id("heading"),
            )))
        }
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Shelf {
        pub title: String,
        pub books: Collection<Book>,
        pub note: Option<String>,
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Book {
        pub name: String,
        pub read: bool,
    }

    #[resource(kind = "test.shelf", label = "Shelf")]
    impl Shelf {
        /// Rename the shelf.
        #[action]
        pub fn rename(&mut self, title: String) {
            self.title = title;
        }

        /// Add a book.
        #[action]
        pub fn add(&mut self, cx: &mut ActionCx, name: String) {
            self.books.push(cx.id(), Book { name, read: false });
        }

        /// Mark a book read.
        #[action]
        pub fn finish(&mut self, book: Id) {
            if let Some(book) = self.books.get_mut(&book) {
                book.read = true;
            }
        }

        /// Remove a book.
        #[action]
        pub fn remove(&mut self, book: Id) {
            self.books.remove(&book);
        }

        /// Annotate the shelf.
        #[action]
        pub fn annotate(&mut self, note: Option<String>) {
            self.note = note;
        }
    }

    fn restore(replica: &mut Replica<Shelf>, target: &[ui_core::automerge::ChangeHash]) {
        use ui_core::history_mode::restore::{
            RestoreOutcome, apply_prepared_content_restore, prepare_content_restore,
        };
        let schema = restore_schema::<Shelf>().expect("a record has a restore schema");
        let mut doc = replica.doc().clone();
        let current = doc.get_heads();
        let plan = prepare_content_restore(&doc, schema, &current, target).unwrap();
        assert_eq!(
            apply_prepared_content_restore(&mut doc, &plan).unwrap(),
            RestoreOutcome::Changed
        );
        doc.commit();
        *replica = Replica::from_doc(doc);
    }

    #[test]
    fn past_versions_restore_by_record_shape_and_render_read_only() {
        // phase: a_record_restores_a_past_version_field_by_field_and_item_by_item
        {
            let mut shelf = Replica::<Shelf>::new();
            shelf.apply(Shelf::RENAME.with("Reading")).unwrap();
            shelf.apply(Shelf::ADD.with("Dune")).unwrap();
            shelf.apply(Shelf::ADD.with("Emma")).unwrap();
            let ids: Vec<Id> = shelf.state().unwrap().books.ids().cloned().collect();
            let past_heads = shelf.heads();
            let past = shelf.state().unwrap();

            shelf.apply(Shelf::RENAME.with("Done")).unwrap();
            shelf.apply(Shelf::FINISH.with(ids[0].clone())).unwrap();
            shelf.apply(Shelf::REMOVE.with(ids[1].clone())).unwrap();
            shelf.apply(Shelf::ADD.with("Ulysses")).unwrap();
            shelf
                .apply(Shelf::ANNOTATE.with(Some("tidy up".into())))
                .unwrap();
            assert_ne!(shelf.state().unwrap(), past);

            restore(&mut shelf, &past_heads);
            assert_eq!(shelf.state().unwrap(), past);
            // Item identity survives: the restored books are the same items.
            assert_eq!(
                shelf
                    .state()
                    .unwrap()
                    .books
                    .ids()
                    .cloned()
                    .collect::<Vec<_>>(),
                ids
            );
        }
        // phase: a_resource_without_a_record_shape_has_no_restore_schema
        {
            struct Opaque;
            impl Resource for Opaque {
                const KIND: &'static str = "test.opaque";
                const LABEL: &'static str = "Opaque";
                type State = ();
                type Action = <Shelf as Resource>::Action;
                fn read(_: &tilo_resource::Doc) -> Result<(), tilo_resource::ModelError> {
                    Ok(())
                }
            }
            assert!(restore_schema::<Opaque>().is_none());
            assert!(restore_schema::<Shelf>().is_some());
        }
        // phase: a_past_version_renders_through_the_apps_own_view_read_only
        {
            let mut page = Replica::<Page>::new();
            page.apply(Page::SET_HEADING.with("First draft")).unwrap();
            let past = page.heads();
            page.apply(Page::SET_HEADING.with("Final")).unwrap();
            let mut doc = page.doc().clone();
            let bytes = doc.fork_at(&past).unwrap().save();

            let frozen = AutoCommit::load(&bytes).unwrap();
            let mut preview = ViewPreview {
                root: build_root::<PageApp>(
                    Reader::read(&frozen).unwrap(),
                    Editing::ReadOnly,
                    AppStorage::for_app(PageApp::ID),
                    crate::framework::settings::Hosted::default(),
                ),
                doc: Some(frozen),
                bounds: Rect::ZERO,
                view: PreviewView::default(),
            };
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            preview
                .layout(Rect::new(10.0, 20.0, 410.0, 320.0), &mut text, &theme)
                .unwrap();
            let mut scene = Scene::new();
            preview.draw(&mut scene, Affine::IDENTITY, &mut text, &theme);
            assert_eq!(shown(&preview.root), ["First draft"]);
            assert!(preview.root.get_rect().width() > 0.0);
        }
    }

    fn shown(root: &ResourceRoot<Page>) -> Vec<String> {
        fn visit(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(node) = widget.accessibility()
                && let Some(label) = node.name
            {
                out.push(label);
            }
            widget.for_each_child(&mut |child| visit(child, out));
        }
        let mut out = Vec::new();
        visit(root, &mut out);
        out
    }
}
