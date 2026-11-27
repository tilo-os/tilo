//! What an app publishes, as `tilolang surface` commits it and
//! `tilolang check` compares it: the resource's surface, the person's own resources
//! and those the views open beside it, the controls its view binds, and the
//! ids of its effects and named views.

use std::rc::Rc;

use tilo_permissions::Role;
use tilo_resource::Resource;
use ui_core::application::Capture;
use ui_core::headless::Driver;

use super::super::{PersonKind, controls, reserved_action_ids};
use super::{
    AUTHOR, Kept, Launch, Opening, Profile, ResourceApp, ResourceRoot, TEST_DEVICE, TEST_WORKSPACE,
    open_app,
};

/// The longest a document the view opened may take to arrive in the report's own workspace,
/// which holds every one it is asked for.
const OPENS_WITHIN: ui_core::time::Duration = ui_core::time::Duration::from_secs(15);

/// What `A` publishes and how its view binds to it, as `tilolang surface`
/// commits it and `tilolang check` compares it: the resource's
/// [`Surface`](tilo_resource::Surface), those of the person's own resources it
/// declares ([`ResourceApp::PERSON_RESOURCES`]) and those of the resources its
/// views open beside its own (the parts of its chains, the settings
/// sections), every control its view, built headless on an empty document,
/// binds to something the resource does not publish, the ids of its effects
/// and named views, and the ids the framework keeps for its own actions
/// ([`reserved_action_ids`](super::reserved_action_ids)).
#[must_use]
pub fn surface_report<A: ResourceApp>() -> serde_json::Value {
    let mut report = report_surface(
        A::ID,
        &Opening::of::<A>(),
        <A::Resource as Resource>::surface(),
        A::PERSON_RESOURCES,
        &published_ids::<A>,
    );
    for other in A::OTHER_WINDOWS {
        other.merge_into(&mut report);
    }
    report
}

/// What a built view publishes beside its resource's verbs: its effects and
/// the resource's named views, and what the effects declare, which its surface
/// publishes as actions of the resource.
#[derive(Default)]
struct PublishedActions {
    /// The ids of every effect, views included.
    effects: Vec<String>,
    /// The ids of the resource's named views.
    views: Vec<String>,
    /// Every read effect's declaration: what its surface publishes it as.
    rows: Vec<tilo_resource::effect::EffectRow>,
}

/// [`PublishedActions`] of `A`'s root `driver` hosts.
fn published_ids<A: ResourceApp>(driver: &mut Driver) -> PublishedActions {
    driver
        .with_widget_mut(|root: &mut ResourceRoot| {
            let typed = root.typed::<A::Resource>()?;
            let views = <A::Resource as Resource>::views(&typed.live.get());
            Some(PublishedActions {
                effects: typed
                    .effects
                    .specs()
                    .iter()
                    .map(|spec| spec.id.clone())
                    .collect(),
                views: views.into_iter().map(|view| view.id).collect(),
                rows: typed.effects.rows(),
            })
        })
        .flatten()
        .unwrap_or_default()
}

/// [`surface_report`] of the app `opening` describes, whose resource's
/// surface is `surface` and whose person resources are `person`; `published`
/// reads the ids of the effects and the named views a built view publishes.
fn report_surface(
    app: &'static str,
    opening: &Opening,
    mut surface: tilo_resource::Surface,
    person: &[PersonKind],
    published: &dyn Fn(&mut Driver) -> PublishedActions,
) -> serde_json::Value {
    let collecting = collect_unbound();
    let profile = Kept::fresh();
    let built = profile.and_then(|profile| {
        open_app(
            opening,
            Launch {
                profile: Profile::Temporary(
                    profile,
                    TEST_WORKSPACE.to_owned(),
                    TEST_DEVICE.to_owned(),
                ),
                role: Role::Owner,
                author: AUTHOR,
                arguments: &[],
                file: None,
                handoff: None,
                // The report is what the code publishes, so the view is built as a
                // capture: its scene comes from the app's fixtures, and nothing it
                // starts — a worker, a fetch, a clock — reaches outside the process.
                // A surface is written from source, not from what a window that sits
                // in a workspace happens to open.
                capture: Some((Capture::new(None), (opening.shot_viewport)(None))),
            },
            None,
            &mut |_| {},
        )
    });
    // What the view publishes besides the resource's verbs: its effects and
    // the resource's named views, whose ids must not be the framework's. The
    // app's read effects are actions of its own resource and are published
    // with it, as a verb of the surface.
    let mut declared = PublishedActions::default();
    let mut view = None;
    let mut offers = Vec::new();
    match built {
        Ok(mut opened) => {
            // What the view asked for while it was on its way (the workspace had not delivered
            // it, or its store was in use) is waited for as a window waits for it: opened as it
            // arrives, in a frame, and published from then on. A report written before would
            // differ from one written after.
            let storage = opened
                .driver
                .with_widget_mut(|root: &mut ResourceRoot| Rc::clone(&root.storage));
            let arrived = opened.driver.wait_until(OPENS_WITHIN, |_| {
                !storage.as_ref().is_some_and(|storage| storage.opening())
            });
            if !arrived {
                view = Some(format!(
                    "what the view opened did not arrive within {OPENS_WITHIN:?}"
                ));
            }
            declared = published(&mut opened.driver);
            offers = offers_of(&opened.driver.resources());
        }
        Err(error) => view = Some(error.to_string()),
    }
    add_effect_verbs(&mut surface, &declared.rows);
    let PublishedActions { effects, views, .. } = declared;
    let (unbound, opened) = collecting.finish();
    // The app that owns each resource: this app for its own records, and the app
    // keeping it for a shared record of another, which the surface then lists as
    // one of its `reads` rather than as a resource of its own.
    let mut owners: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    owners.insert(surface.kind.clone(), app.to_owned());
    let opened: Vec<tilo_resource::Surface> = opened
        .into_iter()
        .map(|(owner, surface)| {
            owners
                .entry(surface.kind.clone())
                .or_insert_with(|| owner.unwrap_or(app).to_owned());
            surface
        })
        .collect();
    let mut resources = vec![surface];
    resources.extend(opened);
    // The person's own resources the app declares are part of its surface whether
    // or not a view of it was showing: the host admits the app's changes to
    // them, and its reads of them, against this.
    for declared in person {
        owners.insert(
            declared.kind().to_owned(),
            declared.owner().unwrap_or(app).to_owned(),
        );
        match resources
            .iter_mut()
            .find(|resource| resource.kind == declared.kind())
        {
            Some(resource) => resource.person = declared.surface().person,
            None => resources.push(declared.surface()),
        }
    }
    serde_json::json!({
        "owners": owners,
        "resources": resources,
        "unbound": unbound,
        "effects": effects,
        "views": views,
        "reserved": reserved_action_ids(),
        "view": view,
        "offers": offers,
    })
}

/// What the app offers a person while it is closed, as its manifest's
/// `[[offers]]` declare it: every action of its own its view publishes on an
/// empty document that reaches people with nothing chosen, and takes nothing
/// or one line a person writes. A palette finds these by their words, and
/// choosing one opens the app and carries it out there.
///
/// Left out: what the framework publishes on every app (its undo, its
/// `open`), save the link a scheme the app takes is opened with; what the
/// shell publishes into the view (its search, its controls); the person's own
/// documents and those the workspace keeps for all its apps; the items of a
/// list, which a closed app has none of — an item is a resource the app's own
/// resources hold, and what it holds is the window's; what acts
/// on one document (`…@…`), whose address names the document a window opens, so
/// an offer of it would hold that window's id; and Settings' pages and
/// settings, save its link, which the shell's own search finds on every
/// desktop whether Settings is open or not.
fn offers_of(resources: &[tilo_actions::ResourceSpec]) -> Vec<serde_json::Value> {
    use tilo_actions::{Collects, InputSpec};

    let reserved = reserved_action_ids();
    let mut offers = Vec::new();
    for action in tilo_actions::api::survey(resources) {
        let contract = &action.contract;
        let resource = action.address.resource.as_str();
        let id = action.address.action.as_str();
        let opens_links = id == collab_types::app_actions::OPEN_LINK;
        if !contract.offered_to_people()
            || !contract.availability.is_available()
            || (reserved.iter().any(|framework| framework == id) && !opens_links)
            // An address that names an item rather than its kind: the app's
            // own resources' ids are the kinds they publish, so one that is
            // not names a record a window opened, which a closed app has none
            // of.
            || action.resource_kind != resource
            || resource.starts_with("tilo.")
            || resource.starts_with("person:")
            || collab_types::workspace_document_of_kind(resource).is_some()
            || (resource.starts_with("settings.") && !opens_links)
            || resource.contains(['/', '@'])
        {
            continue;
        }
        // One line a person writes: never an id or a choice they would have to know.
        let asks = match &action.input {
            InputSpec::Text {
                collects: Collects::Words,
                ..
            } => action
                .prompt_param()
                .filter(|name| {
                    action.params.iter().any(|param| {
                        param.name == *name && (param.entry || param.choices.is_empty())
                    })
                })
                .map(str::to_owned),
            _ => None,
        };
        // Anything it needs beyond the one line it asks for is a choice the
        // app's own view makes: offered there, not while it is closed.
        if action
            .params
            .iter()
            .any(|param| param.required && Some(param.name.as_ref()) != asks.as_deref())
        {
            continue;
        }
        let address = action.address.resource.as_str();
        if offers.iter().any(|offer: &serde_json::Value| {
            offer["resource"] == address && offer["action"] == action.address.action.as_str()
        }) {
            continue;
        }
        let mut offer = serde_json::Map::new();
        offer.insert("resource".into(), address.into());
        offer.insert("action".into(), action.address.action.as_str().into());
        offer.insert("label".into(), serde_json::json!(action.action_label));
        offer.insert("kind".into(), serde_json::json!(contract.kind));
        offer.insert("operation".into(), action.kind.as_str().into());
        if action.resource_kind != address {
            offer.insert("resource-kind".into(), action.resource_kind.clone().into());
        }
        // What a press of it waits for, by the one rule every window and every language's
        // apps decide a press by: what leaves the system confirms, and a destructive change
        // undo takes back confirms unless Shift is held.
        if contract.confirms().by_default() {
            offer.insert("confirm".into(), true.into());
        }
        if let Some(asks) = asks {
            offer.insert("asks".into(), asks.into());
        }
        if !action.keywords.is_empty() {
            offer.insert("keywords".into(), serde_json::json!(action.keywords));
        }
        if let Some(icon) = action.icon {
            offer.insert("icon".into(), serde_json::json!(icon));
        }
        offers.push(serde_json::Value::Object(offer));
    }
    offers
}

/// Publish the app's read effects on `surface`: its resource's verbs take
/// them, each with its parameters and the value it answers with, and the
/// types and enums that value names join the surface's types.
/// Its view effects are what the surface says it shows.
fn add_effect_verbs(
    surface: &mut tilo_resource::Surface,
    rows: &[tilo_resource::effect::EffectRow],
) {
    use tilo_resource::surface::{FieldType, NamedType};

    let mut types = tilo_resource::surface::TypeSet::new();
    let mut verbs = tilo_resource::surface::effect_verbs(rows, &mut types);
    let declared: Vec<NamedType> = types.into_types();
    // A type the resource's fields already declare, named again by a value
    // an action answers with, keeps the one name: the surface declares it
    // once.
    let mut renames: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for named in &declared {
        let same = surface.types.iter().find(|known| {
            known.name != named.name && known.fields == named.fields && known.cases == named.cases
        });
        if let Some(known) = same {
            renames.insert(named.name.clone(), known.name.clone());
        }
    }
    let renamed_type = |ty: &FieldType| ty.renamed(&renames);
    for verb in &mut verbs {
        for param in &mut verb.params {
            param.ty = renamed_type(&param.ty);
        }
        if let Some(returns) = &mut verb.returns {
            returns.ty = renamed_type(&returns.ty);
        }
    }
    let kept: Vec<NamedType> = declared
        .into_iter()
        .filter(|named| !renames.contains_key(&named.name))
        .map(|mut named| {
            for field in &mut named.fields {
                field.ty = renamed_type(&field.ty);
            }
            for case in &mut named.cases {
                for field in &mut case.fields {
                    field.ty = renamed_type(&field.ty);
                }
            }
            named
        })
        .filter(|named| !surface.types.iter().any(|known| known.name == named.name))
        .collect();
    surface.types.extend(kept);
    surface.verbs.extend(verbs);
    surface.shows = tilo_resource::surface::view_effects(rows);
}

/// Collects the controls bound to something the resource does not publish
/// until [`Self::finish`], instead of failing on the first.
struct Collecting(());

/// Start collecting unbound controls on this thread.
fn collect_unbound() -> Collecting {
    controls::UNBOUND.with(|unbound| *unbound.borrow_mut() = Some(Vec::new()));
    controls::OPENED.with(|opened| *opened.borrow_mut() = Some(Vec::new()));
    Collecting(())
}

impl Collecting {
    /// What was collected: one sentence per unbound control, and the
    /// surfaces of the other documents the views opened.
    fn finish(
        self,
    ) -> (
        Vec<String>,
        Vec<(Option<&'static str>, tilo_resource::Surface)>,
    ) {
        let unbound =
            controls::UNBOUND.with(|unbound| unbound.borrow_mut().take().unwrap_or_default());
        let opened = controls::OPENED.with(|opened| opened.borrow_mut().take().unwrap_or_default());
        drop(self);
        (unbound, opened)
    }
}

impl Drop for Collecting {
    fn drop(&mut self) {
        controls::UNBOUND.with(|unbound| *unbound.borrow_mut() = None);
        controls::OPENED.with(|opened| *opened.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::{
        ActionContract, ActionKind, ActionSpec, ConfirmationPolicy, EncodedAction, ExecutionLocus,
        LEAVES_TAG, ResourceSpec, RiskLevel,
    };

    /// An action of the closed app's own resource with `contract`.
    fn action(id: &str, contract: ActionContract) -> ActionSpec {
        ActionSpec::immediate_encoded(
            id,
            id,
            ExecutionLocus::ClientLocal,
            EncodedAction::written("notes.note".to_owned(), Vec::new()),
        )
        .with_contract(contract)
    }

    /// A closed app's offer confirms by what a press of it waits for, as a tilolang app's does:
    /// what leaves the system and an irreversible deletion confirm; an outside change the service
    /// takes back, and a call only an agent's grants decide, act on the press.
    #[test]
    fn an_offer_confirms_by_what_its_press_waits_for() {
        let contract = |kind, risk, tags: &[&str], confirmation| ActionContract {
            kind,
            risk,
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            confirmation,
            ..ActionContract::default()
        };
        let resources = vec![
            ResourceSpec::new("notes.note", "notes.note", "Notes")
                .with_action(action(
                    "post",
                    contract(
                        ActionKind::Outside,
                        RiskLevel::default(),
                        &[LEAVES_TAG],
                        ConfirmationPolicy::None,
                    ),
                ))
                .with_action(action(
                    "purge",
                    contract(
                        ActionKind::Outside,
                        RiskLevel::Destructive,
                        &[],
                        ConfirmationPolicy::Confirm,
                    ),
                ))
                .with_action(action(
                    "mark-read",
                    contract(
                        ActionKind::Outside,
                        RiskLevel::default(),
                        &[],
                        ConfirmationPolicy::None,
                    ),
                ))
                .with_action(action(
                    "rename",
                    contract(
                        ActionKind::Undoable,
                        RiskLevel::default(),
                        &[],
                        ConfirmationPolicy::Confirm,
                    ),
                )),
        ];
        let offers = super::offers_of(&resources);
        let confirms = |id: &str| {
            offers
                .iter()
                .find(|offer| offer["action"] == id)
                .unwrap_or_else(|| panic!("the closed app offers {id}: {offers:?}"))
                .get("confirm")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        assert!(confirms("post"), "what leaves the system confirms");
        assert!(confirms("purge"), "an irreversible deletion confirms");
        assert!(
            !confirms("mark-read"),
            "what the service takes back acts on the press"
        );
        assert!(
            !confirms("rename"),
            "a change nothing destroys acts on the press"
        );
    }
}
