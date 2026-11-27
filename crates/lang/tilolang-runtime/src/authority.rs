//! The `Authority` trait: how the runtime asks the platform's policy.
//!
//! Before it runs an action on anyone's behalf (a press, a step of a run, a block), the runtime
//! asks the authority with the invoker, the authorship chain (every code author between the
//! invoker and the action) and what the action is. The answer is Allow, Ask or Deny; Ask
//! suspends the invocation as a suggestion. The rules are authority.md's; the platform decides
//! them in its one policy (`tilolang_tilo::authority::TiloAuthority`, over `tilo_action_policy`).

use crate::ids::{ActionId, Actor, AppId, ModuleHash};
use crate::meta::EffectClass;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

/// What the authority needs to know about the action being invoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionInfo {
    pub action: ActionId,
    pub app: AppId,
    pub class: EffectClass,
    /// The module that wrote the action expression is the module that declares the action.
    pub own: bool,
    /// The module that declares the action.
    pub module: ModuleHash,
    /// The resource the invocation acts on, when it addresses one: its published id and the
    /// kind of its type. The authority decides the action at that node, as the change it makes
    /// is claimed there.
    pub target: Option<ActionTarget>,
    /// The dispatch delivered the call, admitted for its invoker on their device: what that
    /// device says of them was said there. A call made in the process itself (a press in the
    /// window, what code calls on the way) was not.
    pub admitted: bool,
}

/// A resource an action is invoked on, as the policy names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionTarget {
    /// The document it stands in, published (`tl:<app>:<doc>`).
    pub document: String,
    /// The item, when the resource is one of its document's; `None` for the document itself.
    pub item: Option<String>,
    /// The kind of its type (`tl.<app>.<Type>`).
    pub kind: String,
}

pub trait Authority {
    /// `chain` is the authorship chain, outermost first: each code author between the
    /// invoker and the action. It is empty for a call made straight by an actor.
    fn decide(&self, invoker: &Actor, chain: &[ModuleHash], action: &ActionInfo) -> Decision;

    /// Whether `invoker`'s call of `action` through `chain` waits on their device, which has not
    /// said yet what they may do of it, while nothing here refuses it: the call goes to the
    /// dispatch, which decides it on the device and runs it, asks them, or refuses it there. A
    /// runtime whose person's device decides nothing here has none.
    fn undecided(&self, invoker: &Actor, chain: &[ModuleHash], action: &ActionInfo) -> bool {
        let _ = (invoker, chain, action);
        false
    }

    /// Whether a rule asks `invoker` first before `action` runs: one of their own, or the
    /// workspace's for them, apart from what the code that led there may do. A person's own
    /// press answers what its code asks (they pressed it); only such a rule waits for their yes
    /// on the system's confirm. A runtime without a platform has no such rules.
    fn asks_first(&self, invoker: &Actor, action: &ActionInfo) -> bool {
        let _ = (invoker, action);
        false
    }

    /// Whether the workspace still lets `app`'s code reach `host` with `uses web`. What the
    /// package declares is checked before this ([`crate::web::granted`]); a workspace that
    /// revoked a host refuses it here, and the request answers as a value.
    fn may_fetch(&self, _app: &str, _host: &str) -> bool {
        true
    }

    /// Whether `app` may carry out the outside effect `capability` (`smtp.send`) that the
    /// workspace queued: what the workspace granted it, judged when the effect is carried out
    /// rather than when it is queued. An entry the workspace does not let the app carry out
    /// settles as refused, and never reaches the sink. `None` while whoever decides it has not
    /// said yet: the entry stays queued until they do.
    fn may_reach(&self, _app: &str, _capability: &str) -> Option<bool> {
        Some(true)
    }
}
