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
struct Grants {
    trusted: HashSet<ModuleHash>,
    denied_modules: HashSet<ModuleHash>,
    denied_actions: HashSet<ActionId>,
    granted_outside: HashSet<ActionId>,
}

/// The reference authority (spec 4.3):
///
/// * deny wins over everything;
/// * an outside action asks, unless an explicit grant covers it and every author is trusted;
/// * otherwise the action is allowed when every author in the chain is trusted, the action's
///   own module counting as trusted for its own actions, and asks when not.
///
/// Clones share their grants, so a test or a settings page can keep a handle and change them
/// while the runtime holds another.
#[derive(Clone, Default)]
pub struct SimpleAuthority {
    grants: Arc<Mutex<Grants>>,
}

impl SimpleAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Grants> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Trust the code of this exact module version.
    pub fn trust(&self, module: &ModuleHash) {
        self.lock().trusted.insert(module.clone());
    }

    /// Withdraw trust. Takes effect at the next invocation.
    pub fn revoke(&self, module: &ModuleHash) {
        self.lock().trusted.remove(module);
    }

    pub fn deny_module(&self, module: &ModuleHash) {
        self.lock().denied_modules.insert(module.clone());
    }

    pub fn deny_action(&self, action: &str) {
        self.lock().denied_actions.insert(action.to_owned());
    }

    /// An explicit grant for an outside action.
    pub fn grant_outside(&self, action: &str) {
        self.lock().granted_outside.insert(action.to_owned());
    }

    pub fn revoke_outside(&self, action: &str) {
        self.lock().granted_outside.remove(action);
    }
}

impl Grants {
    fn decide(&self, chain: &[ModuleHash], action: &ActionInfo) -> Decision {
        if self.denied_actions.contains(&action.action)
            || chain.iter().any(|m| self.denied_modules.contains(m))
        {
            return Decision::Deny;
        }
        let all_trusted = chain.iter().all(|m| self.trusted.contains(m));
        if action.class == EffectClass::Outside {
            return if self.granted_outside.contains(&action.action) && all_trusted {
                Decision::Allow
            } else {
                Decision::Ask
            };
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
