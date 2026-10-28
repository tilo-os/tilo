//! [`TiloAuthority`]: who may invoke what on the platform (implementation design 7.5).
//!
//! Two questions, and the minimum wins:
//!
//! 1. **The invoker, inside Tilo.** What the person (or the agent task acting for them) may do
//!    comes from the host's permissions; the host supplies it as a verdict function.
//! 2. **The authorship, inside tilolang.** The runtime's own rules (spec 4.3): own actions are
//!    allowed, foreign ones depend on whether every code author in the chain is trusted. Built-in
//!    packages are trusted; an agent's task code takes its task's grants; a stranger's code asks.
//!
//! An Ask is rendered by the host as trusted chrome, never by wasm.

use std::rc::Rc;

use tilolang_runtime::{ActionInfo, Authority, Decision, ModuleHash, Principal, SimpleAuthority};

/// What the platform says of an invoker's own permission for an action.
pub type InvokerVerdict = dyn Fn(&Principal, &ActionInfo) -> Decision;

/// The platform's authority. Clones share their grants.
#[derive(Clone)]
pub struct TiloAuthority {
    authorship: SimpleAuthority,
    invoker: Rc<InvokerVerdict>,
}

const fn rank(decision: Decision) -> u8 {
    match decision {
        Decision::Deny => 0,
        Decision::Ask => 1,
        Decision::Allow => 2,
    }
}

/// The stricter of two decisions.
#[must_use]
pub const fn stricter(a: Decision, b: Decision) -> Decision {
    if rank(a) <= rank(b) { a } else { b }
}

impl TiloAuthority {
    /// An authority that asks `invoker` for the person's own verdict.
    pub fn new(invoker: impl Fn(&Principal, &ActionInfo) -> Decision + 'static) -> Self {
        Self {
            authorship: SimpleAuthority::new(),
            invoker: Rc::new(invoker),
        }
    }

    /// An authority whose invoker is allowed everything the code authorship allows: for tests
    /// and for hosts that decide permissions elsewhere.
    #[must_use]
    pub fn authorship_only() -> Self {
        Self::new(|_, _| Decision::Allow)
    }

    /// Trust the code of this exact module version (a built-in package).
    pub fn trust(&self, module: &ModuleHash) {
        self.authorship.trust(module);
    }

    /// Withdraw trust. Takes effect at the next invocation.
    pub fn revoke(&self, module: &ModuleHash) {
        self.authorship.revoke(module);
    }

    /// The grants the runtime's own rules read, to change them (an agent task's grants, an
    /// explicit grant of an outside action).
    #[must_use]
    pub const fn grants(&self) -> &SimpleAuthority {
        &self.authorship
    }
}

impl Authority for TiloAuthority {
    fn decide(&self, invoker: &Principal, chain: &[ModuleHash], action: &ActionInfo) -> Decision {
        stricter(
            (self.invoker)(invoker, action),
            self.authorship.decide(invoker, chain, action),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stricter_decision_wins() {
        use Decision::{Allow, Ask, Deny};
        assert_eq!(stricter(Allow, Ask), Ask);
        assert_eq!(stricter(Deny, Allow), Deny);
        assert_eq!(stricter(Ask, Deny), Deny);
        assert_eq!(stricter(Allow, Allow), Allow);
    }
}
