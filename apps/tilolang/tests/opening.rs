//! A window of a workspace this device keeps shows the workspace before its record is read: it opens over
//! the workspace's index, takes the record in once it is decoded off the frame, and goes on as the
//! window of the record, with nothing the person sees built again.

use serde_json::Map;
use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;
use ui::headless::InspectionNode;

const NOTES: &str = "so.tilo.lang.notes";
/// The workspace `--data-dir` opens.
const WORKSPACE: &str = "personal";

/// A directory of its own, removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tilolang-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Everything the window says, as a screen reader hears it.
fn said(app: &mut HostedApp) -> Vec<String> {
    fn walk(node: &InspectionNode, out: &mut Vec<String>) {
        if let Some(name) = node
            .semantics
            .as_ref()
            .and_then(|access| access.name.clone())
        {
            out.push(name);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    app.harness().relayout();
    let mut out = Vec::new();
    walk(&app.harness().inspection().root, &mut out);
    out
}

fn settle(app: &mut HostedApp) {
    for _ in 0..12 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

/// A profile in `dir` whose workspace holds a notebook of `n` notes, as an earlier session left it.
fn kept_notebook(dir: &std::path::Path, n: usize) {
    let notes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tl/notes");
    let packages = tilolang_tilo::bench::compile(&notes).unwrap();
    let mut doc = tilolang_tilo::bench::seeded_record(&packages, n).unwrap();
    let profile = ui::application::NativeProfile::open(dir, None).unwrap();
    let landing =
        ui::record::transfer::install_room_document(profile.store(), NOTES, WORKSPACE, &mut doc)
            .unwrap();
    profile.store().flush(None).unwrap();
    assert!(matches!(
        landing.durability(&**profile.store()),
        ui::record::transfer::Durability::Durable
    ));
}

/// The published resource `id` among `resources` and the ones under them.
fn find<'a>(
    resources: &'a [ui::tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a ui::tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        (resource.id.as_str() == id)
            .then_some(resource)
            .or_else(|| find(&resource.children, id))
    })
}

#[test]
fn a_kept_workspace_shows_before_its_record_and_goes_on_as_the_window_of_the_record() {
    let definition = crate::support::installed("notes");
    let scratch = Scratch::new("kept-workspace");
    kept_notebook(&scratch.0, 200);
    // The first window builds the workspace's index off the frame, and keeps it.
    {
        let mut app = HostedApp::open_data_dir(definition, "ada", &scratch.0).unwrap();
        settle(&mut app);
        assert!(said(&mut app).iter().any(|said| said.contains("Item 0")));
    }
    // The next one shows the notes on its first frame, and the record once it is had.
    let mut app = HostedApp::open_data_dir(definition, "ada", &scratch.0).unwrap();
    let first = said(&mut app);
    assert!(
        first.iter().any(|said| said.contains("Item 0")),
        "the first frame shows the notebook: {first:?}"
    );
    settle(&mut app);
    assert!(said(&mut app).iter().any(|said| said.contains("Item 0")));
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let resources = app.harness().resources();
    let new_note = find(&resources, &notebook)
        .and_then(|resource| {
            resource
                .actions
                .iter()
                .find(|action| action.label == "New note")
        })
        .map(|action| action.id.clone())
        .expect("the notebook publishes New note");
    app.invoke(None, None, &notebook, &new_note, &Map::new(), false)
        .unwrap();
    settle(&mut app);
    let notes = app.read(|doc| {
        tilolang_tilo::action::read_resource(doc, NOTES, &root).map_or(0, |record| {
            record
                .schema()
                .fields
                .iter()
                .filter_map(|field| match record.get(&field.id) {
                    Some(tilolang_tilo::DynValue::Collection(items)) => Some(items.len()),
                    _ => None,
                })
                .sum::<usize>()
        })
    });
    assert_eq!(notes, 201, "the window of the record changes the record");
}
