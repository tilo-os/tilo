//! The notes app on the framework: the [`Notebook`] record, its window and
//! what launching it on a Markdown file does.

use std::rc::Rc;

use ui::application::OpenFile;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, LaunchInput, ResourceApp, ResourceSurface, SurfaceCx,
    ViewCx,
};
use ui::headless::FrameHarness;
use ui::permissions::UserId;
use ui::tilo_resource::Invoke;

use crate::app::NotesApp;
use crate::effect::NotesEffect;
use crate::model::PageId;
pub use crate::notebook::Notebook;
use ui::widget::Element;

/// Framework app: the notes widget is the surface over [`Notebook`].
pub struct NotesShell;

impl ResourceApp for NotesShell {
    type Resource = Notebook;
    const ID: &'static str = "so.tilo.notes";
    const TITLE: &'static str = "Notes";
    const ICON: Option<&'static str> = Some("notes");
    const OPENS: &'static [&'static str] = &["text/markdown", "text/x-markdown"];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        // `present` installs the framework's workspace before the first frame.
        let author = UserId(cx.me().unwrap_or("local").to_owned());
        let mut app = NotesApp::empty_surface(&author, cx.save_status());
        if let Some((page, _)) = launched_note(&cx.launch()) {
            app.open_when_present(page);
        }
        // A search hit, or an agent's `open`, shows the note it names.
        cx.on_open_item(app.opener());
        cx.surface(app)
    }

    /// A Markdown file the app is launched on becomes a note.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Notebook>> {
        let Some((page, file)) = launched_note(input) else {
            return Vec::new();
        };
        let Ok(markdown) = std::str::from_utf8(file.bytes()) else {
            tracing::warn!(
                file = file.name(),
                "notes: the launched file is not UTF-8 Markdown"
            );
            return Vec::new();
        };
        let title = std::path::Path::new(file.name())
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("Imported note");
        // Images next to a file named on the command line come with it.
        let (markdown, assets) = input.argument().map_or_else(
            || (markdown.to_owned(), Vec::new()),
            |path| {
                crate::app::adopt_canonical_markdown_assets(std::path::Path::new(path), markdown)
            },
        );
        vec![
            Notebook::IMPORT_MARKDOWN.with(crate::notebook::notebook::ImportMarkdown {
                title: title.to_owned(),
                markdown,
                images: assets.into_iter().map(|(_, bytes)| bytes).collect(),
                note: Some(crate::notebook::page_id(page)),
            }),
        ]
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::metadata::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenario::SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            // The drawer only exists below the narrow breakpoint.
            Some("drawer") => (430, 860),
            _ => crate::scenario::VIEWPORT,
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Notebook>> {
        crate::scenario::actions(name)
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn scenario_input(name: Option<&str>, input: &mut FrameHarness) -> Result<(), String> {
        crate::scenario::input(name, input)
    }
}

/// The note a launch on a Markdown file imports. The same file always names
/// the same note, so launching on it again opens the note it became rather
/// than importing it twice.
fn launched_note<'a>(input: &LaunchInput<'a>) -> Option<(PageId, &'a OpenFile)> {
    let file = input.file()?;
    let digest = blake3::Hasher::new()
        .update(file.name().as_bytes())
        .update(&[0])
        .update(file.bytes())
        .finalize();
    let bytes = *digest.as_bytes().first_chunk::<16>()?;
    Some((uuid::Builder::from_random_bytes(bytes).into_uuid(), file))
}

impl ResourceSurface<Notebook, NotesEffect> for NotesApp {
    fn take_save_retry(&mut self) -> bool {
        self.take_save_retry_request()
    }

    fn effect(&mut self, effect: NotesEffect, cx: &mut EffectCx<Notebook, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    /// Back and forward are offered only where they lead.
    fn effects(&self) -> Vec<EffectStatus> {
        self.navigation_statuses()
    }

    fn present(&mut self, state: &Rc<Notebook>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Notebook>) {
        self.attach_framework(cx.dispatcher(), cx.actions());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}
