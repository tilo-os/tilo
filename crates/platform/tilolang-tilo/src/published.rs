//! The verbs a session's resources publish that no loaded package declares, as a shell's runtime
//! reads them ([`tilolang_runtime::published`]).

use std::collections::BTreeMap;

use tilo_actions::ResourceSpec;
use tilolang_runtime::published::{Published, PublishedAction};

/// Every verb the resources of `resources` (the session's catalogue) publish that no package of
/// `loaded` (the apps whose package the shell runs here) declares: the resource's id, the verb's
/// id, the app that runs it, and the arguments it takes after the resource (`channel`, `text`).
/// A verb of an app whose package loads here is that package's declared action, which the runtime
/// resolves first, and a verb with no parameter tilolang code can give is left out.
#[must_use]
pub fn from_resources(resources: &[ResourceSpec], loaded: &BTreeMap<String, String>) -> Published {
    let mut published = Published::default();
    collect(resources, loaded, &mut published);
    published
}

fn collect(resources: &[ResourceSpec], loaded: &BTreeMap<String, String>, out: &mut Published) {
    for resource in resources {
        let Some(app) = resource.app() else {
            collect(&resource.children, loaded, out);
            continue;
        };
        // The app's package loading here declares its own actions already.
        if !loaded.contains_key(app) {
            for action in &resource.actions {
                out.insert(
                    resource.id.as_str(),
                    action.id.as_str(),
                    PublishedAction {
                        app: app.to_owned(),
                        params: action
                            .params()
                            .iter()
                            .map(|param| param.name.to_string())
                            .collect(),
                    },
                );
            }
        }
        collect(&resource.children, loaded, out);
    }
}
