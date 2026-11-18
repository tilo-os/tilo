//! The notes app on the framework: the [`Notebook`] resource, the parts its
//! notes are kept in ([`NoteList`]), its window and what launching it on a
//! Markdown file does.

use crate::metadata::CATALOG;
use ui::application::OpenFile;
use ui::framework::{LaunchInput, ResourceApp, ViewCx};
#[cfg(feature = "samples")]
use ui::headless::FrameHarness;
use ui::permissions::MemberId;
#[cfg(feature = "samples")]
use ui::tilo_resource::Invoke;

use crate::app::NotesApp;
use crate::effect::NotesEffect;
use crate::model::PageId;
use crate::note_list::{NoteList, note_list};
pub use crate::notebook::Notebook;
use ui::widget::Element;

/// Framework app: the notes widget is the window over [`Notebook`].
pub struct NotesShell;

impl ResourceApp for NotesShell {
    type Resource = Notebook;
    const ID: &'static str = ui::apps::NOTES;
    const TITLE: &'static str = "Notes";
    const TITLE_KEY: &'static str = "notes-app-title";
    const ICON: Option<&'static str> = Some("notes");
    const OPENS: &'static [&'static str] = &["text/markdown", "text/x-markdown"];
    const ARGUMENTS: usize = 1;
    /// One notebook per workspace, not one per window. A notebook is a
    /// collection every window of Notes in the workspace shows (the sidebar is
    /// its pages), so a new window, or Notes reopened after its windows
    /// closed, must show the pages already written there: with a resource
    /// per window each new window started on an empty notebook, and the
    /// pages came back only by restoring the very window that wrote them.
    /// A workspace is the unit of collaboration, so its members share the
    /// notebook too.
    const WORKSPACE_RESOURCE: bool = true;

    /// The notes, kept in parts beside the notebook.
    fn resources(resources: &mut ui::framework::Resources<'_>) {
        resources.chain::<NoteList>();
    }

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        let author = cx
            .me()
            .cloned()
            .unwrap_or_else(|| MemberId::person("local"));
        // A note's save status sits in its header row beside the title, on
        // the note's own save status surface, so the window paints none.
        let mut app = NotesApp::empty_surface(&author, cx.save_status());
        let chain = cx.chain::<NoteList>();
        let notes = chain.and_then(|chain| {
            let actions = cx.resource_actions(&chain)?;
            Some(crate::app::Notes { chain, actions })
        });
        if let Some(notes) = &notes {
            import_launched(&cx.launch(), &notes.chain);
            #[cfg(feature = "samples")]
            if let Some(scene) = cx.fixture::<crate::scenario::Scene>() {
                crate::scenario::fill(&scene, &notes.chain);
            }
        }
        if let Some((page, _)) = launched_note(&cx.launch()) {
            app.open_when_present(page);
        }
        // A search hit, or an agent's `open`, shows the note it names, by
        // the address the part keeping it publishes it under.
        let mut open = app.opener();
        cx.on_open(move |anchor: &str| {
            let (_, page) = ui::tilo_resource::items::item_of_resource::<NoteList>(anchor)
                .ok_or_else(|| ui::tilo_resource::Refusal::new("there is no such note"))?;
            open(&page)
        });
        app.wake_asset_reads_through(cx.waker());
        app.attach_framework(cx.dispatcher(), cx.actions(), cx.live().clone(), notes);
        app.attach_people(cx.people());
        // The window shows the notebook as it stands on its first sync.
        app.follow(cx.live().clone());
        #[cfg(feature = "samples")]
        cx.on_stage(app.stager());
        // Effects answer at once from what the window shows: the editor's
        // caret, the note open, its way back and forward.
        let steps = app.steps();
        cx.window_effects(|notes: &mut NotesApp, effect: NotesEffect, cx| {
            notes.answer_effect(effect, cx)
        })
        // Back and forward are offered only where they lead.
        .status(move || crate::app::navigation_statuses(steps.get()));
        Box::new(app)
    }

    /// Pages a migration brought from Outline join the notebook when it opens.
    fn import_resource(context: &ui::application::ProcessContext) -> Result<(), String> {
        crate::import_inbox::import(context)
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        crate::scenario::SCENARIOS
    }

    #[cfg(feature = "samples")]
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            // The drawer only exists below the narrow breakpoint.
            Some("drawer") => (430, 860),
            _ => crate::scenario::VIEWPORT,
        }
    }

    /// The scenario's tags; its notes are written into the parts as the
    /// window opens ([`crate::scenario::fill`]).
    #[cfg(feature = "samples")]
    fn scenario(name: Option<&str>) -> Vec<Invoke<Notebook>> {
        crate::scenario::tag_actions(name)
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut ui::framework::Fixture<Notebook>) {
        let scene = crate::scenario::Scene(fixture.scenario().map(str::to_owned));
        fixture.provide(scene);
    }

    #[cfg(feature = "samples")]
    fn scenario_input(name: Option<&str>, input: &mut FrameHarness) -> Result<(), String> {
        crate::scenario::input(name, input)
    }
}

/// Import the Markdown file the app is launched on as a note, kept in the
/// notes' `chain`; a file imported before opens the note it became.
fn import_launched(input: &LaunchInput<'_>, chain: &ui::framework::Chain<NoteList>) {
    let Some((page, file)) = launched_note(input) else {
        return;
    };
    let Ok(markdown) = std::str::from_utf8(file.bytes()) else {
        tracing::warn!(
            file = file.name(),
            "notes: the launched file is not UTF-8 Markdown"
        );
        return;
    };
    let note = crate::notebook::page_id(page);
    if chain
        .parts()
        .iter()
        .any(|(_, part)| part.pages.contains(&note))
    {
        return;
    }
    let title = std::path::Path::new(file.name())
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned();
    // Images next to a file named on the command line come with it.
    let (markdown, assets) = input.argument().map_or_else(
        || (markdown.to_owned(), Vec::new()),
        |path| crate::app::adopt_canonical_markdown_assets(std::path::Path::new(path), markdown),
    );
    let imported = chain.invoke(
        &NoteList::IMPORT_MARKDOWN.with(note_list::ImportMarkdown {
            title,
            markdown,
            images: assets
                .into_iter()
                .map(|(_, bytes)| ui::tilo_resource::Bytes::to_keep(bytes))
                .collect(),
            note: Some(note),
            path: None,
            origin: None,
            reads: None,
        }),
    );
    if let Err(error) = imported {
        tracing::warn!(%error, "notes: the launched file was not imported");
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

ui::reach_tests!(NotesShell, shares: [share_0, share_1, share_2, share_3]);
