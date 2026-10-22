//! Settled pictures for callers, rendered away from the window's thread.
//! Each window keeps its own renderer: repeated looks reuse decoded pictures
//! and paint simulations without borrowing the live viewport's caches.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_content::ContentStore;
use ui::TextRenderer;

use crate::model::Artwork;
use crate::render::Renderer;
use crate::resource::ViewOp;
use crate::view_ops::{self, Picture};
use ui::lock::MutexRecover as _;

#[derive(Clone, Default)]
pub struct Pictures {
    renderer: Arc<Mutex<Option<Worker>>>,
}

struct Worker {
    renderer: Renderer,
    text: TextRenderer,
}

impl Pictures {
    /// Decode and render the finder input on the worker, retaining the
    /// originating window's content store for its eventual matte.
    ///
    /// # Errors
    /// Returns why the selection request or its picture cannot be prepared.
    pub fn prepare(
        &self,
        art: &Artwork,
        content: ContentStore,
        select: &crate::resource::FindArea,
    ) -> Result<crate::subject::Prepared, String> {
        let mut cached = self.renderer.lock_recover();
        let worker = cached.get_or_insert_with(|| Worker {
            renderer: Renderer::default(),
            text: TextRenderer::new(),
        });
        worker.renderer.pictures.read_from(content.clone());
        let prepared = crate::subject::prepare_in(
            select,
            &mut worker.renderer,
            &mut worker.text,
            art,
            content,
        );
        drop(cached);
        prepared
    }

    /// Render the invocation's snapshot, using its window's content store.
    /// File writes stay with the completion, where the host's grants apply.
    pub fn render(
        &self,
        art: &Artwork,
        content: ContentStore,
        op: &ViewOp,
    ) -> Result<(Picture, Value), String> {
        let mut cached = self.renderer.lock_recover();
        let worker = cached.get_or_insert_with(|| Worker {
            renderer: Renderer::default(),
            text: TextRenderer::new(),
        });
        worker.renderer.pictures.read_from(content);
        let result = match op {
            ViewOp::Look { look } => {
                view_ops::look(&mut worker.renderer, &mut worker.text, art, look)
            }
            ViewOp::Export { export } => {
                let mut export = export.clone();
                // Resolve a path's format before removing the file write.
                if export.format.is_none() {
                    export.format = export.path.as_deref().and_then(|path| {
                        std::path::Path::new(path)
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .map(str::to_ascii_lowercase)
                    });
                }
                export.path = None;
                view_ops::export(&mut worker.renderer, &mut worker.text, art, &export)
            }
            _ => Err("this operation does not render a picture".into()),
        };
        drop(cached);
        result
    }
}
