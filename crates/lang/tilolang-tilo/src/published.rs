//! The verbs a session's resources publish that no loaded package declares, as a shell's runtime
//! reads them ([`tilolang_runtime::published`]).

use std::collections::BTreeMap;

use tilo_actions::ResourceSpec;
use tilolang_runtime::published::{Published, PublishedAction};

/// The actions `page` — a catalogue of the resources the apps a window shows publish — says
/// cannot run now, each with its reason, by the key a view's control binds it by
/// ([`tilolang_runtime::unavailable`]): the address the catalogue lists the action at
/// (`sound#refresh`), the address a view names the resource by (`tl:so.tilo.device:sound#refresh`),
/// and the id the app's package names the action by (`Sound/refresh`, and
/// `so.tilo.device#Sound/refresh`), which a view's call compiles to. A window's controls bound to
/// one draw disabled, with the reason, without the window holding any resource of that app.
#[must_use]
pub fn unavailable_from_catalogue(
    page: &tilo_actions::remote::CataloguePage,
) -> tilolang_runtime::Unavailable {
    let mut out = tilolang_runtime::Unavailable::default();
    for row in &page.rows {
        let Some(action) = &row.action else {
            continue;
        };
        let tilo_actions::Availability::Unavailable { reason, .. } = &action.contract.availability
        else {
            continue;
        };
        let verb = action.address.action.as_str();
        out.insert(
            tilolang_runtime::unavailable::address_key(&action.address.resource, verb),
            reason,
        );
        // A resource of another app is named in a view by the address it is published at
        // (`tl:so.tilo.device:sound`), and the verb applied to it by the resource's id there.
        out.insert(
            tilolang_runtime::unavailable::address_key(
                &crate::naming::resource_id(&row.app_id, row.resource.id.as_str()),
                verb,
            ),
            reason,
        );
        // An action of a package a window loads as a description is also bound by the id its
        // catalogue entry names: the resource's type and the verb, and that type as the
        // publishing app's package writes it.
        let ty = crate::catalogue::kind_type_name(&row.resource.kind);
        out.insert(format!("{ty}/{verb}"), reason);
        out.insert(format!("{}#{ty}/{verb}", row.app_id), reason);
    }
    out
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ActionSpec, Availability, EncodedAction, OperationKind};

    /// What the catalogue says cannot run is held by every key a view's control may be bound by:
    /// the address a resource publishes the verb at, the address another app's views name the
    /// resource by, and the id the app's package names the action by.
    #[test]
    fn the_catalogues_unavailable_actions_are_held_by_their_address_and_their_package_id() {
        let mut action = ActionSpec::typed_operation(
            "volume-up",
            "Volume up",
            "Turn the volume up",
            OperationKind::Effect,
            "so.tilo.device",
            Vec::new(),
            |_| {
                EncodedAction::encode_operation("so.tilo.device", &serde_json::json!({}))
                    .expect("an encoded call")
            },
        );
        action.type_mut().contract.availability = Availability::Unavailable {
            code: "unavailable".to_owned(),
            reason: "the sound service is restarting".to_owned(),
        };
        let spec = ResourceSpec::new("sound", "sound", "Sound").with_action(action);
        let page = tilo_actions::remote::CataloguePage::published(
            "so.tilo.device",
            &[spec],
            &tilo_actions::remote::CatalogueQuery::default(),
        );
        let map = unavailable_from_catalogue(&page);
        for key in [
            "sound#volume-up",
            "tl:so.tilo.device:sound#volume-up",
            "Sound/volume-up",
            "so.tilo.device#Sound/volume-up",
        ] {
            assert_eq!(
                map.get(key),
                Some("the sound service is restarting"),
                "{key} names the action"
            );
        }
    }
}
