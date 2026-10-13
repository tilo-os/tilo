//! The least authority a shortcut needs, and how a run's step is decided
//! against its grant.
//!
//! A shortcut's grant is a set of rules over the published tree, the same
//! `(node, kinds, answer)` a person grants to a collaborator or an agent. It
//! bounds the shortcut: the host still decides, from the invoking person's
//! own grants, whether that person may take the action at all. The effective
//! answer is the stricter of the two.

use tilo_action_policy::{
    ActionKind, Answer, Context, Grant, GrantId, KindSet, Level, Lifetime, Rule, Segment, Subject,
    TreePath, decide,
};
use tilo_actions::ActionKey;
use tilo_actions::door::ActionAddress;

use crate::{
    AGENT_TRANSFORM_KIND, ActionCatalog, Binding, ResourceTarget, Selector, Step, StepKind,
};

/// The node a step's action has in the tree: the room the run acts in (or
/// any room, for a run confined to none), the app, the resource kind, the
/// resource and the action.
#[must_use]
pub fn step_node(
    room: Option<&str>,
    app_id: &str,
    resource_kind: &str,
    address: &ActionAddress,
) -> TreePath {
    let base = match room {
        Some(room) => TreePath::room(room),
        None => TreePath::root().any(Level::Room),
    };
    base.app(app_id)
        .kind(resource_kind)
        .resource(address.resource.clone())
        .action(address.action.clone())
}

/// A rule naming one action on every room's matching resource, as a
/// shortcut's grant usually does.
#[must_use]
pub fn action_rule(
    app_id: &str,
    resource_kind: &str,
    resource: Option<&str>,
    action: &str,
    kinds: tilo_action_policy::KindSet,
    answer: Answer,
) -> Rule {
    let node = TreePath::root()
        .any(Level::Room)
        .app(app_id)
        .kind(resource_kind);
    let node = match resource {
        Some(resource) => node.resource(resource),
        None => node.any(Level::Resource),
    };
    Rule::new(node.action(action), kinds, answer)
}

/// The confirmations a grant gives in advance: each allowing rule that
/// names one action, as (resource, action) — the resource `*` where the
/// rule covers every resource of its kind.
#[must_use]
pub fn advance_consents(grant: &[Rule]) -> Vec<(String, String)> {
    grant
        .iter()
        .filter(|rule| rule.answer == Answer::Allow)
        .filter_map(|rule| {
            let segments = rule.node.segments();
            let Some(Segment::Name(Level::Action, action)) = segments.last() else {
                return None;
            };
            let resource = segments.iter().rev().find_map(|segment| match segment {
                Segment::Name(Level::Resource, name) => Some(name.clone()),
                Segment::Any(Level::Resource) => Some("*".to_owned()),
                _ => None,
            })?;
            Some((resource, action.clone()))
        })
        .collect()
}

/// What a shortcut's grant answers for one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepGrant {
    pub answer: Answer,
    /// The deciding rule names this very action (not an app or a kind of
    /// resource above it): an advance consent an unattended run may use in
    /// place of a person's confirmation.
    pub names_action: bool,
}

/// What `grant` answers for the action at `node` of `kind`.
#[must_use]
pub fn grant_answer(grant: &[Rule], node: &TreePath, kind: ActionKind) -> StepGrant {
    let subject = Subject::Shortcut(String::new());
    let grants: Vec<Grant> = grant
        .iter()
        .enumerate()
        .map(|(index, rule)| Grant {
            id: GrantId::new(index.to_string()),
            subject: subject.clone(),
            node: rule.node.clone(),
            kinds: rule.kinds,
            answer: rule.answer,
            lifetime: Lifetime::UntilRevoked,
            granted_by: subject.clone(),
        })
        .collect();
    let decision = decide(&grants, &[subject], node, kind, &Context::at(0));
    StepGrant {
        answer: decision.answer,
        names_action: decision.grant().is_some_and(|grant| {
            matches!(
                grant.node.segments().last(),
                Some(Segment::Name(Level::Action, _))
            )
        }),
    }
}

/// The resources a permission reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The one resource a step names.
    Resource(String),
    /// Every resource of a kind, for a step that picks its resource when it
    /// runs.
    Kind(String),
}

/// One permission the shortcut will use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub app_id: String,
    pub reach: Reach,
    pub action: String,
    /// The kind of action it is: reading, an undoable change, or one that
    /// reaches outside.
    pub kind: ActionKind,
    /// How the catalogue names it: "List notes in Inbox".
    pub label: String,
}

impl Permission {
    /// The rule allowing it in any room: this action, on the one resource
    /// or on every resource of the kind, for its kind of action and the
    /// weaker ones. It reaches the action anywhere in the resource: a record
    /// holding the kind's resources as items (a chat's workspace) publishes
    /// it on the item, and every member decides the change there.
    #[must_use]
    pub fn rule(&self) -> Rule {
        let kinds = ActionKind::ALL
            .into_iter()
            .filter(|kind| *kind <= self.kind)
            .fold(KindSet::NONE, KindSet::with);
        Rule::new(
            self.reached().action(self.action.clone()),
            kinds,
            Answer::Allow,
        )
    }

    /// Anywhere in the resources it reaches, in any room.
    fn reached(&self) -> TreePath {
        let app = TreePath::root().any(Level::Room).app(self.app_id.clone());
        match &self.reach {
            Reach::Resource(resource) => app.any(Level::Kind).resource(resource.clone()),
            Reach::Kind(kind) => app.kind(kind.clone()).any(Level::Resource),
        }
        .rest()
    }
}

/// Every permission `steps` use, in step order, each once as the strongest
/// kind any step takes it as.
#[must_use]
pub fn least_privilege(steps: &[Step], catalog: &ActionCatalog) -> Vec<Permission> {
    let mut out = Vec::new();
    collect(steps, catalog, &mut out);
    out
}

/// The grant rules for `permissions`.
#[must_use]
pub fn rules(permissions: &[Permission]) -> Vec<Rule> {
    permissions.iter().map(Permission::rule).collect()
}

fn collect(steps: &[Step], catalog: &ActionCatalog, out: &mut Vec<Permission>) {
    for step in steps {
        match &step.kind {
            StepKind::Invoke {
                action, resource, ..
            } => {
                let target = match resource {
                    Selector::Fixed(id)
                    | Selector::FromBinding(Binding::Literal(serde_json::Value::String(id))) => {
                        ResourceTarget::Id(id)
                    }
                    Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
                    Selector::FromBinding(_) => ResourceTarget::Any,
                };
                add(catalog, action, target, out);
            }
            StepKind::Ai { transform, .. } => add(
                catalog,
                &transform.key(),
                ResourceTarget::Kind(AGENT_TRANSFORM_KIND),
                out,
            ),
            StepKind::If {
                then, otherwise, ..
            } => {
                collect(then, catalog, out);
                collect(otherwise, catalog, out);
            }
            StepKind::ForEach { body, .. } => collect(body, catalog, out),
            StepKind::SetVar { .. } | StepKind::Ask { .. } => {}
        }
    }
}

fn add(
    catalog: &ActionCatalog,
    key: &ActionKey,
    target: ResourceTarget<'_>,
    out: &mut Vec<Permission>,
) {
    let (app_id, action) = (key.app.as_str(), key.action.as_str());
    let row = catalog.find(key, target);
    let descriptor = row.and_then(|row| row.action.as_ref());
    let reach = match target {
        ResourceTarget::Id(id) => Reach::Resource(id.to_owned()),
        // The kind of the resource the step finds, which may be a sub-kind
        // of the one it names: a grant covers exactly that kind.
        ResourceTarget::Kind(kind) => Reach::Kind(descriptor.map_or_else(
            || kind.to_owned(),
            |descriptor| descriptor.resource_kind.clone(),
        )),
        // A bound resource is only known when the shortcut runs; the kind
        // of the resource the catalogue offers the action on is what it
        // may reach. Without one there is nothing to grant.
        ResourceTarget::Any => match descriptor {
            Some(descriptor) => Reach::Kind(descriptor.resource_kind.clone()),
            None => return,
        },
    };
    let kind = descriptor.map_or(ActionKind::Undoable, |descriptor| descriptor.contract.kind);
    let label = match (&reach, descriptor) {
        (Reach::Resource(_), Some(descriptor)) => format!(
            "{} in {}",
            descriptor.action_label, descriptor.resource_label
        ),
        (Reach::Kind(kind), Some(descriptor)) => {
            format!("{} in {kind}", descriptor.action_label)
        }
        (Reach::Resource(resource) | Reach::Kind(resource), None) => {
            format!("{action} in {resource}")
        }
    };
    if let Some(existing) = out.iter_mut().find(|existing| {
        existing.app_id == app_id && existing.reach == reach && existing.action == action
    }) {
        existing.kind = existing.kind.max(kind);
        return;
    }
    out.push(Permission {
        app_id: app_id.to_owned(),
        reach,
        action: action.to_owned(),
        kind,
        label,
    });
}

/// Read rules the triggers of a program need: a record trigger reads the
/// kind it watches, in whatever app publishes it; a connector trigger reads
/// the connector's app.
#[must_use]
pub fn trigger_rules(triggers: &[crate::TriggerSpec]) -> Vec<Rule> {
    let any = TreePath::root().any(Level::Room);
    let mut out: Vec<Rule> = Vec::new();
    for trigger in triggers {
        let rule = match trigger {
            crate::TriggerSpec::RecordChanged { kind, .. } => Rule::new(
                any.clone().any(Level::App).kind(kind.clone()),
                KindSet::READ,
                Answer::Allow,
            ),
            crate::TriggerSpec::Connector { app_id, .. } => Rule::new(
                any.clone().app(app_id.clone()),
                KindSet::READ,
                Answer::Allow,
            ),
            _ => continue,
        };
        if !out.contains(&rule) {
            out.push(rule);
        }
    }
    out
}

/// The action a bot settling an outbox entry takes: the entry's result is
/// a field edit of the resource that queued it.
pub const SETTLE_ACTION: &str = "field-edit";

/// The field a connector's records keep their outbox under.
pub const OUTBOX_FIELD: &str = "outbox";

/// The rules for settling what `permissions` queue: an action that reaches
/// outside queues an entry its performer settles in the same resource. A
/// settle edits one entry, an item of the outbox, so the rule reaches the
/// resource's items and none of the resource's own fields: what it is
/// connected to and who owns it stay the person's to change.
fn settle_rules(permissions: &[Permission]) -> Vec<Rule> {
    permissions
        .iter()
        .filter(|permission| permission.kind == ActionKind::Outside)
        .map(|permission| {
            Rule::new(
                permission
                    .reached()
                    .any(Level::Item)
                    .rest()
                    .action(SETTLE_ACTION),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
            )
        })
        .collect()
}

/// The whole grant a program needs: its steps' least privilege, the
/// settling of what they queue outside, then its triggers' reads. What a
/// bot's stored grant must equal.
#[must_use]
pub fn derived_rules(shortcut: &crate::Shortcut, catalog: &ActionCatalog) -> Vec<Rule> {
    let permissions = least_privilege(&shortcut.steps, catalog);
    let mut out = rules(&permissions);
    for rule in settle_rules(&permissions)
        .into_iter()
        .chain(trigger_rules(&shortcut.triggers))
    {
        if !out.contains(&rule) {
            out.push(rule);
        }
    }
    out
}

/// The rules of `shortcut.grant` that no step and no trigger could use,
/// judged by shape alone: an action rule must name the app and action of
/// some step (or lowered `Ai` step), a read rule must be one a trigger
/// needs. The governor runs this without a catalogue, so a program can never
/// carry authority its steps do not even mention.
#[must_use]
pub fn unused_rules(shortcut: &crate::Shortcut) -> Vec<Rule> {
    let mut keys: Vec<ActionKey> = Vec::new();
    step_keys(&shortcut.steps, &mut keys);
    let reads = trigger_rules(&shortcut.triggers);
    shortcut
        .grant
        .iter()
        .filter(|rule| {
            if reads.contains(rule) {
                return false;
            }
            let app = rule.node.id_at(Level::App);
            let action = rule.node.id_at(Level::Action);
            // Settling is judged by shape too: some step acts in the app.
            let settles = action == Some(SETTLE_ACTION);
            !keys.iter().any(|key| {
                app == Some(key.app.as_str()) && (settles || action == Some(key.action.as_str()))
            })
        })
        .cloned()
        .collect()
}

fn step_keys(steps: &[Step], out: &mut Vec<ActionKey>) {
    for step in steps {
        match &step.kind {
            StepKind::Invoke { action, .. } => out.push(action.clone()),
            StepKind::Ai { transform, .. } => out.push(transform.key()),
            StepKind::If {
                then, otherwise, ..
            } => {
                step_keys(then, out);
                step_keys(otherwise, out);
            }
            StepKind::ForEach { body, .. } => step_keys(body, out),
            StepKind::SetVar { .. } | StepKind::Ask { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "so.tilo.notes";

    fn answer(grant: &[Rule], resource: &str, kind: &str, action: &str) -> Answer {
        let node = step_node(
            Some("room"),
            NOTES,
            kind,
            &ActionAddress::new(resource, action),
        );
        grant_answer(grant, &node, ActionKind::Undoable).answer
    }

    #[test]
    fn a_shortcut_grant_reaches_its_resource_or_exactly_its_kind() {
        // One resource's action: not the same action on another resource.
        let exact = [action_rule(
            NOTES,
            "tilo.rich-text",
            Some("notes.notebook/page/1/body"),
            "apply",
            KindSet::ALL,
            Answer::Allow,
        )];
        assert_eq!(
            answer(
                &exact,
                "notes.notebook/page/1/body",
                "tilo.rich-text",
                "apply"
            ),
            Answer::Allow
        );
        assert_eq!(
            answer(&exact, "notes.notebook", "notes.notebook", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(
                &exact,
                "notes.notebook/page/2/body",
                "tilo.rich-text",
                "apply"
            ),
            Answer::Deny
        );

        // Every resource of one kind: that kind exactly, in that app, for that
        // action. Neither a sub-kind nor a kind sharing a prefix.
        let kind = [action_rule(
            NOTES,
            "notes.page",
            None,
            "apply",
            KindSet::ALL,
            Answer::Allow,
        )];
        assert_eq!(
            answer(&kind, "notes.page:1", "notes.page", "apply"),
            Answer::Allow
        );
        assert_eq!(
            answer(&kind, "notes.page:2", "notes.page", "apply"),
            Answer::Allow
        );
        assert_eq!(
            answer(&kind, "notes.page:1/x", "notes.page.special", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(&kind, "notes.pages:1", "notes.pages", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(&kind, "notes.page:1", "notes.page", "delete"),
            Answer::Deny
        );
        let chat = step_node(
            Some("room"),
            "so.tilo.chat",
            "notes.page",
            &ActionAddress::new("notes.page:1", "apply"),
        );
        assert_eq!(
            grant_answer(&kind, &chat, ActionKind::Undoable).answer,
            Answer::Deny
        );

        // A narrower deny on the kind's undoable changes caps it.
        let mut restricted = kind.to_vec();
        restricted.push(action_rule(
            NOTES,
            "notes.page",
            Some("notes.page:1"),
            "apply",
            KindSet::UNDOABLE,
            Answer::Deny,
        ));
        assert_eq!(
            answer(&restricted, "notes.page:1", "notes.page", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(&restricted, "notes.page:2", "notes.page", "apply"),
            Answer::Allow
        );
    }

    #[test]
    fn only_a_rule_naming_the_action_is_an_advance_consent() {
        let node = step_node(
            None,
            NOTES,
            "notes.note",
            &ActionAddress::new("notes.note:1", "delete"),
        );
        let app = [Rule::new(
            TreePath::root().any(Level::Room).app(NOTES),
            KindSet::ALL,
            Answer::Allow,
        )];
        let decided = grant_answer(&app, &node, ActionKind::Undoable);
        assert_eq!(decided.answer, Answer::Allow);
        assert!(!decided.names_action);
        let consent = [action_rule(
            NOTES,
            "notes.note",
            Some("notes.note:1"),
            "delete",
            KindSet::ALL,
            Answer::Allow,
        )];
        assert!(grant_answer(&consent, &node, ActionKind::Undoable).names_action);
    }

    #[test]
    fn triggers_read_what_they_watch() {
        use crate::{ActorFilter, TriggerSpec};
        let rules = trigger_rules(&[
            TriggerSpec::RecordChanged {
                kind: "chat.workspace".into(),
                actions: vec!["post".into()],
                fields: vec![],
                by: ActorFilter::NotBots,
            },
            TriggerSpec::Connector {
                app_id: "so.tilo.discord".into(),
                source: "message-created".into(),
                filter: serde_json::Map::new(),
            },
        ]);
        let any = TreePath::root().any(Level::Room);
        assert_eq!(
            rules,
            vec![
                Rule::new(
                    any.clone().any(Level::App).kind("chat.workspace"),
                    KindSet::READ,
                    Answer::Allow
                ),
                Rule::new(any.app("so.tilo.discord"), KindSet::READ, Answer::Allow),
            ]
        );
    }

    #[test]
    fn a_rule_no_step_or_trigger_uses_is_unused() {
        let post = action_rule(
            "so.tilo.chat",
            "chat.workspace",
            None,
            "post",
            KindSet::READ_UNDOABLE,
            Answer::Allow,
        );
        let delete = action_rule(
            "so.tilo.chat",
            "chat.workspace",
            None,
            "delete",
            KindSet::READ_UNDOABLE,
            Answer::Allow,
        );
        let shortcut = crate::Shortcut {
            id: "bridge".into(),
            name: "Bridge".into(),
            inputs: vec![],
            steps: vec![crate::Step::new(
                "post",
                crate::StepKind::Invoke {
                    action: ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                    resource: crate::Selector::FirstOfKind("chat.workspace".into()),
                    args: std::collections::BTreeMap::default(),
                    appearance: None,
                },
            )],
            output: None,
            grant: vec![post, delete.clone()],
            triggers: vec![],
            saved_by: None,
        };
        assert_eq!(unused_rules(&shortcut), vec![delete]);
    }

    #[test]
    fn an_action_reaching_outside_lets_its_performer_settle_it() {
        let permission = |action: &str, kind| Permission {
            app_id: "so.tilo.discord".into(),
            reach: Reach::Kind("discord.guild".into()),
            action: action.into(),
            kind,
            label: String::new(),
        };
        let settles = settle_rules(&[
            permission("send", ActionKind::Outside),
            permission("map", ActionKind::Undoable),
        ]);
        assert_eq!(settles.len(), 1, "only what is queued outside is settled");
        // It reaches an entry of a guild's outbox, as the room decides a
        // settle (an item of the record), and none of the guild's own fields.
        let guild = TreePath::room("r")
            .app("so.tilo.discord")
            .kind("discord.guild")
            .resource("g");
        let rule = &settles[0];
        assert!(
            rule.node
                .covers(&guild.clone().item("e1").action(SETTLE_ACTION))
        );
        assert!(!rule.node.covers(&guild.clone().action(SETTLE_ACTION)));
        assert!(
            !rule
                .node
                .covers(&guild.field("ownership").action(SETTLE_ACTION))
        );
        let shortcut = crate::Shortcut {
            id: "bridge".into(),
            name: "Bridge".into(),
            inputs: vec![],
            steps: vec![crate::Step::new(
                "send",
                crate::StepKind::Invoke {
                    action: ActionKey::new("so.tilo.discord", "discord.guild", "send"),
                    resource: crate::Selector::FirstOfKind("discord.guild".into()),
                    args: std::collections::BTreeMap::default(),
                    appearance: None,
                },
            )],
            output: None,
            grant: settles,
            triggers: vec![],
            saved_by: None,
        };
        assert!(unused_rules(&shortcut).is_empty());
    }
}
