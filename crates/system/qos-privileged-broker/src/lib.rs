//! The qOS privileged broker — a trusted helper that performs privileged
//! system operations on a sandboxed app's behalf, over a mediated channel.
//!
//! ## The boundary this exists to hold
//!
//! A qOS app runs in a per-workspace sandbox that has no route to the
//! kernel's privileged surface: no `nft`, no block-device node, no
//! `CAP_NET_ADMIN`. That is the point — the sandbox is the security boundary
//! and it is not weakened for convenience. But a firewall front-end must
//! change firewall rules, a disk tool must write a device, a VPN app must
//! bring up a tunnel. The broker is how those happen *without* handing the
//! sandboxed app the privilege: the app sends a **typed request** ("apply
//! this firewall plan") over a Unix socket the launcher plumbs in; the broker
//! runs outside the sandbox, validates the request, and performs the
//! operation itself. The app never touches `nft`/`firewall-cmd`/the device.
//!
//! ## Three gates, deny-by-default at each
//!
//! Every request passes three gates before anything happens, and failing any
//! one is a refusal with no side effect:
//!
//! 1. **Declared.** The app's `.qpkg` manifest must declare the capability
//!    the request needs (`[capabilities]`, see [`pkg::capabilities`]). A
//!    request for a capability the manifest did not declare is
//!    [`DenyReason::Undeclared`]. This is the install-time ceiling.
//! 2. **Well-formed.** The request's contents must pass a shape check that is
//!    an allowlist, not a filter: the firewall applier will run *only*
//!    `firewall-cmd`, so a plan line naming any other binary is
//!    [`DenyReason::Malformed`] and never reaches a shell. A consented app
//!    still cannot make the broker run an arbitrary command.
//! 3. **Consented.** The user must have granted the capability in the
//!    workspace. Consent is an ordinary [`collab_types::caps::CapOverride`]
//!    on the capability's scope, evaluated by the same
//!    [`collab_types::caps::decide_capability`] the compositor and filesystem
//!    use — there is no second permission vocabulary. No grant is
//!    [`DenyReason::NoConsent`], and no action.
//!
//! Only after all three does the broker apply the operation, through a
//! pluggable [`FirewallApplier`] so the real `nft`/`firewall-cmd` boundary
//! can be mocked in a test while the authorization path stays real.

use collab_types::caps::{
    Actor, CapDecision, CapOverride, ResourceRef, Scope, decide_capability,
};
use pkg::capabilities::PRIVILEGED_VERB;
use serde::{Deserialize, Serialize};

pub mod firewall;
pub mod wire;

pub use firewall::{FirewallApplier, RecordingFirewall, SystemFirewall};
pub use wire::{serve_connection, BrokerClient, WireError};

// Re-export the manifest capability vocabulary so a consumer (the compositor
// host, an app-side test) can name a declared capability without also
// depending on `pkg` directly.
pub use pkg::capabilities::{Capabilities, PrivilegedCapability, PrivilegedRequest};

/// A typed request an app sends the broker.
///
/// One variant per privileged capability that has a live applier. The others
/// (`block-device`, `raw-device-write`, `container-runtime`, `vpn-tun`,
/// `packet-capture`) are declarable in a manifest today but their appliers
/// are staged, so they have no request variant yet — an app cannot send an
/// operation the broker has no code to perform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrokerRequest {
    /// Apply a firewall plan — an ordered list of `firewall-cmd` command
    /// lines the app computed from the ruleset the user edited.
    Firewall(FirewallOp),
}

/// A firewall mutation: the ordered `firewall-cmd` command lines to run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallOp {
    /// Each entry is one full command line, e.g.
    /// `firewall-cmd --permanent --zone=public --add-port=443/tcp`.
    pub plan: Vec<String>,
}

impl BrokerRequest {
    /// The privileged capability this request needs — the one the manifest
    /// must have declared and the user must have consented to.
    pub fn required_capability(&self) -> PrivilegedCapability {
        match self {
            BrokerRequest::Firewall(_) => PrivilegedCapability::FirewallAdmin,
        }
    }

    /// Shape validation — gate 2. An allowlist of what the broker will do for
    /// this capability, so a consented app still cannot smuggle an arbitrary
    /// command through.
    pub fn validate_shape(&self) -> Result<(), String> {
        match self {
            BrokerRequest::Firewall(op) => op.validate_shape(),
        }
    }
}

impl FirewallOp {
    /// The firewall applier runs `firewall-cmd` and nothing else. A plan that
    /// is empty, or that names any other binary as its first token, is
    /// refused here — before the applier, before any exec.
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.plan.is_empty() {
            return Err("firewall plan is empty".to_string());
        }
        for line in &self.plan {
            let bin = line.split_whitespace().next().unwrap_or("");
            if bin != "firewall-cmd" {
                return Err(format!(
                    "refusing a firewall plan line that is not a firewall-cmd invocation: {line:?}"
                ));
            }
        }
        Ok(())
    }
}

/// The broker's reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrokerResponse {
    /// The operation was authorised and applied. `commands` is how many
    /// underlying commands ran.
    Applied { commands: usize },
    /// The operation was refused at one of the three gates. No side effect.
    Denied(DenyReason),
    /// The operation was authorised but the applier failed part-way. The
    /// string is the underlying error; the system may be partially changed,
    /// exactly as the underlying tool left it.
    Failed(String),
}

/// Why a request was refused. Each is a distinct gate, so the app (and the
/// audit log) can tell "you never asked for this" from "the user said no".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenyReason {
    /// The app's manifest did not declare this capability. The install-time
    /// ceiling: no amount of consent can rescue it, because the package never
    /// claimed to need it.
    Undeclared(PrivilegedCapability),
    /// The request's contents failed the shape allowlist. The string names
    /// what was wrong.
    Malformed(String),
    /// The user has not consented to this capability in the workspace.
    NoConsent(PrivilegedCapability),
}

impl DenyReason {
    /// A human sentence for logs and the app's status line.
    pub fn message(&self) -> String {
        match self {
            DenyReason::Undeclared(c) => format!(
                "the package does not declare the `{}` capability",
                c.wire()
            ),
            DenyReason::Malformed(why) => format!("malformed request: {why}"),
            DenyReason::NoConsent(c) => {
                format!("the user has not allowed this app to {}", c.consent_phrase())
            }
        }
    }
}

/// The outcome of [`authorize`] — the three gates, separated from the actual
/// application so the decision is testable on its own and the applier is only
/// reached once every gate has passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authorization {
    /// Every gate passed; the named capability may be exercised.
    Allowed(PrivilegedCapability),
    /// A gate refused it.
    Denied(DenyReason),
}

/// Run the three gates for `request` made by `app_id`.
///
/// This is the whole security decision, and it does no I/O and applies
/// nothing — [`Broker::handle`] calls it and only reaches the applier on
/// [`Authorization::Allowed`]. Keeping it standalone means a test can prove
/// "undeclared is refused" and "consent-denied is refused" without an
/// applier at all.
pub fn authorize(
    app_id: &str,
    declared: &Capabilities,
    consent: &dyn ConsentOracle,
    request: &BrokerRequest,
) -> Authorization {
    let capability = request.required_capability();

    // Gate 1 — the install-time ceiling.
    if !declared.declares(capability) {
        return Authorization::Denied(DenyReason::Undeclared(capability));
    }

    // Gate 2 — the shape allowlist.
    if let Err(why) = request.validate_shape() {
        return Authorization::Denied(DenyReason::Malformed(why));
    }

    // Gate 3 — the user's consent.
    if !consent.consents(app_id, capability) {
        return Authorization::Denied(DenyReason::NoConsent(capability));
    }

    Authorization::Allowed(capability)
}

/// The broker: it owns the appliers and turns a request into a response.
///
/// Generic over the firewall applier so tests substitute a recorder for the
/// real `firewall-cmd` boundary while every gate in [`authorize`] runs for
/// real.
pub struct Broker<F: FirewallApplier> {
    firewall: F,
}

impl<F: FirewallApplier> Broker<F> {
    /// Build a broker over a firewall applier.
    pub fn new(firewall: F) -> Self {
        Self { firewall }
    }

    /// Authorise `request` from `app_id` against the app's `declared`
    /// capabilities and the `consent` oracle, and — only if every gate passes
    /// — apply it.
    pub fn handle(
        &mut self,
        app_id: &str,
        declared: &Capabilities,
        consent: &dyn ConsentOracle,
        request: &BrokerRequest,
    ) -> BrokerResponse {
        match authorize(app_id, declared, consent, request) {
            Authorization::Denied(reason) => {
                tracing::info!(
                    app = app_id,
                    reason = ?reason,
                    "privileged request denied"
                );
                BrokerResponse::Denied(reason)
            }
            Authorization::Allowed(capability) => {
                tracing::info!(
                    app = app_id,
                    capability = capability.wire(),
                    "privileged request authorised; applying"
                );
                match request {
                    BrokerRequest::Firewall(op) => match self.firewall.apply(&op.plan) {
                        Ok(commands) => BrokerResponse::Applied { commands },
                        Err(e) => BrokerResponse::Failed(e),
                    },
                }
            }
        }
    }

    /// Consume the broker and return its firewall applier — so a test that
    /// moved the broker onto another thread (see [`wire::serve_connection`])
    /// can recover the recorder and assert on what it applied.
    pub fn into_firewall_for_test(self) -> F {
        self.firewall
    }
}

/// Whether the user has consented to an app exercising a privileged
/// capability — the third gate.
///
/// The production implementation consults the workspace's capability list
/// (and may raise the consent prompt); tests use a fixed answer or a hand-
/// built capability list. The trait is deliberately synchronous and boolean:
/// a `false` means "no action", whatever the reason.
pub trait ConsentOracle {
    /// Has the user consented to `app_id` exercising `capability` right now?
    fn consents(&self, app_id: &str, capability: PrivilegedCapability) -> bool;
}

/// Consent read straight out of a workspace's capability list — **the
/// production decision path.**
///
/// This asks [`decide_capability`] exactly the question the compositor asks
/// about a camera or a network host: may this app, as an actor holding no
/// role, exercise [`PRIVILEGED_VERB`] on the capability's scope? A standing
/// `CapOverride::grant(Subject::app(id), capability.scope(), Verb::Admin)` —
/// the record the consent prompt stores — makes it `Allow`; anything less is
/// a refusal. Because privileged scopes carry no role base authority, absence
/// of a grant always denies, for an Owner as much as a guest.
pub struct CapListConsent<'a> {
    /// The workspace's capabilities. In the compositor these are
    /// `Workspace::capabilities`; in a test they are hand-built.
    pub caps: &'a [CapOverride],
    /// Wall-clock now (seconds since the Unix epoch) for expiry evaluation,
    /// or `None` to skip it.
    pub now: Option<u64>,
}

impl<'a> CapListConsent<'a> {
    /// Build a consent oracle over a capability list.
    pub fn new(caps: &'a [CapOverride], now: Option<u64>) -> Self {
        Self { caps, now }
    }
}

impl ConsentOracle for CapListConsent<'_> {
    fn consents(&self, app_id: &str, capability: PrivilegedCapability) -> bool {
        let actor = Actor::app(app_id);
        let resource = scope_to_resource(&capability.scope());
        decide_capability(&actor, &resource, PRIVILEGED_VERB, self.caps, self.now)
            == CapDecision::Allow
    }
}

/// Map a capability [`Scope`] to the [`ResourceRef`] the decision function
/// asks about. A privileged capability is a `Scope::Device`, so it becomes a
/// `ResourceRef::Device` — the same resource kind the launcher and portal use
/// for camera / microphone, and one [`collab_types::caps::role_base_applies`]
/// gives no role base, so it always requires an explicit grant.
fn scope_to_resource(scope: &Scope) -> ResourceRef {
    match scope {
        Scope::Device(d) => ResourceRef::Device(d.clone()),
        Scope::FsPath(p) => ResourceRef::FsPath(p.clone()),
        Scope::NetHost(h) => ResourceRef::NetHost(h.clone()),
        Scope::Workspace => ResourceRef::Workspace,
        Scope::App(a) => ResourceRef::App(a.clone()),
        Scope::Room(r) => ResourceRef::Room(r.clone()),
        Scope::Action { app, action } => ResourceRef::Action {
            app: app.clone(),
            action: action.clone(),
        },
        // Artifact-shaped scopes never name a privileged capability; treat
        // them as the workspace so the decision is well-defined and, being
        // role-based, still requires the grant that a privileged scope would.
        Scope::ResourceType(_) | Scope::ResourceInstance(_) => ResourceRef::Workspace,
    }
}

/// A [`ConsentOracle`] with a fixed answer, for tests and the "no compositor"
/// fallback (where it answers `false`: no consent surface means no action).
pub struct FixedConsent(pub bool);

impl ConsentOracle for FixedConsent {
    fn consents(&self, _app_id: &str, _capability: PrivilegedCapability) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{CapOverride, Subject, Verb};

    fn firewall_plan() -> BrokerRequest {
        BrokerRequest::Firewall(FirewallOp {
            plan: vec![
                "firewall-cmd --set-default-zone=public".to_string(),
                "firewall-cmd --permanent --zone=public --add-port=443/tcp".to_string(),
                "firewall-cmd --reload".to_string(),
            ],
        })
    }

    fn declares_firewall() -> Capabilities {
        Capabilities {
            privileged: vec![pkg::capabilities::PrivilegedRequest::new(
                PrivilegedCapability::FirewallAdmin,
            )],
        }
    }

    /// A grant the consent prompt would store when the user clicks Allow.
    fn firewall_grant(app: &str) -> CapOverride {
        CapOverride::grant(
            Subject::app(app),
            PrivilegedCapability::FirewallAdmin.scope(),
            Verb::Admin,
        )
    }

    #[test]
    fn undeclared_capability_is_denied_before_consent() {
        // The app consents to everything, but never declared firewall-admin.
        // The ceiling refuses it regardless.
        let declared = Capabilities::default();
        let mut broker = Broker::new(RecordingFirewall::default());
        let resp = broker.handle("dev.qos.rogue", &declared, &FixedConsent(true), &firewall_plan());
        assert_eq!(
            resp,
            BrokerResponse::Denied(DenyReason::Undeclared(PrivilegedCapability::FirewallAdmin))
        );
        assert!(
            broker.firewall.applied.is_empty(),
            "an undeclared request must never reach the applier"
        );
    }

    #[test]
    fn declared_and_consented_firewall_rule_applies_via_the_real_path() {
        // Declared in the manifest, consented by a real CapOverride the
        // compositor grant flow would store, evaluated by the real
        // decide_capability inside CapListConsent. Only the nft boundary is
        // mocked.
        let declared = declares_firewall();
        let caps = vec![firewall_grant("dev.qos.firewall")];
        let consent = CapListConsent::new(&caps, Some(1_000));
        let mut broker = Broker::new(RecordingFirewall::default());
        let resp = broker.handle("dev.qos.firewall", &declared, &consent, &firewall_plan());
        assert_eq!(resp, BrokerResponse::Applied { commands: 3 });
        assert_eq!(
            broker.firewall.applied.len(),
            1,
            "the applier must have run exactly once"
        );
        assert_eq!(broker.firewall.applied[0].len(), 3);
    }

    #[test]
    fn consent_denied_means_no_action() {
        // Declared and well-formed, but the workspace holds no grant for this
        // app: decide_capability denies, and nothing is applied.
        let declared = declares_firewall();
        let consent = CapListConsent::new(&[], Some(1_000));
        let mut broker = Broker::new(RecordingFirewall::default());
        let resp = broker.handle("dev.qos.firewall", &declared, &consent, &firewall_plan());
        assert_eq!(
            resp,
            BrokerResponse::Denied(DenyReason::NoConsent(PrivilegedCapability::FirewallAdmin))
        );
        assert!(
            broker.firewall.applied.is_empty(),
            "a consent-denied request must never reach the applier"
        );
    }

    #[test]
    fn another_apps_grant_does_not_consent_this_app() {
        // The grant names a different app id, so this app is still denied —
        // proving CapListConsent matches the subject, not merely the scope.
        let declared = declares_firewall();
        let caps = vec![firewall_grant("dev.qos.other")];
        let consent = CapListConsent::new(&caps, Some(1_000));
        let mut broker = Broker::new(RecordingFirewall::default());
        let resp = broker.handle("dev.qos.firewall", &declared, &consent, &firewall_plan());
        assert_eq!(
            resp,
            BrokerResponse::Denied(DenyReason::NoConsent(PrivilegedCapability::FirewallAdmin))
        );
    }

    #[test]
    fn a_grant_for_another_capability_does_not_consent_firewall() {
        // A block-device grant is a real CapOverride, but on a different
        // scope; it must not authorise a firewall change.
        let declared = declares_firewall();
        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.firewall"),
            PrivilegedCapability::BlockDevice.scope(),
            Verb::Admin,
        )];
        let consent = CapListConsent::new(&caps, Some(1_000));
        let mut broker = Broker::new(RecordingFirewall::default());
        let resp = broker.handle("dev.qos.firewall", &declared, &consent, &firewall_plan());
        assert_eq!(
            resp,
            BrokerResponse::Denied(DenyReason::NoConsent(PrivilegedCapability::FirewallAdmin))
        );
    }

    #[test]
    fn a_plan_naming_another_binary_is_malformed_and_never_runs() {
        // Declared and consented, but the plan tries to run something other
        // than firewall-cmd. The shape allowlist refuses it before the
        // applier — a consented app still cannot run an arbitrary command.
        let declared = declares_firewall();
        let caps = vec![firewall_grant("dev.qos.firewall")];
        let consent = CapListConsent::new(&caps, Some(1_000));
        let mut broker = Broker::new(RecordingFirewall::default());
        let evil = BrokerRequest::Firewall(FirewallOp {
            plan: vec!["rm -rf /".to_string()],
        });
        let resp = broker.handle("dev.qos.firewall", &declared, &consent, &evil);
        assert!(matches!(resp, BrokerResponse::Denied(DenyReason::Malformed(_))));
        assert!(
            broker.firewall.applied.is_empty(),
            "a malformed plan must never reach the applier"
        );
    }

    #[test]
    fn an_expired_grant_does_not_consent() {
        // The grant was live but has lapsed; expiry drops it and the app is
        // denied — the same expiry semantics as every other capability.
        let declared = declares_firewall();
        let caps = vec![firewall_grant("dev.qos.firewall").expiring(
            collab_types::caps::Expiry::AtUnix(500),
        )];
        let mut broker = Broker::new(RecordingFirewall::default());

        let live = CapListConsent::new(&caps, Some(499));
        assert_eq!(
            broker.handle("dev.qos.firewall", &declared, &live, &firewall_plan()),
            BrokerResponse::Applied { commands: 3 }
        );

        let lapsed = CapListConsent::new(&caps, Some(501));
        assert_eq!(
            broker.handle("dev.qos.firewall", &declared, &lapsed, &firewall_plan()),
            BrokerResponse::Denied(DenyReason::NoConsent(PrivilegedCapability::FirewallAdmin))
        );
    }
}
