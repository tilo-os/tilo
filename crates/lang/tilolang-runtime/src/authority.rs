//! The `Authority` trait (implementation design 4.7) and the reference `SimpleAuthority`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::ids::{ActionId, AppId, ModuleHash, Principal};
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
}

pub trait Authority {
    /// `chain` is the authorship chain, outermost first: each code author between the
    /// invoker and the action. It is empty for a call made straight by a principal.
    fn decide(&self, invoker: &Principal, chain: &[ModuleHash], action: &ActionInfo) -> Decision;
}

#[derive(Default)]
struct Trust {
    trusted: HashSet<ModuleHash>,
}

/// The authorship rule of the spec (4.3), for a runtime without a platform (its own tests, a
/// benchmark): an outside action asks; otherwise the action is allowed when every author in the
/// chain is trusted, the action's own module counting as trusted for its own actions, and asks
/// when not.
///
/// What a person may do, the denials and grants of a room and the answers a person gave their
/// agents are not the runtime's: a platform decides them in its one policy, with the code's
/// authorship as one more input (`tilolang_tilo::authority::TiloAuthority`).
///
/// Clones share their trust, so a test can keep a handle and change it while the runtime holds
/// another.
#[derive(Clone, Default)]
pub struct SimpleAuthority {
    trust: Arc<Mutex<Trust>>,
}

impl SimpleAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Trust> {
        self.trust.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Trust the code of this exact module version.
    pub fn trust(&self, module: &ModuleHash) {
        self.lock().trusted.insert(module.clone());
    }

    /// Whether the code of this module is trusted.
    pub fn is_trusted(&self, module: &ModuleHash) -> bool {
        self.lock().trusted.contains(module)
    }

    /// Trust the code a person or an agent task writes (an embedded block's author).
    pub fn trust_principal(&self, id: &str) {
        self.trust(&ModuleHash::principal(id));
    }

    pub fn revoke_principal(&self, id: &str) {
        self.revoke(&ModuleHash::principal(id));
    }

    /// Withdraw trust. Takes effect at the next invocation.
    pub fn revoke(&self, module: &ModuleHash) {
        self.lock().trusted.remove(module);
    }
}

impl Trust {
    fn decide(&self, chain: &[ModuleHash], action: &ActionInfo) -> Decision {
        if action.class == EffectClass::Outside {
            return Decision::Ask;
        }
        // An own invocation is free for the declaring module; every other author in the
        // chain still needs trust (the minimum along the whole chain, spec 4.3).
        let covered =
            |m: &ModuleHash| self.trusted.contains(m) || (action.own && *m == action.module);
        if chain.iter().all(covered) {
            Decision::Allow
        } else {
            Decision::Ask
        }
    }
}

impl Authority for SimpleAuthority {
    fn decide(&self, _invoker: &Principal, chain: &[ModuleHash], action: &ActionInfo) -> Decision {
        self.lock().decide(chain, action)
    }
}
