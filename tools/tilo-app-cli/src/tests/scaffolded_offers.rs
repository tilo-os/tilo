//! What `tilo-app new` writes for the offers its template's code publishes: the manifest
//! `tilo-app surface` would write, so a freshly scaffolded app checks clean before it is built.

use super::*;

#[test]
fn a_scaffolded_manifest_carries_the_offers_its_code_publishes() {
    // The manifest `tilo-app surface` writes for a freshly scaffolded app: the offers the
    // template's view publishes, in the words the app answers with while it is closed.
    // Written here so a new app checks clean before it is built.
    for template in Template::ALL {
        let spec = AppSpec {
            package_name: "notes".into(),
            crate_name: "notes".into(),
            title: "Notes".into(),
            app_id: "so.tilo.notes".into(),
            sdk: AppSdk::Workspace {
                ui_path: PathBuf::from("../../crates/ui/ui-core"),
            },
            template,
        };
        let manifest = app_manifest(&spec).unwrap();
        let mut offer = serde_json::Map::new();
        let (resource, action, label) = match template {
            Template::Starter => ("so.tilo.notes.greeting", "clear", "Forget the name"),
            Template::Document => ("so.tilo.notes.document", "clear", "Erase the document"),
            Template::Collection => ("so.tilo.notes.items", "add", "Add an item"),
            Template::Graph => ("so.tilo.notes.graph-project", "create", "Create a project"),
        };
        offer.insert("resource".into(), resource.into());
        offer.insert("action".into(), action.into());
        offer.insert("label".into(), label.into());
        offer.insert("kind".into(), "undoable".into());
        offer.insert("operation".into(), "document".into());
        if template == Template::Collection {
            offer.insert("asks".into(), "title".into());
        }
        let offers = vec![serde_json::Value::Object(offer)];
        assert_eq!(
            crate::surface::with_offers(&manifest, &offers),
            manifest,
            "{} writes the offers its code publishes",
            template.name()
        );
        assert!(
            manifest.contains(&format!("[resources]\nkinds = [\"{resource}\"]\n")),
            "{} names the record its code declares: {manifest}",
            template.name()
        );
    }
}
