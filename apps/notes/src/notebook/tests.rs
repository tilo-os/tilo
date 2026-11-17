//! The notebook on its own: the tags its notes carry.

use super::*;
use ui::tilo_resource::{Edit, Invoke, Replica};

// Count the owned editor blocks made on this test's thread, excluding setup
// and assertions. A rename should inspect the already-read shared paragraphs.
thread_local! {
    pub(crate) static MATERIALIZED_BLOCKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn edit(notebook: &mut Replica<Notebook>, edit: &Edit<Notebook>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    notebook
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

#[test]
fn tags_are_named_items_and_one_per_name() {
    let mut notebook = Replica::<Notebook>::new();
    notebook
        .apply(Notebook::NEW_TAG.with(notebook::NewTag {
            name: " Release ".into(),
        }))
        .unwrap();
    let release = notebook.state().unwrap().tag_named("Release").unwrap();
    assert!(
        notebook
            .apply(Notebook::NEW_TAG.with(notebook::NewTag {
                name: "Release".into()
            }))
            .is_err(),
        "one tag per name"
    );
    assert!(
        edit(
            &mut notebook,
            &Notebook::paths()
                .tags
                .item(&release)
                .name
                .set(Atom("  ".into()))
        )
        .is_err(),
        "a tag has a name"
    );
    edit(
        &mut notebook,
        &Notebook::paths()
            .tags
            .item(&release)
            .name
            .set(Atom("Shipped".into())),
    )
    .unwrap();
    assert_eq!(
        notebook.state().unwrap().tag_named("Shipped"),
        Some(release.clone())
    );
    notebook
        .apply_confirmed(Notebook::DELETE_TAG.with(release.clone()))
        .unwrap();
    assert!(notebook.state().unwrap().tags.get(&release).is_none());
}

#[test]
fn the_notebook_reads_as_its_tags() {
    let mut notebook = Replica::<Notebook>::new();
    notebook
        .apply(Notebook::NEW_TAG.with(notebook::NewTag {
            name: "Work".into(),
        }))
        .unwrap();
    let state = notebook.state().unwrap();
    let read =
        <Notebook as tilo_resource::Resource>::read_content(&state, notebook.doc(), None).unwrap();
    assert_eq!(read["tags"][0]["name"], "Work");
    let published = notebook.published().unwrap();
    for verb in ["new-tag", "delete-tag"] {
        assert!(
            published
                .actions
                .iter()
                .chain(published.children.iter().flat_map(|child| &child.actions))
                .any(|spec| spec.id == verb),
            "the notebook offers {verb}"
        );
    }
}
