//! The actions a window shows, and what the device decides of each for the person at the window.
//!
//! A window decides no permission itself. It names to its device every action it publishes or
//! shows, as one read when it opens; the device decides each with the person's ordinary answer in
//! the workspace that keeps it (the window's own, or another: a person's profile record is kept in
//! their own workspace wherever it is shown), and answers again whenever a grant, a role or a
//! declaration changes. The window acts at once on an allowed action, shows why a refused one is
//! refused, and sends one that asks first through the device, which keeps it as its asked record.
//! Wherever anything lands (a write, an effect's call, a grant given once) the device admits it
//! again, so a verdict gone stale never lets a refused change through.

use serde::{Deserialize, Serialize};

use tilo_action_policy::{Level, Segment, TreePath};

use crate::{ActionKind, ActionNeed};

/// The provider a device answers a window's shown actions on, and the app it is published under.
pub const PROVIDER: &str = "tilo.shown";
pub const APP: &str = "tilo.shown";

/// What a person reads when the workspace holding a record is not held on this host.
pub const NOT_HERE: &str =
    "This is kept in a workspace this device doesn't have, so it can't be changed here.";
/// What a person reads when the workspace's grants refuse the action.
pub const REFUSED: &str = "You aren't allowed to change this.";
/// What a person reads when the holding workspace could not be read just now.
pub const UNREADABLE: &str = "This can't be checked right now. Try again.";
/// What a person reads at a control while its verdict is on its way.
pub const CHECKING: &str = "Checking whether you can do this.";
/// What a provider that decides no shown actions answers.
pub const UNAVAILABLE: &str = "this device decides no actions a window shows";

/// The record a device decides an action as declared on ([`ShownAction::on_any_record`]): one no
/// grant names, since no record is named so.
const ANY_RECORD: &str = "\u{1}any record";

/// What the action of a [`ShownAction::reach`] question starts with, before the capability.
const REACH: &str = "\u{1}reach ";

/// One question a window shows the answer to: whether its person may take an action of a kind
/// at a node of the workspace's tree. A published action's node is where every API decides it
/// ([`crate::action_node`]); a window also asks about its app's, a record's or the workspace's
/// own (its settings, inviting someone).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ShownAction {
    /// The workspace that keeps the node when it is not the window's.
    pub workspace: Option<String>,
    /// The node below the workspace.
    pub node: TreePath,
    /// The kind of action.
    pub kind_needed: ActionKind,
    /// Whether it exercises authority over others.
    pub privileged: bool,
}

impl ShownAction {
    /// The action `action` of the record `resource` (of kind `kind`) that the app `app`
    /// publishes, held in `workspace` when that is not the window's, which `need` describes.
    #[must_use]
    pub fn new(
        workspace: Option<&str>,
        app: &str,
        (kind, resource, action): (&str, &str, &str),
        need: &ActionNeed,
    ) -> Self {
        Self {
            workspace: workspace.map(str::to_owned),
            node: crate::action_node(
                &TreePath::root(),
                app,
                kind,
                resource,
                need.field.as_deref(),
                action,
            ),
            kind_needed: need.kind,
            privileged: need.privileged,
        }
    }

    /// `action` of the published `resource` of `app`, as its contract declares it. The verbs an
    /// accessibility surface publishes for a control (focusing it, pressing it, setting its value)
    /// stand in for the person's own pointer and keyboard, which only they invoke
    /// ([`crate::kind_is_a_drawn_control`]), and need what reading needs: what the control then
    /// writes is decided as those writes are, so a person typing in a field is not asked first as
    /// if the field reached out. A control's own action says what it does and keeps its need.
    #[must_use]
    pub fn published(
        app: &str,
        resource: &crate::ResourceSpec,
        action: &crate::ActionSpec,
    ) -> Self {
        let need = if crate::kind_is_a_drawn_control(&resource.kind)
            && action.operation_type.as_deref() == Some(crate::SEMANTIC_OPERATION_TYPE)
        {
            ActionNeed::of(ActionKind::Read)
        } else {
            action.contract.need()
        };
        Self::new(
            resource.workspace.as_deref(),
            app,
            (&resource.kind, resource.id.as_str(), &action.id),
            &need,
        )
    }

    /// An action of `kind` at `node`, below the window's workspace.
    #[must_use]
    pub const fn at(node: TreePath, kind: ActionKind) -> Self {
        Self {
            workspace: None,
            node,
            kind_needed: kind,
            privileged: false,
        }
    }

    /// An action of `kind` on anything the app `app` keeps in the window's workspace.
    #[must_use]
    pub fn app(app: &str, kind: ActionKind) -> Self {
        Self::at(TreePath::root().app(app), kind)
    }

    /// Whether the app `app` may carry out its outside capability `capability` (`smtp.send`): a
    /// queued effect goes out only once the device says so ([`reach_verdict`]). Only its own
    /// answer answers it, never one about the app's other actions.
    #[must_use]
    pub fn reach(app: &str, capability: &str) -> Self {
        Self::at(
            TreePath::root()
                .app(app)
                .action(format!("{REACH}{capability}")),
            ActionKind::Outside,
        )
    }

    /// The app and capability a [`Self::reach`] question asks about.
    #[must_use]
    pub fn reached(&self) -> Option<(&str, &str)> {
        let app = self.node.id_at(Level::App)?;
        let capability = self.node.id_at(Level::Action)?.strip_prefix(REACH)?;
        Some((app, capability))
    }

    /// One of the workspace's own actions (inviting someone, changing its settings).
    #[must_use]
    pub fn workspace_action(action: &str) -> Self {
        Self::at(
            TreePath::root()
                .kind(tilo_action_policy::roles::WORKSPACE_KIND)
                .action(action),
            tilo_action_policy::roles::GOVERNANCE_KIND,
        )
    }
}

impl ShownAction {
    /// Where the action acts (the app, the kind, the record and item it is on) and what it does
    /// there (the field it changes, the action).
    fn parts(&self) -> (&[Segment], &[Segment]) {
        let segments = self.node.segments();
        let at = segments
            .iter()
            .position(|segment| {
                !matches!(
                    segment.level(),
                    Some(Level::App | Level::Kind | Level::Resource | Level::Item)
                )
            })
            .unwrap_or(segments.len());
        segments.split_at(at)
    }

    /// The same action as its app declares it: of its kind, wherever a record of it stands. What
    /// the device answers once for every record of the kind its grants say nothing else of.
    #[must_use]
    pub fn declared(&self) -> Self {
        let (place, does) = self.parts();
        let declared = place
            .iter()
            .take_while(|segment| matches!(segment.level(), Some(Level::App | Level::Kind)))
            .chain(does)
            .cloned();
        Self {
            node: TreePath::of_segments(declared),
            ..self.clone()
        }
    }

    /// What the device decides this action's answer as declared by: the action on a record of
    /// its kind that no grant names. That is what the answer stands for in the window (a record
    /// it did not ask about), so a grant reaching every record (`**`, a field of each) reaches it
    /// as it reaches them. An action at any other node is decided at its own.
    #[must_use]
    pub fn on_any_record(&self) -> Self {
        let (place, does) = self.parts();
        let declared = matches!(
            place.iter().map(Segment::level).collect::<Vec<_>>()[..],
            [Some(Level::App), Some(Level::Kind)]
        );
        if !declared {
            return self.clone();
        }
        let record = TreePath::of_segments(place.to_vec()).resource(ANY_RECORD);
        Self {
            node: TreePath::of_segments(record.segments().iter().chain(does).cloned()),
            ..self.clone()
        }
    }

    /// The same action at `node` (below the window's workspace), where a grant there may answer
    /// it differently from its declaration: at a node below where it is declared, and, for an
    /// action that names no field of its own, on the field a grant names wherever that grant
    /// reaches (an edit carrying a value of that field, as a field grant on every record says).
    #[must_use]
    pub fn at_node(&self, node: &TreePath) -> Option<Self> {
        let declared = self.declared();
        let (base, does) = declared.parts();
        let place: Vec<Segment> = node
            .segments()
            .iter()
            .take_while(|segment| {
                matches!(
                    segment.level(),
                    Some(Level::App | Level::Kind | Level::Resource | Level::Item)
                )
            })
            .cloned()
            .collect();
        let fields: Vec<Segment> = node
            .segments()
            .iter()
            .skip(place.len())
            .filter(|segment| segment.level() == Some(Level::Field))
            .cloned()
            .collect();
        let (base_path, place_path) = (
            TreePath::of_segments(base.to_vec()),
            TreePath::of_segments(place.clone()),
        );
        let below = place.len() > base.len() && base_path.covers(&place_path);
        let on_field = !fields.is_empty()
            && !does
                .iter()
                .any(|segment| segment.level() == Some(Level::Field))
            && (below || place_path.covers(&base_path));
        let at = if below { place } else { base.to_vec() };
        if on_field {
            return Some(Self {
                node: TreePath::of_segments(
                    at.into_iter().chain(fields).chain(does.iter().cloned()),
                ),
                ..self.clone()
            });
        }
        below.then(|| Self {
            node: TreePath::of_segments(at.into_iter().chain(does.iter().cloned())),
            ..self.clone()
        })
    }

    /// Whether the device's answer of this action, at its node, answers `question`: the same
    /// action at the node or below it, or, for an answer that names no action, any action of its
    /// kind there. How specific it is, when it does: a deeper node first, and at one node the
    /// answer naming the action before the one that names none.
    fn answers(&self, question: &Self) -> Option<usize> {
        let ((place, does), (asked, asked_does)) = (self.parts(), question.parts());
        (self.workspace == question.workspace
            && self.kind_needed == question.kind_needed
            && self.privileged == question.privileged
            && ((does.is_empty() && question.reached().is_none()) || does == asked_does)
            && TreePath::of_segments(place.to_vec()).covers(&TreePath::of_segments(asked.to_vec())))
        .then_some(
            place
                .len()
                .saturating_mul(2)
                .saturating_add(usize::from(!does.is_empty())),
        )
    }
}

/// The actions `app` declares in `resources` (its surface), each at the app's root: what a device
/// answers with a window's launch, so its first frame shows them as decided. Reads need no answer,
/// since a window reads only what its device synced to it.
#[must_use]
pub fn declared_by(app: &str, resources: &[crate::surface::Surface]) -> Vec<ShownAction> {
    let mut actions: Vec<ShownAction> = resources
        .iter()
        .flat_map(|resource| {
            resource
                .verbs
                .iter()
                .filter(|verb| verb.kind != ActionKind::Read)
                .map(|verb| {
                    // A verb the API offers by no name of its own (a bound control's edit, the
                    // resource's creation) is asked by its id, as the window names it.
                    let named = if verb.name.is_empty() {
                        &verb.id
                    } else {
                        &verb.name
                    };
                    ShownAction::at(
                        TreePath::root()
                            .app(app)
                            .kind(resource.kind.clone())
                            .action(named.clone()),
                        verb.kind,
                    )
                })
        })
        .collect();
    // And each kind of change on anything of the app, for an action it publishes none of (one its
    // scenario or its own code calls): the answer of the app's node.
    actions.extend(
        [ActionKind::Undoable, ActionKind::Outside]
            .into_iter()
            .map(|kind| ShownAction::app(app, kind)),
    );
    actions.sort();
    actions.dedup();
    actions
}

/// What the device says of one [`ShownAction`] now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionVerdict {
    /// The person may run it.
    Allowed,
    /// The person may run it once they confirm it.
    AsksFirst,
    /// They may not, and why, as the person reads it: refused by the workspace's grants, or the
    /// workspace is not kept where the device could ask it.
    Refused(String),
}

/// What the workspace `workspace`, with `grants`, says of a [`ShownAction::reach`] question:
/// refused where a grant denying the app's outside reach stops that capability, allowed
/// otherwise. `None` for any other question. Whoever's window asks, it is the app's reach.
#[must_use]
pub fn reach_verdict<'a>(
    question: &ShownAction,
    workspace: &str,
    grants: impl IntoIterator<Item = &'a tilo_action_policy::Grant>,
    cx: &tilo_action_policy::Context<'_>,
) -> Option<ActionVerdict> {
    let (app, capability) = question.reached()?;
    let node = TreePath::workspace(workspace).app(app);
    let stopped = grants.into_iter().any(|grant| {
        tilo_action_policy::roles::outside_denial_stops(grant, app, capability)
            && grant.is_live(cx)
            && cx.covers(&grant.node, &node)
    });
    Some(if stopped {
        ActionVerdict::Refused(REACH_REFUSED.into())
    } else {
        ActionVerdict::Allowed
    })
}

/// What a person reads when the workspace took an app's outside reach back.
pub const REACH_REFUSED: &str = "This workspace doesn't let this app reach outside for this.";

/// A [`ShownAction`] and what the device says of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDecision {
    pub action: ShownAction,
    pub verdict: ActionVerdict,
}

/// The decisions a window asks its device about, and how long it lets the answer wait.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownAsk {
    /// The actions the window shows.
    pub actions: Vec<ShownAction>,
    /// The [`decisions_revision`] of the decisions the window shows now: a device that keeps an
    /// answer waits past it for the grants to change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// How long the device may keep the answer for a change, at most
    /// [`crate::remote::MAX_REMOTE_WAIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

/// What a device says of `actions` now: one decision each, in their order.
#[must_use]
pub fn decide(
    actions: &[ShownAction],
    verdict: impl Fn(&ShownAction) -> ActionVerdict,
) -> Vec<ActionDecision> {
    actions
        .iter()
        .map(|action| ActionDecision {
            action: action.clone(),
            verdict: verdict(action),
        })
        .collect()
}

/// A stable revision of `decisions`: what a window shows them as, and what it names to read on
/// past when it asks again.
///
/// # Panics
/// Never: a decision always encodes.
#[must_use]
pub fn decisions_revision(decisions: &[ActionDecision]) -> String {
    let bytes = serde_json::to_vec(decisions).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

/// The questions a window asked its device, each with the verdict the device last sent, or none
/// yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShownVerdicts {
    verdicts: std::collections::BTreeMap<ShownAction, Option<ActionVerdict>>,
    /// What the device said besides: each asked action as declared, and at each node below it a
    /// grant names. A record no one asked about yet (one made while the window is open) takes the
    /// answer of the nearest of them above it.
    around: std::collections::BTreeMap<ShownAction, ActionVerdict>,
}

impl ShownVerdicts {
    /// The `actions` asked about, each keeping the verdict it had when it was asked about before,
    /// and none yet otherwise.
    #[must_use]
    pub fn asking(&self, actions: impl IntoIterator<Item = ShownAction>) -> Self {
        let verdicts = actions
            .into_iter()
            .map(|action| {
                let verdict = self.verdicts.get(&action).cloned().flatten();
                (action, verdict)
            })
            .collect();
        Self {
            verdicts,
            around: self.around.clone(),
        }
    }

    /// Whether these refuse exactly what `earlier` refused: one asked about since, and decided,
    /// is refused by neither unless it is refused now.
    #[must_use]
    pub fn refuse_as(&self, earlier: &Self) -> bool {
        let refused = |verdicts: &Self, action: &ShownAction| {
            matches!(
                verdicts.verdicts.get(action),
                Some(Some(ActionVerdict::Refused(_)))
            )
        };
        let around = |verdicts: &Self, action: &ShownAction| {
            matches!(verdicts.around.get(action), Some(ActionVerdict::Refused(_)))
        };
        self.verdicts
            .keys()
            .chain(earlier.verdicts.keys())
            .all(|action| refused(self, action) == refused(earlier, action))
            && self
                .around
                .keys()
                .chain(earlier.around.keys())
                .all(|action| around(self, action) == around(earlier, action))
    }

    /// Take the device's `decisions`: those of actions asked about, and what it said around them.
    pub fn decide(&mut self, decisions: &[ActionDecision]) {
        self.around.clear();
        for decision in decisions {
            match self.verdicts.get_mut(&decision.action) {
                Some(verdict) => *verdict = Some(decision.verdict.clone()),
                None => {
                    self.around
                        .insert(decision.action.clone(), decision.verdict.clone());
                }
            }
        }
    }

    /// Whether nothing was asked about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }

    /// Whether `action` was asked about.
    #[must_use]
    pub fn asked(&self, action: &ShownAction) -> bool {
        self.verdicts.contains_key(action)
    }

    /// Whether an action asked about waits for its verdict.
    #[must_use]
    pub fn waits(&self) -> bool {
        self.verdicts.values().any(Option::is_none)
    }

    /// The actions asked about.
    pub fn actions(&self) -> impl Iterator<Item = &ShownAction> {
        self.verdicts.keys()
    }

    /// What the device said of `action`: `None` when it was not asked about, `Some(None)` while
    /// its verdict is on its way.
    #[must_use]
    pub fn verdict(&self, action: &ShownAction) -> Option<Option<&ActionVerdict>> {
        if let Some(verdict) = self.verdicts.get(action) {
            return Some(verdict.as_ref());
        }
        // One not asked about takes the answer the device gave nearest above it.
        self.verdicts
            .iter()
            .filter_map(|(answered, verdict)| Some((answered, verdict.as_ref()?)))
            .chain(&self.around)
            .filter_map(|(answered, verdict)| Some((answered.answers(action)?, verdict)))
            .max_by_key(|(specific, _)| *specific)
            .map(|(_, verdict)| Some(verdict))
    }

    /// What the window shows of `action`: refused until its verdict arrives, and when it was not
    /// asked about.
    #[must_use]
    pub fn answer(&self, action: &ShownAction) -> tilo_action_policy::Answer {
        use tilo_action_policy::Answer;
        match self.verdict(action).flatten() {
            Some(ActionVerdict::Allowed) => Answer::Allow,
            Some(ActionVerdict::AsksFirst) => Answer::Ask,
            Some(ActionVerdict::Refused(_)) | None => Answer::Deny,
        }
    }

    /// Why `action` is refused here, in words a person reads: its verdict's, or that it is still
    /// being checked; `None` when it is not refused.
    #[must_use]
    pub fn refusal(&self, action: &ShownAction) -> Option<&str> {
        match self.verdict(action) {
            None | Some(None) => Some(CHECKING),
            Some(Some(ActionVerdict::Refused(why))) => Some(why),
            Some(Some(_)) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionSpec, OperationKind, ResourceSpec};
    use tilo_action_policy::Answer;

    /// A window's controls show refused while a verdict is on its way, then what the device
    /// said: the device's word, never the window's own, decides.
    #[test]
    fn a_shown_action_is_shown_as_the_device_says() {
        let mut mute =
            ActionSpec::operation("mute", "Mute", OperationKind::Effect, "t.mute", &()).unwrap();
        mute.type_mut().contract.kind = ActionKind::Undoable;
        let playback = ResourceSpec::new("person:test.playback", "test.playback", "Playback")
            .with_action(mute.clone());
        let action = ShownAction::published("board", &playback, &mute);
        let mut shown = ShownVerdicts::default().asking([action.clone()]);
        assert_eq!(
            shown.answer(&action),
            Answer::Deny,
            "refused until its verdict arrives"
        );
        assert_eq!(shown.refusal(&action), Some(CHECKING));
        shown.decide(&[ActionDecision {
            action: action.clone(),
            verdict: ActionVerdict::Allowed,
        }]);
        assert_eq!(shown.answer(&action), Answer::Allow);
        assert_eq!(shown.refusal(&action), None);
        assert_eq!(
            shown.asking([action.clone()]).answer(&action),
            Answer::Allow,
            "asked again as it was, it keeps its verdict"
        );
        let other = ShownAction::app("board", ActionKind::Undoable);
        assert_eq!(
            shown.refusal(&other),
            Some(CHECKING),
            "what was not asked about is not allowed"
        );
        let decisions = [ActionDecision {
            action: action.clone(),
            verdict: ActionVerdict::Refused("no".into()),
        }];
        shown.decide(&decisions);
        assert_eq!(shown.refusal(&action), Some("no"));
        assert_eq!(shown.answer(&action), Answer::Deny);
        assert_eq!(
            decisions_revision(&decisions),
            decisions_revision(&decisions.clone())
        );
    }

    /// A record no one asked about (one made while the window is open) takes the answer of the
    /// nearest node above it the device answered: under a node its grants refuse it is refused,
    /// though the app allows the action, and elsewhere it takes the app's answer. An action the
    /// app publishes none of takes its node's answer of that kind of change.
    #[test]
    fn a_record_no_one_asked_about_takes_the_nearest_answer_above_it() {
        let task = || TreePath::root().app("tasks").kind("tl.tasks.Task");
        let at = |path: TreePath| ShownAction::at(path, ActionKind::Undoable);
        let declared = at(task().action("complete"));
        let kept = at(task().resource("list-b").action("complete"));
        let kept_any = at(task().resource("list-b"));
        let app = ShownAction::app("tasks", ActionKind::Undoable);
        let mut shown = ShownVerdicts::default().asking([declared.clone(), app.clone()]);
        let refused = || ActionVerdict::Refused("kept from you".into());
        shown.decide(&[
            ActionDecision {
                action: declared,
                verdict: ActionVerdict::Allowed,
            },
            ActionDecision {
                action: app,
                verdict: ActionVerdict::Allowed,
            },
            ActionDecision {
                action: kept,
                verdict: refused(),
            },
            ActionDecision {
                action: kept_any,
                verdict: refused(),
            },
        ]);
        let made_under_kept = at(task().resource("list-b").item("new-1").action("complete"));
        assert_eq!(shown.answer(&made_under_kept), Answer::Deny);
        assert_eq!(shown.refusal(&made_under_kept), Some("kept from you"));
        let unpublished_under_kept = at(task().resource("list-b").item("new-1").action("seed"));
        assert_eq!(
            shown.answer(&unpublished_under_kept),
            Answer::Deny,
            "what the app publishes none of under the kept node is refused too"
        );
        let made_elsewhere = at(task().resource("list-a").item("new-2").action("complete"));
        assert_eq!(shown.answer(&made_elsewhere), Answer::Allow);
        let outside = ShownAction::at(
            task().resource("list-a").action("complete"),
            ActionKind::Outside,
        );
        assert_eq!(
            shown.answer(&outside),
            Answer::Deny,
            "an answer of one kind of change says nothing of another"
        );
    }

    /// A control's own effect reaching outside keeps its need, so a guest is asked first; the
    /// accessibility surface's value verb is the person typing into a field, and needs only
    /// reading.
    #[test]
    fn a_buttons_own_effect_keeps_its_need_while_the_screens_value_verb_needs_only_reading() {
        let app = "so.tilo.notes";
        let effect = ActionSpec::operation(
            "invoke",
            "Send",
            OperationKind::Effect,
            "tilo.ui.control.invoke",
            &(),
        )
        .unwrap();
        let control = ResourceSpec::new("tilo.control:send", "tilo.control", "Send")
            .with_action(effect.clone());
        assert_eq!(
            ShownAction::published(app, &control, &effect).kind_needed,
            ActionKind::Outside
        );
        let value = ActionSpec::operation(
            "set",
            "Set",
            OperationKind::Effect,
            crate::SEMANTIC_OPERATION_TYPE,
            &(),
        )
        .unwrap();
        assert_eq!(value.contract.need().kind, ActionKind::Outside);
        let screen =
            ResourceSpec::new("ui:title", "tilo.ui.control", "Title").with_action(value.clone());
        assert_eq!(
            ShownAction::published(app, &screen, &value).kind_needed,
            ActionKind::Read
        );
    }
    /// Whether an app reaches outside is the workspace's say over the app, answered only by its
    /// own question: a denial of the app's whole outside reach refuses every capability, one
    /// naming a capability refuses that one alone, and the person's answer for the app's outside
    /// actions never stands in for it, allowed or not.
    #[test]
    fn an_apps_reach_is_refused_where_the_workspace_took_it_back() {
        use tilo_action_policy::{Context, Grant, Grantee, roles};
        use tilo_action_types::KindSet;
        const MAIL: &str = "so.tilo.mail";
        let send = ShownAction::reach(MAIL, "smtp.send");
        let search = ShownAction::reach(MAIL, "imap.search");
        assert_eq!(send.reached(), Some((MAIL, "smtp.send")));
        let cx = Context::at_secs(0);
        let whole = Grant::new(
            "mail-denied",
            Grantee::app(MAIL),
            TreePath::workspace("w").app(MAIL),
            KindSet::OUTSIDE,
            Answer::Deny,
            Grantee::person("ada"),
        );
        let one = roles::outside_capability_denial("w", MAIL, "smtp.send", Grantee::person("ada"));
        for (grants, sends, searches) in [
            (vec![], true, true),
            (vec![whole], false, false),
            (vec![one], false, true),
        ] {
            let verdict = |question| reach_verdict(question, "w", &grants, &cx);
            assert_eq!(
                verdict(&send) == Some(ActionVerdict::Allowed),
                sends,
                "{grants:?}"
            );
            assert_eq!(
                verdict(&search) == Some(ActionVerdict::Allowed),
                searches,
                "{grants:?}"
            );
        }
        assert_eq!(
            reach_verdict(&ShownAction::app(MAIL, ActionKind::Outside), "w", &[], &cx),
            None,
            "any other question is the person's"
        );
        let mut shown =
            ShownVerdicts::default().asking([ShownAction::app(MAIL, ActionKind::Outside)]);
        shown.decide(&[ActionDecision {
            action: ShownAction::app(MAIL, ActionKind::Outside),
            verdict: ActionVerdict::Allowed,
        }]);
        assert_eq!(
            shown.answer(&send),
            Answer::Deny,
            "the app's outside actions being allowed says nothing of its reach"
        );
    }
}
