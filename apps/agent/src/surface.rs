//! What the room's apps currently have open, and what may be done to it.
//!
//! An app on the Tilo action model publishes a [`Resource`] tree and the
//! operations each resource exports (`docs/action-model-recipe.md`), and
//! [`tilo_actions::door`] turns a live tree into inert, stably-addressed
//! descriptors the agent can perform. The agent queries that tree each time,
//! so it receives the operations the app currently publishes.
//!
//! ## What the agent supplies, and what it must not
//!
//! Nothing but the arguments it fills in. Everything comes from the app: the resource ids, the action ids, the
//! labels a person would see, whether an operation takes text, and the
//! encoded action itself. The agent never mints any of it.
//!
//! ## Addresses
//!
//! `"<resource id>#<action id>"`, the same spelling the shell's own action
//! surface uses (`shell::shell::search::providers::app_resources`), so one
//! address names one operation wherever it is built. Never a screen
//! position: the same address keeps meaning the same operation across
//! layouts, themes and sessions.
//!
//! ## Authority
//!
//! A denied operation is **absent** from [`offers`], not marked unavailable
//! in it. The question asked is the shell's own gate: `Write` on
//! `(app, action)`, resolved against the agent's capability set and denied
//! unless something explicitly permits. It runs again at dispatch, because a
//! survey is a snapshot and acting on a stale one must not happen.

use std::fmt::Write as _;

use automerge::AutoCommit;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_actions::{ActionContract, ActionKind, ActionParam, ActionRegistry, Resource};
use ui::permissions::{Answer, PermissionContext, PermissionScope, TreePath};
use ui::tilo_resource::ResourceKind;

/// Separator between a resource id and an action id in an address.
///
/// `#` because a resource id may contain `.`, `:` and `/`, while an action id
/// is a plain identifier and neither half ever contains `#`. Splitting on the
/// last occurrence is unambiguous.
pub const ADDRESS_SEP: char = '#';

/// The single-string form of an address.
pub fn encode_address(address: &ActionAddress) -> String {
    format!("{}{}{}", address.resource, ADDRESS_SEP, address.action)
}

/// Recover an address from [`encode_address`]. `None` when the string is not
/// one — an address a model made up is refused rather than looked up
/// against a half-parsed id.
pub fn decode_address(id: &str) -> Option<ActionAddress> {
    let id = if let Some(rest) = id.strip_prefix("broker:") {
        rest.split_once(':')?.1
    } else {
        id
    };
    let (resource, action) = id.rsplit_once(ADDRESS_SEP)?;
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some(ActionAddress::new(resource, action))
}

/// The app a resource id belongs to: the segment before the first `.`.
///
/// Resource ids are app-namespaced by convention (`word-processor.document`,
/// `files.entry:/…`), which lets a capability question name the app being
/// acted on without a second table of who owns what.
pub fn app_of(resource_id: &str) -> &str {
    resource_id
        .split_once('.')
        .map_or(resource_id, |(head, _)| head)
}

/// The action id an application exports so that it can be opened.
pub const OPEN_ACTION: &str = "open";

/// The tail of the resource id an application has *as an application* rather
/// than as a document.
pub const APP_RESOURCE: &str = "app";

/// The stable kind tag on that resource, shared across every app.
pub const APPLICATION_KIND: &str = "applications.app";

/// The address that opens the app `namespace` names.
///
/// Inside the app's own namespace, so [`app_of`] answers it exactly as it
/// answers every other address.
pub fn open_address(namespace: &str) -> ActionAddress {
    ActionAddress::new(format!("{namespace}.{APP_RESOURCE}"), OPEN_ACTION)
}

/// The app `address` would open, or `None` when it opens nothing.
pub fn opens(address: &ActionAddress) -> Option<&str> {
    if address.action != OPEN_ACTION {
        return None;
    }
    if let Some(app_id) = address.resource.strip_prefix("app.") {
        return (!app_id.is_empty()).then_some(app_id);
    }
    let (namespace, tail) = address.resource.split_once('.')?;
    (tail == APP_RESOURCE).then_some(namespace)
}

/// One app the agent speaks to.
///
/// What the app does is its resource kind's: the kind is registered once
/// from the app's own [`ResourceApp`](ui::framework::ResourceApp), and the
/// tree the agent surveys is the one the kind publishes for a document —
/// the tree the app's own window publishes — asked afresh each time. Which
/// app a document in the room is comes from the authenticated route it
/// arrived on, never from its contents. The rest is how the agent talks
/// about the app.
pub struct AppContract {
    /// The app's resource kind: its id and title, how its documents are
    /// read and published, and the decoders its actions replay with.
    pub kind: ResourceKind,
    /// What this app is called, in the words a sentence about it would use
    /// — "I'm not allowed to open the word processor here".
    pub title: &'static str,
    /// The namespace the app's nested resource ids start with, and the key
    /// the agent holds its document under.
    pub namespace: &'static str,
    /// The app itself, whose view the agent holds each of its records in.
    pub definition: fn() -> &'static ui::application::ApplicationDefinition,
}

impl AppContract {
    /// What a launched copy of this app announces to the broker.
    pub fn app_id(&self) -> &'static str {
        self.kind.app_id().unwrap_or_else(|| self.kind.kind())
    }
}

/// Every app the agent can speak to.
///
/// TODO: not implemented yet: the agent will act through apps' published
/// surfaces via the broker instead of linking each app it speaks to.
pub static CONTRACTS: &[AppContract] = &[];

/// The contract whose resource ids start with `namespace`.
pub fn for_namespace(namespace: &str) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|c| c.namespace == namespace)
}

/// The namespace of the app a resource belongs to: the app whose resource
/// kind `kind` is — a framework app publishes its document under its kind —
/// else the one its id starts with ([`app_of`]).
pub fn namespace_of<'a>(kind: Option<&str>, resource_id: &'a str) -> &'a str {
    kind.and_then(|kind| {
        CONTRACTS
            .iter()
            .find(|contract| contract.kind.kind() == kind)
    })
    .map_or_else(|| app_of(resource_id), |contract| contract.namespace)
}

/// The contract for an announced app id.
pub fn for_app_id(app_id: &str) -> Option<&'static AppContract> {
    CONTRACTS
        .iter()
        .find(|contract| contract.app_id() == app_id)
}

/// The contract whose package-primary document `artifact` is, in the room
/// `cluster_id`. The artifact is part of the authenticated envelope a
/// document arrives in, so this names the app that wrote it without reading
/// a byte of what it wrote.
pub fn for_artifact(
    cluster_id: &str,
    artifact: collab_types::ArtifactId,
) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|contract| {
        artifact == collab_types::document_artifact_id(contract.app_id(), cluster_id, "")
    })
}

/// The contract an operation belongs to: the app whose resource kind it is
/// published on, or whose namespace its resource id starts with.
pub fn for_descriptor(descriptor: &ActionDescriptor) -> Option<&'static AppContract> {
    CONTRACTS
        .iter()
        .find(|contract| contract.kind.kind() == descriptor.resource_kind)
        .or_else(|| for_namespace(app_of(&descriptor.address.resource)))
}

/// Decoders for every app the agent speaks to, in one registry: the door
/// decodes an encoded action through it before dispatching, and a type no
/// app registered is a refusal rather than a best-effort apply.
pub fn registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();
    for c in CONTRACTS {
        c.kind.register(&mut r);
    }
    r
}

/// One operation the room exports right now.
///
/// Inert: the address and the words the app puts on it, and nothing that
/// could perform it — there is no callback, token or handle in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The app that published it, by namespace.
    pub app: String,
    /// Exact live publisher selected by the broker catalogue. `None` only
    /// for the agent's local editorial/open helper offers.
    pub broker_peer_id: Option<u64>,
    pub descriptor: ActionDescriptor,
}

impl Offer {
    /// The address, in the one-string form an answer names it by.
    pub fn id(&self) -> String {
        let address = encode_address(&self.descriptor.address);
        self.broker_peer_id.map_or_else(
            || address.clone(),
            |peer| format!("broker:{peer}:{address}"),
        )
    }

    pub fn address(&self) -> &ActionAddress {
        &self.descriptor.address
    }

    /// Whether performing it requires a structured argument object.
    pub fn wants_arguments(&self) -> bool {
        self.descriptor.params.iter().any(|param| param.required)
    }

    /// Whether every value it needs is one of a few words it names (an
    /// alignment, a style), which an instruction says outright rather than
    /// quotes.
    pub fn wants_only_choices(&self) -> bool {
        self.wants_arguments()
            && self
                .descriptor
                .params
                .iter()
                .filter(|param| param.required)
                .all(|param| !param.choices.is_empty())
    }

    /// The choices the instruction's own words name for each required
    /// parameter that has them.
    ///
    /// # Errors
    /// The choices of the first parameter the instruction names none, or
    /// more than one, of.
    pub fn arguments_from_choices(
        &self,
        instruction: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, Vec<String>> {
        let words: Vec<String> = keywords(instruction).iter().map(|w| spelled(w)).collect();
        let mut arguments = serde_json::Map::new();
        for param in self.descriptor.params.iter().filter(|param| param.required) {
            let named: Vec<String> = param
                .choices
                .iter()
                .map(ToString::to_string)
                .filter(|choice| words.iter().any(|word| *word == choice.to_lowercase()))
                .collect();
            let [choice] = named.as_slice() else {
                return Err(param.choices.iter().map(ToString::to_string).collect());
            };
            arguments.insert(
                param.name.to_string(),
                serde_json::Value::String(choice.clone()),
            );
        }
        Ok(arguments)
    }

    /// Whether the text it wants is the address of content some other
    /// operation produced, rather than words the instruction can quote.
    ///
    /// Nobody writes a content hash into an instruction, and inventing one
    /// would name bytes that do not exist — so an operation that wants
    /// content can only be performed as a step after something that
    /// produced some.
    pub fn wants_content(&self) -> bool {
        self.content_param().is_some()
    }

    /// The parameter content another operation produced is carried under:
    /// its typed content parameter (`#[param(content)]`), or the one input of
    /// an operation that collects only content.
    pub fn content_param(&self) -> Option<String> {
        if let Some(param) = self
            .descriptor
            .params
            .iter()
            .find(|param| param.is_content())
        {
            return Some(param.name.to_string());
        }
        self.descriptor
            .input
            .wants_content()
            .then(|| tilo_actions::PROMPTED_PARAM.to_owned())
    }

    /// Whether performing it answers with content another operation takes:
    /// its declared result is a `tilo.content` resource.
    pub fn produces_content(&self) -> bool {
        matches!(
            &self.descriptor.contract.result,
            tilo_actions::ResultSpec::Resource { resource_kind }
                if resource_kind == tilo_actions::CONTENT_RESOURCE_KIND
        )
    }

    /// Deterministic floor for the common one-text-parameter action. Richer
    /// shapes are deliberately left to the structured planner; guessing how
    /// prose maps onto multiple or non-text fields would violate the schema.
    pub fn arguments_from_literal(
        &self,
        instruction: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let [param] = self.descriptor.params.as_slice() else {
            return None;
        };
        if !matches!(
            param.ty,
            tilo_actions::ParamType::Text | tilo_actions::ParamType::Secret
        ) {
            return None;
        }
        let literal = quoted_literal(instruction)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert(param.name.to_string(), serde_json::Value::String(literal));
        Some(arguments)
    }

    /// One line, in the app's own words: what this does and what it does it
    /// to.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} — on {} ({})",
            self.descriptor.action_label,
            self.descriptor.resource_label,
            self.descriptor.resource_kind
        );
        if !self.descriptor.params.is_empty() {
            let names = self
                .descriptor
                .params
                .iter()
                .map(|param| format!("{}: {}", param.name, param.ty.label()))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(s, "; arguments {{{names}}}");
        }
        if let Some(d) = &self.descriptor.description {
            let _ = write!(s, ". {d}");
        }
        s
    }
}

/// Convert the broker's permission-filtered, live catalogue into the same
/// inert offers the planner already consumes. No application registry or
/// document reconstruction is involved: arbitrary apps appear here solely
/// because they published a descriptor to the authenticated broker.
///
/// Accepts either the action host's fetched [`tilo_actions::remote::CataloguePage`]
/// or the broker's pushed [`ui::collaboration::BrokerActionSurface`] directory.
pub fn broker_offers<C: BrokerCatalogue + ?Sized>(catalogue: &C) -> Vec<Offer> {
    catalogue.broker_offers()
}

/// A live, permission-filtered action directory the broker supplies.
pub trait BrokerCatalogue {
    /// Every live action it names, as planner offers.
    fn broker_offers(&self) -> Vec<Offer>;
}

impl BrokerCatalogue for tilo_actions::remote::CataloguePage {
    fn broker_offers(&self) -> Vec<Offer> {
        catalogue_page_offers(self)
    }
}

impl BrokerCatalogue for [ui::collaboration::BrokerActionSurface] {
    fn broker_offers(&self) -> Vec<Offer> {
        surface_offers(self)
    }
}

impl BrokerCatalogue for Vec<ui::collaboration::BrokerActionSurface> {
    fn broker_offers(&self) -> Vec<Offer> {
        surface_offers(self)
    }
}

/// Offers from the broker's pushed surface directory: only live surfaces.
fn surface_offers(surfaces: &[ui::collaboration::BrokerActionSurface]) -> Vec<Offer> {
    surfaces
        .iter()
        .filter(|surface| surface.live)
        .flat_map(|surface| {
            surface.actions.iter().map(|action| {
                let input = if action.input.kind == "text" {
                    tilo_actions::InputSpec::Text {
                        prompt: action.input.prompt.clone(),
                        placeholder: action.input.placeholder.clone(),
                        collects: if action.input.wants_content {
                            tilo_actions::Collects::Content
                        } else {
                            tilo_actions::Collects::Words
                        },
                    }
                } else {
                    tilo_actions::InputSpec::None
                };
                Offer {
                    app: surface.app_id.clone(),
                    broker_peer_id: Some(surface.peer_id),
                    descriptor: ActionDescriptor {
                        semantic_identity: action.semantic_identity.clone(),
                        address: ActionAddress::new(&action.resource, &action.action),
                        resource_kind: action.resource_kind.clone(),
                        resource_label: action.resource_label.clone(),
                        action_label: action.action_label.clone(),
                        description: action.description.clone(),
                        locus: tilo_actions::ExecutionLocus::ServerPreferred,
                        kind: action.operation_kind,
                        contract: action.contract.clone(),
                        params: action.params.clone(),
                        input,
                        view: None,
                    },
                }
            })
        })
        .collect()
}

fn catalogue_page_offers(catalogue: &tilo_actions::remote::CataloguePage) -> Vec<Offer> {
    catalogue
        .rows
        .iter()
        .filter_map(|entry| {
            // A live application's actions name its broker connection; the
            // workspace's own actions (governance, opening applications)
            // keep the reserved publisher zero.
            let peer = if let Ok(peer) = entry.provider.parse::<u64>() {
                peer
            } else {
                tilo_actions::remote::workspace_provider_room(&entry.provider)?;
                0
            };
            let mut descriptor = entry.action.clone()?;
            descriptor.locus = tilo_actions::ExecutionLocus::ServerPreferred;
            Some(Offer {
                app: entry.app_id.clone(),
                broker_peer_id: Some(peer),
                descriptor,
            })
        })
        .collect()
}

/// Where `address` sits in the room, as the grants deciding it name it.
///
/// Phrased about the app the resource id names (by its app id, as every
/// grant names an app), not about the agent's own namespace: a room grants
/// per app, so folding every app's operations under one name would make a
/// grant in one of them a grant in all.
///
/// * An operation sits at its app, resource and action: a grant there
///   authorises that one operation in an app that is *already running*, and
///   confers no ability to start one.
/// * Opening an application is an undoable change of the app itself (a
///   window appearing in the room): a grant on the app authorises both its
///   operations and opening it, a grant on the room any of them.
pub fn scope_for(address: &ActionAddress) -> PermissionScope {
    if let Some(app) = opens(address) {
        return PermissionScope::app(app_id_of(app), ActionKind::Undoable);
    }
    PermissionScope::Action {
        node: TreePath::root()
            .app(app_id_of(app_of(&address.resource)))
            .resource(address.resource.clone())
            .action(address.action.clone()),
        kind: ActionKind::Undoable,
    }
}

/// The app id permissions name for `app`: the id of the app whose
/// namespace it is, or `app` itself when it already is an app id.
fn app_id_of(app: &str) -> &str {
    for_namespace(app).map_or(app, |contract| contract.app_id())
}

/// Whether the agent may perform what `scope` names, without asking first.
/// Fail-closed: anything not affirmatively allowed is refused; an agent
/// never gets past a grant that asks first on its own.
fn allowed(perms: &PermissionContext, scope: &PermissionScope) -> bool {
    match scope {
        PermissionScope::Action { node, kind } => perms.answer(node, *kind) == Answer::Allow,
        PermissionScope::Sandbox { .. } => perms.check(scope),
    }
}

/// Whether the agent may perform `address`.
pub fn permitted(perms: &PermissionContext, address: &ActionAddress) -> bool {
    allowed(perms, &scope_for(address))
}

/// Whether the agent may perform `descriptor`, published by the app
/// `app_id`, as the kind of action it declares: a read-only view is never
/// upgraded, an outside effect never downgraded. The node names the app that
/// published it, as the app's own host does, so an invitation to one app is
/// never leave to act in another.
pub(crate) fn permitted_descriptor(
    perms: &PermissionContext,
    app_id: &str,
    descriptor: &ActionDescriptor,
) -> bool {
    if opens(&descriptor.address).is_some() {
        return permitted(perms, &descriptor.address);
    }
    allowed(
        perms,
        &PermissionScope::Action {
            node: tilo_actions::action_node(
                &TreePath::root(),
                app_id,
                &descriptor.resource_kind,
                &descriptor.address.resource,
                descriptor.contract.field.as_deref(),
                &descriptor.address.action,
            ),
            kind: descriptor.contract.kind,
        },
    )
}

/// The tree `contract` publishes for `doc` as it stands: the one its kind
/// publishes, which is the one the app's own window publishes.
///
/// Rebuilt on every call, never cached — an app's tree changes with every
/// edit, and the whole value of an address is that it names what is there
/// *now*.
pub fn tree(contract: &AppContract, doc: &AutoCommit) -> Vec<Resource> {
    contract
        .kind
        .publish(contract.kind.kind(), doc)
        .into_iter()
        .collect()
}

/// Everything the room exports that the agent may perform.
///
/// The union across every document it holds, in the order the apps are
/// declared and then depth-first within each.
///
/// An open is offered onto content when an operation here, or one `beside`
/// says the room offers elsewhere, produces some.
pub fn offers(
    held: &[(&'static AppContract, &AutoCommit)],
    beside: &[Offer],
    perms: &PermissionContext,
) -> Vec<Offer> {
    let mut out = Vec::new();
    for (contract, doc) in held {
        out.extend(offers_in(contract, &tree(contract, doc), perms));
    }
    let onto = out.iter().chain(beside).any(Offer::produces_content);
    out.extend(openable(held, onto, perms));
    out
}

/// What `contract`'s app publishes in `resources` that the agent may
/// perform. Denied means absent, so nothing downstream can decide to
/// perform an operation it may not.
pub fn offers_in(
    contract: &'static AppContract,
    resources: &[Resource],
    perms: &PermissionContext,
) -> Vec<Offer> {
    tilo_actions::door::survey(resources)
        .into_iter()
        .filter(|descriptor| permitted_descriptor(perms, contract.app_id(), descriptor))
        .map(|descriptor| Offer {
            app: contract.namespace.to_string(),
            broker_peer_id: None,
            descriptor,
        })
        .collect()
}

/// Whether the room holds `namespace`'s state **with something in it**.
///
/// Two facts wear the same word. An application is *here* when a document it
/// wrote has arrived on its route; it is **surveyable** when the tree it
/// publishes for that document offers something to do by name — beyond the
/// generic typed edits every resource takes. An app can be here and offer
/// nothing — freshly opened, before it holds anything a request could act
/// on — and a step that follows an open, asking what the open produced,
/// needs the second answer while the first is what it was told.
pub fn surveyable(held: &[(&'static AppContract, &AutoCommit)], namespace: &str) -> bool {
    held.iter().any(|(c, doc)| {
        c.namespace == namespace
            && tilo_actions::door::survey(&tree(c, doc))
                .iter()
                .any(|descriptor| !descriptor.contract.is_programmatic())
    })
}

/// The applications this room does not have open, as the operations that
/// would open them.
///
/// Bounded by the contracts the agent speaks — an app it cannot read
/// afterwards is one it could start and then have nothing to say about —
/// and by the apps that are **not already here with something in them**. A
/// held app with resources of its own needs no opening; offering it anyway
/// would invite a second window over the first.
///
/// # An open application holding nothing
///
/// "Already here" is not the same as "has something to work on". An app
/// present but exporting nothing is still openable: the open is what gives
/// it something, and the operations it then publishes answer the rest of
/// the request. An app with resources of its own stays excluded.
fn openable(
    held: &[(&'static AppContract, &AutoCommit)],
    onto: bool,
    perms: &PermissionContext,
) -> Vec<Offer> {
    // An open is offered content when an operation in the room produces
    // some, on the terms every other content-taking operation is: the
    // content is that operation's typed result, never read off a document.
    let onto = if onto {
        tilo_actions::InputSpec::Text {
            prompt: "The content to open it onto".to_string(),
            placeholder: String::new(),
            collects: tilo_actions::Collects::Content,
        }
    } else {
        tilo_actions::InputSpec::None
    };
    CONTRACTS
        .iter()
        .filter(|c| !surveyable(held, c.namespace))
        .map(|c| Offer {
            app: c.namespace.to_string(),
            broker_peer_id: None,
            descriptor: ActionDescriptor {
                semantic_identity: None,
                address: open_address(c.namespace),
                resource_kind: APPLICATION_KIND.to_string(),
                resource_label: c.title.to_string(),
                action_label: format!("open {}", c.title),
                description: Some(format!(
                    "{} is not open in this room, so none of what it can do is \
                     here to be named yet. Opening it is what puts its own \
                     operations on this list.",
                    c.title
                )),
                // Inert on this descriptor: `Session::perform` recognises
                // the address before the door sees it and asks the session
                // over the socket instead, since starting an application is
                // work only the session hosting the room can do.
                locus: tilo_actions::ExecutionLocus::ServerPreferred,
                kind: tilo_actions::OperationKind::Effect,
                contract: ActionContract::default(),
                params: Vec::new(),
                input: onto.clone(),
                view: None,
            },
        })
        .filter(|o: &Offer| permitted(perms, o.address()))
        .collect()
}

/// One resource as its app publishes it, with the operations taken out.
///
/// [`Offer`] carries an operation; a resource that exports **nothing**
/// never becomes an offer, so anything an app says by publishing one alone
/// — a note in a child resource's label, say a picture's pixel dimensions —
/// is invisible to a caller working from the survey alone. This is that
/// other half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFacts {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// The labels of the child resources that export nothing.
    pub notes: Vec<String>,
}

/// Every resource the room's apps are publishing, whether or not it exports
/// anything. Rebuilt on every call for the same reason [`tree`] is.
pub fn facts(held: &[(&'static AppContract, &AutoCommit)]) -> Vec<ResourceFacts> {
    fn walk(r: &Resource, out: &mut Vec<ResourceFacts>) {
        out.push(ResourceFacts {
            id: r.id.as_str().to_string(),
            kind: r.kind.clone(),
            label: r.label.clone(),
            notes: r
                .children
                .iter()
                .filter(|c| c.actions.is_empty())
                .map(|c| c.label.clone())
                .collect(),
        });
        for c in &r.children {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for (contract, doc) in held {
        for r in tree(contract, doc) {
            walk(&r, &mut out);
        }
    }
    out
}

/// What the room exports and what it says about itself, taken together, at
/// the same moment against the same documents — asking separately would be
/// reading two different rooms.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    /// Everything the agent may perform. Denied is absent, as in [`offers`].
    pub offers: Vec<Offer>,
    /// Every resource the room's apps publish, gate or no gate: showing a
    /// resource is non-destructive and needs no permission, acting on one
    /// does — and the only way to ask for a fact is to name an address that
    /// already passed the gate to be in [`Self::offers`].
    pub facts: Vec<ResourceFacts>,
    /// The namespaces of the apps the room is actually holding, read off
    /// the held replicas rather than inferred from [`Self::offers`]: an app
    /// that is here but has nothing open exports no operations and is also
    /// offered as openable, so guessing "open" from the offers would report
    /// a running application as absent.
    pub held: Vec<&'static str>,
}

impl Catalogue {
    /// The facts about the resource `address` is published by.
    pub fn about(&self, resource: &str) -> Option<&ResourceFacts> {
        self.facts.iter().find(|f| f.id == resource)
    }

    /// The same catalogue narrowed to one resource, keeping the operations
    /// that name no resource at all.
    ///
    /// What a request that arrived carrying its own subject is decided
    /// against. The opens stay: dropping them would mean the application
    /// that would export an operation on the named resource, when it is not
    /// running, can never be started to answer the request.
    #[must_use]
    pub fn scoped_to(&self, resource: &str) -> Catalogue {
        Catalogue {
            held: self.held.clone(),
            offers: self
                .offers
                .iter()
                .filter(|o| o.address().resource == resource || opens(o.address()).is_some())
                .cloned()
                .collect(),
            facts: self
                .facts
                .iter()
                .filter(|f| f.id == resource)
                .cloned()
                .collect(),
        }
    }
}

/// Everything the room exports and everything it says, in one survey.
pub fn catalogue(
    held: &[(&'static AppContract, &AutoCommit)],
    perms: &PermissionContext,
) -> Catalogue {
    Catalogue {
        offers: offers(held, &[], perms),
        facts: facts(held),
        held: held.iter().map(|(c, _)| c.namespace).collect(),
    }
}

/// Find the offer an id names, in a freshly taken survey.
pub fn find<'a>(offers: &'a [Offer], id: &str) -> Option<&'a Offer> {
    offers.iter().find(|offer| offer.id() == id)
}

/// The app's own words for the operation an id names.
///
/// `None` when the survey no longer carries it, so the caller says
/// something that names no operation at all rather than falling back to the
/// address — an identifier in a sentence tells a reader nothing.
pub fn label_of(offers: &[Offer], id: &str) -> Option<String> {
    find(offers, id).map(|o| o.descriptor.action_label.clone())
}

// ---------------------------------------------------------------------------
// The deterministic floor
// ---------------------------------------------------------------------------

/// Words that carry no intent and must not decide anything.
const NOISE: &[&str] = &[
    "the", "this", "that", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "please",
    "can", "you", "could", "would", "it", "its", "my", "our", "me", "us", "into", "from", "at",
    "by", "as", "is", "are", "be", "do", "does", "make", "made", "up", "out",
];

fn significant(word: &str) -> bool {
    word.chars().count() >= 3 && !NOISE.contains(&word)
}

/// The instruction's words, lowercased, with punctuation dropped and the
/// noise removed.
/// `word` in the spelling apps name their choices in.
fn spelled(word: &str) -> String {
    match word {
        "centre" | "centred" | "centered" => "center".to_owned(),
        other => other.to_owned(),
    }
}

/// Every word of `text`, lowercased and in the spelling apps name their
/// choices in, noise and all.
fn words_of(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(spelled)
        .collect()
}

fn keywords(rest: &str) -> Vec<String> {
    rest.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| significant(w))
        .map(std::string::ToString::to_string)
        .collect()
}

/// A literal the instruction quoted, if it quoted exactly one.
///
/// The only argument the floor will ever supply: an operation that collects
/// text needs words that are not in the offer, and with no model the only
/// honest source is somewhere the instruction said them outright.
///
/// Straight and typographic double quotes only. An apostrophe is a letter in
/// English prose far more often than a quotation mark, and reading "don't
/// rename it" as a quoted `t rename it` is exactly the confident nonsense
/// this vocabulary exists to avoid.
pub fn quoted_literal(rest: &str) -> Option<String> {
    let mut found: Vec<String> = Vec::new();

    // Straight quotes: splitting alternates outside/inside, so every odd
    // segment is quoted. An unterminated quote leaves a final odd segment,
    // which is why the count has to be even to have closed.
    let parts: Vec<&str> = rest.split('"').collect();
    if parts.len() % 2 == 1 {
        for inside in parts.iter().skip(1).step_by(2) {
            let lit = inside.trim();
            if !lit.is_empty() {
                found.push(lit.to_string());
            }
        }
    }

    // Typographic quotes, which open and close with different characters.
    let mut haystack = rest;
    while let Some(open) = haystack.find('\u{201c}') {
        let Some(after) = haystack.get(open + '\u{201c}'.len_utf8()..) else {
            break;
        };
        let Some(close) = after.find('\u{201d}') else {
            break;
        };
        let Some(lit) = after.get(..close) else {
            break;
        };
        let lit = lit.trim();
        if !lit.is_empty() {
            found.push(lit.to_string());
        }
        let Some(rest_after) = after.get(close + '\u{201d}'.len_utf8()..) else {
            break;
        };
        haystack = rest_after;
    }

    match found.len() {
        1 => found.pop(),
        // Two literals names a thing *and* a value, which this vocabulary
        // cannot take apart. Refusing beats picking one.
        _ => None,
    }
}

/// How well an offer answers `words`: how many of them the app's own labels
/// carry. Zero means it does not answer at all.
fn score(offer: &Offer, words: &[String]) -> usize {
    let action = offer.descriptor.action_label.to_lowercase();
    let resource = offer.descriptor.resource_label.to_lowercase();
    let kind = offer.descriptor.resource_kind.to_lowercase();
    words
        .iter()
        .filter(|w| {
            // The verb has to land on the operation. Matching only the
            // thing being acted on says which file, never what to do to it.
            // A word naming one of its choices (an alignment) lands on it
            // too.
            action.contains(w.as_str())
                || resource.contains(w.as_str())
                || kind.contains(w.as_str())
                || offer.descriptor.params.iter().any(|param| {
                    param
                        .choices
                        .iter()
                        .any(|choice| choice.to_lowercase() == spelled(w))
                })
        })
        .count()
}

/// What the deterministic floor decided, when it could not decide.
///
/// Every string here is a **label**, never an address: what the agent
/// writes back goes into a document somebody is reading, and an internal
/// identifier belongs in the log line next to the refusal, not in the
/// sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoMatch {
    /// Nothing the room exports carries a word of the instruction.
    Nothing,
    /// Several operations answer it equally well, and choosing between them
    /// would be a guess. Carries their labels.
    Ambiguous(Vec<String>),
    /// The operation collects text and the instruction quoted none, so
    /// there is nothing to perform it with that the instruction actually
    /// said. Carries its label.
    NeedsText(String),
}

/// The floor: what the agent decides on its own when no model answered.
///
/// A keyword vocabulary cannot enumerate an open-ended action set, so this
/// does not try to. It matches the instruction's own words against the
/// words the apps put on their operations, and insists on a clear winner:
/// an instruction that fits two operations equally is [`NoMatch::Ambiguous`],
/// not whichever came first — unless what the instruction *supplied* breaks
/// the tie itself (see `collects_a_value_said`). A run with no model
/// reachable either does the one thing plainly named, or says which of
/// these three it ran into — it never reports success for something it did
/// not do, and never substitutes the nearest available operation for the
/// one asked for.
///
/// # Errors
///
/// Returns [`NoMatch::Nothing`] when no offer's words match the
/// instruction, [`NoMatch::Ambiguous`] when more than one offer answers
/// equally well and nothing the instruction supplied breaks the tie, and
/// [`NoMatch::NeedsText`] when the one offer left collects a value but the
/// instruction quoted none.
pub fn from_verbs<'a>(
    rest: &str,
    offers: &'a [Offer],
) -> Result<(&'a Offer, serde_json::Map<String, serde_json::Value>), NoMatch> {
    let words = keywords(rest);
    if words.is_empty() || offers.is_empty() {
        return Err(NoMatch::Nothing);
    }
    let mut best = best_scored(offers, &words);
    if best.is_empty() {
        return Err(NoMatch::Nothing);
    }
    let said = quoted_literal(rest);
    if best.len() > 1 {
        // Two operations answering to one verb need not be two meanings.
        // Whether the instruction supplied a value is a fact about which of
        // them it named — the same fact dispatch reads a moment later, off
        // [`Offer::wants_text`] — so a tie between an operation that takes
        // one and an operation that does not is not a guess.
        //
        // Only ever narrowing, and never to nothing: if every tied
        // operation refuses what the instruction supplied, the tie stands
        // and is refused, which is what keeps a genuine ambiguity (two
        // operations that both take nothing) a question rather than a coin
        // toss.
        let fits: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|o| collects_a_value_said(o) == said.is_some())
            .collect();
        if !fits.is_empty() {
            best = fits;
        }
    }
    if best.len() > 1 {
        // Equal keyword hits still leave a shorter label closer to what
        // was said: "insert table" beats "generate table of contents" for
        // an instruction that only said "table". Align left / Align centre
        // stay tied (same length) and remain Ambiguous.
        let label_words = |offer: &Offer| keywords(&offer.descriptor.action_label).len().max(1);
        let fewest = best
            .iter()
            .map(|offer| label_words(offer))
            .min()
            .unwrap_or(1);
        let denser: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|offer| label_words(offer) == fewest)
            .collect();
        if !denser.is_empty() {
            best = denser;
        }
    }
    if best.len() > 1 {
        // Words too small to decide anything alone still tell two labels
        // apart once they tie: "zoom in" is Zoom in, not Zoom out.
        let said: std::collections::HashSet<String> = words_of(rest).into_iter().collect();
        let echoed = |offer: &Offer| {
            words_of(&offer.descriptor.action_label)
                .iter()
                .filter(|word| said.contains(*word))
                .count()
        };
        let most = best.iter().map(|offer| echoed(offer)).max().unwrap_or(0);
        let closer: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|offer| echoed(offer) == most)
            .collect();
        if !closer.is_empty() {
            best = closer;
        }
    }
    if best.len() > 1 {
        return Err(NoMatch::Ambiguous(
            best.iter()
                .take(4)
                .map(|o| o.descriptor.action_label.clone())
                .collect(),
        ));
    }
    // Unreachable given the checks above — `best` is non-empty and, past
    // the `best.len() > 1` refusal, has exactly one entry — but handled like
    // any other malformed state: refuse rather than panic.
    let Some(&chosen) = best.first() else {
        return Err(NoMatch::Nothing);
    };
    if chosen.wants_only_choices() {
        return chosen
            .arguments_from_choices(rest)
            .and_then(|arguments| with_place(chosen, rest, offers, arguments))
            .map(|arguments| (chosen, arguments))
            .map_err(|choices| {
                NoMatch::Ambiguous(
                    choices
                        .iter()
                        .take(4)
                        .map(|choice| format!("{} {choice}", chosen.descriptor.action_label))
                        .collect(),
                )
            });
    }
    if !collects_a_value_said(chosen) {
        return with_place(chosen, rest, offers, serde_json::Map::new())
            .map(|arguments| (chosen, arguments))
            .map_err(NoMatch::Ambiguous);
    }
    match chosen.arguments_from_literal(rest) {
        Some(arguments) => Ok((chosen, arguments)),
        None => Err(NoMatch::NeedsText(chosen.descriptor.action_label.clone())),
    }
}

/// The offers that answer `words` best, all equally well.
fn best_scored<'a>(offers: &'a [Offer], words: &[String]) -> Vec<&'a Offer> {
    let places = place_rows(offers);
    let mut best: Vec<&Offer> = Vec::new();
    let mut best_score = 0usize;
    for offer in offers {
        // A paragraph published as a place answers by what it does, never
        // by what it says: its words name where, not what to do.
        if places.contains(offer.descriptor.address.resource.as_str())
            && score_action(offer, words) == 0
        {
            continue;
        }
        let s = score(offer, words);
        if s == 0 {
            continue;
        }
        if s > best_score {
            best_score = s;
            best = vec![offer];
        } else if s == best_score {
            best.push(offer);
        }
    }
    best
}

/// How many of `words` the operation's own label carries.
fn score_action(offer: &Offer, words: &[String]) -> usize {
    let action = offer.descriptor.action_label.to_lowercase();
    words.iter().filter(|w| action.contains(w.as_str())).count()
}

/// The resources published as places inside another resource that offers
/// an operation told where to act (`at`): the paragraphs of a document,
/// each an item of it (`<resource>/<field>/<item>`), of the field the place
/// names when it names one.
pub(crate) fn place_rows(offers: &[Offer]) -> std::collections::HashSet<&str> {
    let placed = placing(offers);
    offers
        .iter()
        .map(|offer| offer.descriptor.address.resource.as_str())
        .filter(|resource| place_in(&placed, resource).is_some())
        .collect()
}

/// Each resource offering an operation told where to act, with the field
/// its places are items of when the operation names it.
fn placing(offers: &[Offer]) -> std::collections::HashMap<&str, Option<&str>> {
    let mut placed = std::collections::HashMap::new();
    for offer in offers {
        if let Some(param) = offer
            .descriptor
            .params
            .iter()
            .find(|param| param.is_place())
        {
            placed.insert(
                offer.descriptor.address.resource.as_str(),
                param.place_field(),
            );
        }
    }
    placed
}

/// The resource `resource` is a place in, and the item it names there.
fn place_in<'a>(
    placed: &std::collections::HashMap<&str, Option<&str>>,
    resource: &'a str,
) -> Option<(&'a str, &'a str)> {
    let (parent, rest) = resource.split_once('/')?;
    let (field, item) = rest.split_once('/')?;
    let named = placed.get(parent)?;
    (named.is_none_or(|named| named == field) && !item.is_empty() && !item.contains('/'))
        .then_some((parent, item))
}

/// The place inside `offer`'s resource that `rest` names, as the paragraphs
/// it publishes word it: "the title" is the first, a quoted phrase is the
/// paragraph that begins with it.
///
/// `Ok(None)` when the instruction names none: the operation acts at the
/// agent's caret.
///
/// # Errors
/// The labels of the paragraphs a quoted phrase begins more than one of.
fn named_place(
    offer: &Offer,
    rest: &str,
    offers: &[Offer],
) -> Result<Option<serde_json::Value>, Vec<String>> {
    let resource = offer.descriptor.address.resource.as_str();
    let field = offer
        .descriptor
        .params
        .iter()
        .find(|param| param.is_place())
        .and_then(ActionParam::place_field);
    let parent = std::collections::HashMap::from([(resource, field)]);
    let mut candidates: Vec<(&str, &str)> = Vec::new();
    for candidate in offers {
        if let Some((_, item)) = place_in(&parent, candidate.descriptor.address.resource.as_str())
            && !candidates.iter().any(|(known, _)| *known == item)
        {
            candidates.push((item, candidate.descriptor.resource_label.as_str()));
        }
    }
    let item = |item: &str| Some(serde_json::json!({ "item": item }));
    if let Some(phrase) = quoted_literal(rest) {
        let phrase = phrase.to_lowercase();
        let said = |label: &str| {
            let label = label.trim_end_matches('…').to_lowercase();
            label.starts_with(&phrase) || phrase.starts_with(&label)
        };
        let named: Vec<&(&str, &str)> =
            candidates.iter().filter(|(_, label)| said(label)).collect();
        return match named.as_slice() {
            [] => Ok(None),
            [(id, _)] => Ok(item(id)),
            many => Err(many.iter().map(|(_, label)| (*label).to_owned()).collect()),
        };
    }
    if words_of(rest).iter().any(|word| word == "title") {
        return Ok(candidates.first().and_then(|(id, _)| item(id)));
    }
    Ok(None)
}

/// `arguments` with the place `rest` names inside `offer`'s resource, when
/// it takes one and the instruction names one.
fn with_place(
    offer: &Offer,
    rest: &str,
    offers: &[Offer],
    mut arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, Vec<String>> {
    let Some(param) = offer
        .descriptor
        .params
        .iter()
        .find(|param| param.is_place())
    else {
        return Ok(arguments);
    };
    if let Some(place) = named_place(offer, rest, offers)? {
        arguments.insert(param.name.to_string(), place);
    }
    Ok(arguments)
}

/// Whether performing `offer` needs a value the instruction could have said.
///
/// Narrower than [`Offer::wants_text`]. An operation that collects
/// *content* wants text too, but the text it wants is a content hash —
/// there is none anywhere in a sentence, nobody writes one down, and
/// inventing one would name bytes that do not exist. So it is never asked
/// of the words: either an earlier step produced content and the caller
/// binds it, or the operation is refused at dispatch for having nothing to
/// place. This is the question to ask of an instruction and an operation
/// together — whether what the one said is what the other takes.
fn collects_a_value_said(offer: &Offer) -> bool {
    offer.wants_arguments() && !offer.wants_content() && !offer.wants_only_choices()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::door::ActionAddress;

    #[test]
    fn offer_addresses_round_trip_name_their_app_and_stay_distinct_per_publisher() {
        // Case `an_address_with_a_path_in_it_round_trips`.
        {
            let a = ActionAddress::new("files.entry:/var/workspace/a.b/plan.md", "rename");
            assert_eq!(decode_address(&encode_address(&a)), Some(a));
        }

        // Case `the_app_is_read_off_the_resource_namespace`.
        {
            assert_eq!(app_of("word-processor.document"), "word-processor");
            assert_eq!(app_of("files.entry:/var/workspace/plan.md"), "files");
            assert_eq!(namespace_of(None, "files.entry:/plan.md"), "files");
        }

        // Case `duplicate_addresses_from_two_live_publishers_remain_distinct`.
        {
            let entry = |peer_id: u64| tilo_actions::remote::CatalogueRow {
                provider: peer_id.to_string(),
                app_id: "so.tilo.calculator".into(),
                parent_resource: String::new(),
                resource: tilo_actions::remote::CatalogueResource {
                    id: "calculator.document".into(),
                    kind: "calculator.document".into(),
                    label: "Calculator".into(),
                    presentation: None,
                },
                has_children: false,
                visibility: None,
                action: Some(ActionDescriptor {
                    semantic_identity: None,
                    address: ActionAddress::new("calculator.document", "clear"),
                    resource_kind: "calculator.document".into(),
                    resource_label: "Calculator".into(),
                    action_label: "Clear".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: tilo_actions::ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                }),
            };
            let catalogue = tilo_actions::remote::CataloguePage {
                revision: "1".into(),
                rows: vec![entry(41), entry(42)],
                ..tilo_actions::remote::CataloguePage::default()
            };
            let offers = broker_offers(&catalogue);
            assert_eq!(offers.len(), 2);
            assert_ne!(offers[0].id(), offers[1].id());
            assert_eq!(
                find(&offers, &offers[0].id()).unwrap().broker_peer_id,
                Some(41)
            );
            assert_eq!(
                find(&offers, &offers[1].id()).unwrap().broker_peer_id,
                Some(42)
            );
        }

        // Case `broker_prefixed_shell_launch_is_still_recognised_as_an_open`.
        {
            let address = ActionAddress::new("app.so.tilo.diffmerge", OPEN_ACTION);
            let offer = Offer {
                app: "so.tilo.diffmerge".into(),
                broker_peer_id: Some(0),
                descriptor: ActionDescriptor {
                    semantic_identity: None,
                    address,
                    resource_kind: APPLICATION_KIND.into(),
                    resource_label: "Diffmerge".into(),
                    action_label: "Open Diffmerge".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ServerPreferred,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                },
            };
            let decoded = decode_address(&offer.id()).expect("broker offer id remains decodable");
            assert_eq!(opens(&decoded), Some("so.tilo.diffmerge"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn a_quoted_literal_is_the_value_that_tells_same_verb_operations_apart() {
        // Case `one_quoted_literal_is_an_argument_and_two_are_not`.
        {
            assert_eq!(
                quoted_literal(r#"rename it to "launch.md""#),
                Some("launch.md".into())
            );
            assert_eq!(quoted_literal(r#"rename "a.md" to "b.md""#), None);
            assert_eq!(quoted_literal("rename it to launch.md"), None);
            assert_eq!(
                quoted_literal("rename it to \u{201c}launch.md\u{201d}"),
                Some("launch.md".into())
            );
            assert_eq!(quoted_literal("don't rename anything"), None);
            assert_eq!(quoted_literal(r#"rename it to "launch.md"#), None);
        }

        // Case `a_supplied_value_tells_two_same_verb_operations_apart`.
        {
            let offers = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "crop",
                    "Crop to a region",
                    tilo_actions::InputSpec::Text {
                        prompt: "Region to keep".to_string(),
                        placeholder: "0.25,0.25,0.5,0.5".to_string(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-square",
                    "Crop to a square",
                    tilo_actions::InputSpec::None,
                ),
            ];

            let (chosen, argument) =
                from_verbs(r#"crop the photograph to "0.25,0.25,0.5,0.5""#, &offers)
                    .expect("a quoted region tells the two crops apart");
            assert_eq!(
                chosen.address().action,
                "crop",
                "a quoted region must reach the crop that collects one, not {}",
                chosen.descriptor.action_label,
            );
            assert_eq!(
                argument.get("value"),
                Some(&serde_json::Value::String("0.25,0.25,0.5,0.5".into()))
            );

            let (chosen, argument) = from_verbs("crop the photograph", &offers)
                .expect("with no region said, the crop that needs none is the one that can act");
            assert_eq!(chosen.address().action, "crop-square");
            assert!(argument.is_empty());

            // What is gone is a false ambiguity, not the mechanism: two
            // operations that both take nothing are still not told apart by an
            // instruction that supplies nothing, and that is still a genuine
            // question rather than a guess.
            let both_bare = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "rotate-left",
                    "Rotate left",
                    tilo_actions::InputSpec::None,
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "rotate-right",
                    "Rotate right",
                    tilo_actions::InputSpec::None,
                ),
            ];
            match from_verbs("rotate the photograph", &both_bare) {
                Err(NoMatch::Ambiguous(labels)) => assert_eq!(labels.len(), 2, "{labels:?}"),
                other => panic!("a genuine ambiguity must still be refused: {other:?}"),
            }

            // A small word the instruction said still tells two labels apart
            // once they tie: "zoom in" is Zoom in, never Zoom out.
            let zooms = vec![
                offer(
                    "word-processor",
                    "word-processor.document",
                    "zoom-in",
                    "Zoom in",
                    tilo_actions::InputSpec::None,
                ),
                offer(
                    "word-processor",
                    "word-processor.document",
                    "zoom-out",
                    "Zoom out",
                    tilo_actions::InputSpec::None,
                ),
            ];
            let (chosen, _) = from_verbs("zoom in", &zooms).expect("zoom in names one operation");
            assert_eq!(chosen.address().action, "zoom-in");
            let (chosen, _) = from_verbs("zoom out please", &zooms).expect("so does zoom out");
            assert_eq!(chosen.address().action, "zoom-out");
            assert!(
                matches!(from_verbs("zoom", &zooms), Err(NoMatch::Ambiguous(_))),
                "zoom alone is still a question"
            );

            // Nor is it dropped when the split would leave nothing: two tied
            // operations that both *want* a value are still ambiguous when the
            // instruction supplied none — narrowing must give up and leave the
            // original tie standing rather than choose from an empty set.
            let both_prompted = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-left-half",
                    "Crop the left half",
                    tilo_actions::InputSpec::Text {
                        prompt: "Which half".to_string(),
                        placeholder: String::new(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-right-half",
                    "Crop the right half",
                    tilo_actions::InputSpec::Text {
                        prompt: "Which half".to_string(),
                        placeholder: String::new(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
            ];
            match from_verbs("crop the photograph", &both_prompted) {
                Err(NoMatch::Ambiguous(labels)) => assert_eq!(labels.len(), 2, "{labels:?}"),
                other => panic!(
                    "with no value supplied and both operations wanting one, the \
                     tie must stand rather than pick from nothing: {other:?}"
                ),
            }
        }
    }

    // -- telling two operations of one verb apart -------------------------

    fn offer(
        app: &'static str,
        resource: &str,
        action: &str,
        label: &str,
        input: tilo_actions::InputSpec,
    ) -> Offer {
        let params = matches!(&input, tilo_actions::InputSpec::Text { .. })
            .then(|| {
                vec![tilo_actions::ActionParam::required(
                    "value",
                    tilo_actions::ParamType::Text,
                    "value",
                )]
            })
            .unwrap_or_default();
        Offer {
            app: app.to_string(),
            broker_peer_id: None,
            descriptor: ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: "a thing".to_string(),
                action_label: label.to_string(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: ActionContract::default(),
                params,
                input,
                view: None,
            },
        }
    }
}
