//! [`TiloAuthority`]: who may invoke what on the platform (authority.md).
//!
//! The runtime asks before every invocation it makes on its own (a flow's step, an agent's block,
//! a press in a view), and the answer is the one `tilo_action_policy` gives every door: the
//! person's authority in the room ([`Standing`]), bounded by whoever acts for them (an agent, by
//! its delegation and the answers the person gave it) and by every author of the code that led to
//! the action ([`tilo_action_policy::authorship`]), the minimum winning. Code a package wrote is
//! its application ([`Subject::App`]); code a person or an agent wrote (an embedded block) is
//! that person or agent. Built-in and installed packages are trusted, an agent's code as far as
//! its person admitted the agent; a stranger's code asks.
//!
//! Where the host has no standing for the person (a window outside a host, a test), the person's
//! own part is not decided here, as a native window's detached dispatcher does not decide it: the
//! door decides what they commit. The agent and code bounds still are.
//!
//! An Ask is rendered by the host as trusted chrome, never by wasm.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use tilo_action_policy::{
    Answer, Authority as Policy, Context, Grant, Party, Standing, Subject, TreePath, authorship,
    roles,
};
use tilolang_runtime::meta::Meta;
use tilolang_runtime::{ActionInfo, Authority, Decision, ModuleHash, Principal};

use crate::meta::{MetaExt as _, action_kind};

/// Where the host reads the person's standing in the room, each time it decides.
pub type StandingSource = dyn Fn() -> Option<Standing>;

/// The standing of `person` holding nothing: for a host that only reads, so that nothing its
/// runtime might invoke runs.
pub fn holding_nothing(person: &str) -> impl Fn() -> Option<Standing> + 'static {
    let standing = Standing::person(
        tilo_action_policy::RoomPolicy::new(NO_ROOM, format!("{person}'s owner")),
        person,
    );
    move || Some(standing.clone())
}

/// The room a host without a standing decides in.
const NO_ROOM: &str = "local";

#[derive(Default)]
struct State {
    /// The code trusted to act as far as its invoker may: module versions, and the principals
    /// whose blocks are trusted (`principal:<id>`).
    trusted: HashSet<ModuleHash>,
    /// The application each loaded module version is the code of.
    apps: HashMap<ModuleHash, String>,
    /// The resource kind each action and flow is published under, by its id.
    kinds: HashMap<String, String>,
    /// The principals the door admitted as software acting for a person, with that person.
    agents: HashMap<String, Option<String>>,
    /// The packages whose records are each member's own (`records: .own`), by app id.
    own: HashSet<String>,
}

/// The platform's authority. Clones share their state.
#[derive(Clone)]
pub struct TiloAuthority {
    viewer: Principal,
    standing: Rc<StandingSource>,
    state: Rc<RefCell<State>>,
}

impl TiloAuthority {
    /// The authority of a host for `viewer`, the person at the window, whose standing in the room
    /// `standing` reads.
    pub fn new(viewer: Principal, standing: impl Fn() -> Option<Standing> + 'static) -> Self {
        Self {
            viewer,
            standing: Rc::new(standing),
            state: Rc::default(),
        }
    }

    /// The code of `module` is `meta`'s package, trusted: a built-in or installed package.
    pub fn load(&self, module: &ModuleHash, meta: &Meta) {
        let mut state = self.state.borrow_mut();
        state.trusted.insert(module.clone());
        state.apps.insert(module.clone(), meta.app.clone());
        if meta.own_records {
            state.own.insert(meta.app.clone());
        }
        for action in &meta.actions {
            let kind = meta
                .subject_of(&action.params)
                .map_or_else(|| meta.package_kind(), |ty| meta.kind_of(&ty.name));
            state.kinds.insert(action.id.clone(), kind);
        }
        for flow in &meta.flows {
            state.kinds.insert(flow.id.clone(), meta.package_kind());
        }
    }

    /// Trust the code of this exact module version.
    pub fn trust(&self, module: &ModuleHash) {
        self.state.borrow_mut().trusted.insert(module.clone());
    }

    /// Withdraw trust. Takes effect at the next invocation.
    pub fn revoke(&self, module: &ModuleHash) {
        self.state.borrow_mut().trusted.remove(module);
    }

    /// Admit `actor` as software acting for `person` (an agent's task): it acts with that
    /// person's authority bounded by its delegation and the answers the person gave it, and the
    /// code it writes is trusted as far as that goes.
    pub fn admit_agent(&self, actor: &str, person: Option<&str>) {
        let mut state = self.state.borrow_mut();
        state
            .agents
            .insert(actor.to_owned(), person.map(str::to_owned));
        state.trusted.insert(ModuleHash::principal(actor));
    }

    /// Whether `actor` was admitted as software acting for a person.
    #[must_use]
    pub fn is_agent(&self, actor: &str) -> bool {
        self.state.borrow().agents.contains_key(actor)
    }
}

/// The prefix of the platform's migration principal: a package version updating its own
/// records (`migration:<app>@<module hash>`).
pub const MIGRATION_PREFIX: &str = "migration:";

/// The principal a migration of `app`'s package version `module` (its module hash) runs as: the
/// package updating what an earlier version of it wrote, which history names as such.
#[must_use]
pub fn migration_principal(app: &str, module: &str) -> Principal {
    Principal::new(format!("{MIGRATION_PREFIX}{app}@{module}"))
}

/// The app and module hash a migration principal names, when `id` is one.
#[must_use]
pub fn migration_of(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(MIGRATION_PREFIX)?.split_once('@')
}

/// The subject a principal is in the room's policy: an agent by its id, a person by theirs.
fn principal_subject(id: &str) -> Subject {
    match id.strip_prefix("agent:") {
        Some(agent) => Subject::agent(agent),
        None => Subject::person(id),
    }
}

/// The grant subject an agent acting as `actor` is, as the broker names an agent principal.
fn agent_id(actor: &str) -> &str {
    actor.strip_prefix("agent:").unwrap_or(actor)
}

const fn decision(answer: Answer) -> Decision {
    match answer {
        Answer::Allow => Decision::Allow,
        Answer::Ask => Decision::Ask,
        Answer::Deny => Decision::Deny,
    }
}

impl Authority for TiloAuthority {
    fn decide(&self, invoker: &Principal, chain: &[ModuleHash], action: &ActionInfo) -> Decision {
        let standing = (self.standing)();
        let state = self.state.borrow();
        let room = standing.as_ref().map_or(NO_ROOM, Standing::room);
        let room_grants: &[Grant] = standing
            .as_ref()
            .map_or(&[], |standing| &standing.policy.grants);
        let kind = state
            .kinds
            .get(&action.action)
            .cloned()
            .unwrap_or_else(|| format!("tl.{}", action.app));
        let node = TreePath::room(room).app(&action.app).kind(kind);
        // An app whose records are each member's own acts on the record of the person at the
        // window: whoever else invokes is decided as on anyone's own record, refused.
        let own = standing
            .as_ref()
            .filter(|_| state.own.contains(&action.app))
            .and_then(|standing| crate::member_record_of(&standing.person));
        let node = match own {
            Some(record) => node.resource(record),
            None => node,
        }
        .action(&action.action);
        let agent = state.agents.get(&invoker.0);
        // Who the person is: the one an agent acts for, or the invoker; the person at the window
        // is the standing's person.
        let person = match agent {
            Some(person) => person.as_deref(),
            None => Some(invoker.0.as_str()),
        };
        let person = person.map(|person| match &standing {
            Some(standing) if person == self.viewer.0 => standing.person.as_str(),
            _ => person,
        });
        // A migration is the platform running the package's own update of its records on the
        // room's origin: no person's grants decide it, the package's code still bounds it.
        let migration = migration_of(&invoker.0).is_some();
        let person_party = match (&standing, person, agent) {
            _ if migration => Party::default().owning(TreePath::root()),
            (Some(standing), Some(person), _) => standing.policy.party(person),
            // An agent nobody delegated: only what the room grants it.
            (Some(standing), None, _) => standing
                .policy
                .party_of(Subject::agent(agent_id(&invoker.0))),
            // No standing here: the door decides the person's own part.
            (None, _, _) => Party::default().owning(TreePath::root()),
        };
        let delegation = match (agent, person) {
            (Some(_), Some(person)) => {
                Some(roles::agent_delegation(room, agent_id(&invoker.0), person))
            }
            _ => None,
        };
        // Each author of the code that led here, once, with what it holds of its own.
        let mut authors: Vec<(Subject, Vec<Grant>, bool)> = Vec::new();
        for module in chain {
            let (subject, of_app) = if let Some(id) = module.0.strip_prefix("principal:") {
                (principal_subject(id), None)
            } else {
                let app = state
                    .apps
                    .get(module)
                    .cloned()
                    .unwrap_or_else(|| format!("tl-module:{}", module.0));
                (Subject::app(app.clone()), Some(app))
            };
            let trusted = state.trusted.contains(module);
            let mut own = authorship::code_grants(room, &subject, trusted);
            if let Some(app) = &of_app
                && action.own
                && *module == action.module
            {
                own.push(roles::app_authorship(room, app, subject.clone()));
            }
            match authors.iter_mut().find(|(known, _, _)| *known == subject) {
                Some((_, held, _)) if trusted => held.extend(own),
                // An untrusted version of the same author asks for all of them.
                Some((_, held, _)) => *held = own,
                None => authors.push((subject, own, of_app.is_some())),
            }
        }
        // A grant written under an id the app had before (`previously:`) is the app's.
        let earlier = crate::schema::earlier_ids();
        let mut policy = Policy::person(person_party);
        if let Some(delegation) = &delegation {
            policy = policy.bounded_by(delegation.party(room_grants));
        }
        for (subject, own, of_app) in &authors {
            // The room's grants bound an application's code; a block's author holds only what
            // its code may.
            let room = if *of_app { room_grants } else { &[] };
            policy = policy.bounded_by(authorship::code_party_in(
                subject,
                own,
                room,
                Some(&earlier),
            ));
        }
        let cx = Context::at_time(std::time::SystemTime::now()).with_earlier_ids(&earlier);
        decision(policy.decide(&node, action_kind(action.class), &cx).answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    use tilo_action_policy::{ActionKind, RoomPolicy};
    use tilolang_runtime::meta::EffectClass;

    const ROOM: &str = "room";
    const PERSON: &str = "ada";
    const APP: &str = "so.tilo.lang.todo";

    fn module() -> ModuleHash {
        ModuleHash("todo-v1".to_owned())
    }

    fn info(class: EffectClass, own: bool) -> ActionInfo {
        ActionInfo {
            action: "remove".to_owned(),
            app: APP.to_owned(),
            class,
            own,
            module: module(),
        }
    }

    impl TiloAuthority {
        fn load_trusted_app(&self, module: &ModuleHash, app: &str) {
            let mut state = self.state.borrow_mut();
            state.trusted.insert(module.clone());
            state.apps.insert(module.clone(), app.to_owned());
        }
    }

    fn authority(grants: Vec<Grant>) -> TiloAuthority {
        let mut policy = RoomPolicy::new(ROOM, PERSON);
        policy.grants.extend(grants);
        let standing = Standing::person(policy, PERSON);
        let authority = TiloAuthority::new(Principal::new(PERSON), move || Some(standing.clone()));
        let mut state = authority.state.borrow_mut();
        state.trusted.insert(module());
        state.apps.insert(module(), APP.to_owned());
        drop(state);
        authority
    }

    #[test]
    fn a_package_whose_records_are_each_members_own_is_its_persons_alone() {
        let ada = "ab".repeat(32);
        let owner = "ef".repeat(32);
        let mut policy = RoomPolicy::new(ROOM, owner.clone());
        policy
            .members
            .insert(ada.clone(), tilo_action_policy::Role::Editor);
        let standing = Standing::person(policy, ada.clone());
        let authority =
            TiloAuthority::new(Principal::new(ada.clone()), move || Some(standing.clone()));
        {
            let mut state = authority.state.borrow_mut();
            state.trusted.insert(module());
            state.apps.insert(module(), APP.to_owned());
        }
        let change = info(EffectClass::Undoable, true);
        let room_owner = Principal::new(owner);
        // Shared, the room's owner may change what the package holds.
        assert_eq!(
            authority.decide(&room_owner, &[module()], &change),
            Decision::Allow
        );
        // Each member's own, it is Ada's record: only she acts on it, not even the room's owner.
        authority.state.borrow_mut().own.insert(APP.to_owned());
        assert_eq!(
            authority.decide(&Principal::new(ada), &[module()], &change),
            Decision::Allow
        );
        assert_eq!(
            authority.decide(&room_owner, &[module()], &change),
            Decision::Deny
        );
    }

    #[test]
    fn the_persons_agent_answer_denies_the_agent() {
        let answer = agent_answers::grant(
            ROOM,
            "helper",
            (&AnswerScope::app(APP), ActionKind::Undoable),
            Answer::Deny,
            Subject::person(PERSON),
        );
        let authority = authority(vec![answer]);
        authority.admit_agent("agent:helper", Some(PERSON));
        let agent = Principal::new("agent:helper");
        let own = info(EffectClass::Undoable, true);
        assert_eq!(authority.decide(&agent, &[], &own), Decision::Deny);
        assert_eq!(authority.decide(&agent, &[module()], &own), Decision::Deny);
        // The person at the window is not their agent.
        let me = Principal::new(PERSON);
        assert_eq!(authority.decide(&me, &[module()], &own), Decision::Allow);
    }

    #[test]
    fn an_agent_asks_before_reaching_outside_until_its_person_answers() {
        let authority = authority(Vec::new());
        authority.admit_agent("agent:helper", Some(PERSON));
        let agent = Principal::new("agent:helper");
        let outside = info(EffectClass::Outside, true);
        assert_eq!(authority.decide(&agent, &[], &outside), Decision::Ask);
        let undoable = info(EffectClass::Undoable, true);
        assert_eq!(authority.decide(&agent, &[], &undoable), Decision::Allow);
    }

    #[test]
    fn code_of_an_untrusted_author_asks() {
        let authority = authority(Vec::new());
        let me = Principal::new(PERSON);
        let stranger = ModuleHash::principal("person:stranger");
        let foreign = info(EffectClass::Undoable, false);
        assert_eq!(
            authority.decide(&me, std::slice::from_ref(&stranger), &foreign),
            Decision::Ask
        );
        let unknown = ModuleHash("someone-elses".to_owned());
        assert_eq!(authority.decide(&me, &[unknown], &foreign), Decision::Ask);
        authority.trust(&stranger);
        assert_eq!(
            authority.decide(&me, &[stranger], &foreign),
            Decision::Allow
        );
    }

    #[test]
    fn a_trusted_modules_own_action_is_allowed() {
        let authority = authority(Vec::new());
        let me = Principal::new(PERSON);
        let own = info(EffectClass::Undoable, true);
        assert_eq!(authority.decide(&me, &[module()], &own), Decision::Allow);
        // Untrusted, it is still an author of its own records.
        authority.revoke(&module());
        assert_eq!(authority.decide(&me, &[module()], &own), Decision::Allow);
        let foreign = info(EffectClass::Undoable, false);
        assert_eq!(authority.decide(&me, &[module()], &foreign), Decision::Ask);
    }

    #[test]
    fn outside_from_a_strangers_code_asks() {
        let authority = authority(Vec::new());
        let me = Principal::new(PERSON);
        let stranger = ModuleHash::principal("person:stranger");
        let outside = info(EffectClass::Outside, false);
        assert_eq!(authority.decide(&me, &[stranger], &outside), Decision::Ask);
        // Trusted code reaching outside asks too, unless the room grants its app that.
        assert_eq!(
            authority.decide(&me, &[module()], &info(EffectClass::Outside, true)),
            Decision::Ask
        );
    }

    #[test]
    fn the_rooms_denials_reach_the_runtime() {
        let me = Principal::new("bob");
        // Bob is no member: nothing he invokes runs.
        let authority = authority(Vec::new());
        let own = info(EffectClass::Undoable, true);
        assert_eq!(authority.decide(&me, &[module()], &own), Decision::Deny);
        // A denial of the app's code holds for the owner too.
        let denied = Grant::new(
            "deny-todo",
            Subject::app(APP),
            TreePath::room(ROOM),
            tilo_action_policy::KindSet::ALL,
            Answer::Deny,
            Subject::person(PERSON),
        );
        let authority = self::authority(vec![denied]);
        let me = Principal::new(PERSON);
        assert_eq!(authority.decide(&me, &[module()], &own), Decision::Deny);
        assert_eq!(authority.decide(&me, &[], &own), Decision::Allow);
    }

    #[test]
    fn grants_on_the_earlier_id_decide_for_the_renamed_app() {
        const TASKS: &str = "so.tilo.lang.tasks";
        crate::schema::register_previously(TASKS, &[APP.to_owned()]);
        let tasks = ModuleHash("tasks-v2".to_owned());
        let action = |class| ActionInfo {
            action: "remove".to_owned(),
            app: TASKS.to_owned(),
            class,
            own: true,
            module: tasks.clone(),
        };
        let renamed = |grants: Vec<Grant>| {
            let authority = authority(grants);
            let mut state = authority.state.borrow_mut();
            state.trusted.insert(tasks.clone());
            state.apps.insert(tasks.clone(), TASKS.to_owned());
            drop(state);
            authority
        };
        let own = action(EffectClass::Undoable);
        let me = Principal::new(PERSON);
        assert_eq!(
            renamed(Vec::new()).decide(&me, std::slice::from_ref(&tasks), &own),
            Decision::Allow
        );

        // The room's denial of Todo's code holds for Tasks' code.
        let app_denied = Grant::new(
            "deny-todo",
            Subject::app(APP),
            TreePath::room(ROOM),
            tilo_action_policy::KindSet::ALL,
            Answer::Deny,
            Subject::person(PERSON),
        );
        assert_eq!(
            renamed(vec![app_denied.clone()]).decide(&me, std::slice::from_ref(&tasks), &own),
            Decision::Deny
        );
        // The person's answer for their agent on Todo holds on Tasks.
        let answer = agent_answers::grant(
            ROOM,
            "helper",
            (&AnswerScope::app(APP), ActionKind::Undoable),
            Answer::Deny,
            Subject::person(PERSON),
        );
        let authority = renamed(vec![answer]);
        authority.admit_agent("agent:helper", Some(PERSON));
        let agent = Principal::new("agent:helper");
        assert_eq!(
            authority.decide(&agent, std::slice::from_ref(&tasks), &own),
            Decision::Deny
        );
        // A grant on Todo's node, `room/app:todo/**`, decides for a member on Tasks.
        let mut policy = RoomPolicy::new(ROOM, PERSON);
        let node_denied = Grant::new(
            "no-todo-for-bob",
            Subject::person("bob"),
            TreePath::room(ROOM).app(APP).rest(),
            tilo_action_policy::KindSet::ALL,
            Answer::Deny,
            Subject::person(PERSON),
        );
        let editor = Grant::new(
            "bob-edits",
            Subject::person("bob"),
            TreePath::room(ROOM),
            tilo_action_policy::KindSet::ALL,
            Answer::Allow,
            Subject::person(PERSON),
        );
        policy.grants.extend([node_denied.clone(), editor.clone()]);
        let stored = policy.grants.clone();
        let standing = Standing::person(policy.clone(), "bob");
        let bob = TiloAuthority::new(Principal::new("bob"), move || Some(standing.clone()));
        bob.load_trusted_app(&tasks, TASKS);
        assert_eq!(
            bob.decide(&Principal::new("bob"), std::slice::from_ref(&tasks), &own),
            Decision::Deny
        );
        // Another app of the room is not Todo's.
        let notes = ActionInfo {
            app: "so.tilo.lang.notes".to_owned(),
            ..action(EffectClass::Undoable)
        };
        assert_eq!(
            bob.decide(&Principal::new("bob"), &[], &notes),
            Decision::Allow
        );
        // Nothing stored was rewritten: the grants name Todo as they did.
        assert_eq!(policy.grants, stored);
        assert!(stored.contains(&node_denied) && stored.contains(&editor));
        assert_eq!(app_denied.subject, Subject::app(APP));
    }

    #[test]
    fn without_a_standing_the_bounds_still_decide() {
        let authority = TiloAuthority::new(Principal::new(PERSON), || None);
        authority.admit_agent("agent:helper", Some(PERSON));
        let agent = Principal::new("agent:helper");
        assert_eq!(
            authority.decide(&agent, &[], &info(EffectClass::Outside, true)),
            Decision::Ask
        );
        let me = Principal::new(PERSON);
        let stranger = ModuleHash::principal("person:stranger");
        assert_eq!(
            authority.decide(&me, &[stranger], &info(EffectClass::Undoable, false)),
            Decision::Ask
        );
        assert_eq!(
            authority.decide(&me, &[], &info(EffectClass::Undoable, true)),
            Decision::Allow
        );
    }
}
