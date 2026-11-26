//! What the power menu offers, and what choosing a row does: the session's own
//! effects, which the shell draws as its power sheet.
//!
//! The menu is the session's because only it knows what this session may
//! offer — a session in another desktop's window owns no machine, and one that
//! cannot check a password is never locked — and because locking, logging out
//! and the rest act on the session itself rather than on a document. The shell
//! reads the offers here through `desktop:power-menu` and draws a row for
//! each; choosing one arrives back here as the same effect the session's host
//! actions name.

use tilo_ui_base::icons::Icon;

/// What the person can do to their session and their computer. Each acts on the
/// device it is invoked on and on nobody else's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    /// Lock
    Lock,
    /// Log out
    LogOut,
    /// Switch user
    SwitchUser,
    /// Suspend
    Suspend,
    /// Restart
    Restart,
    /// Shut down
    ShutDown,
}

impl Effect {
    /// Every effect, in the order the menu shows them.
    pub(crate) const ALL: [Self; 6] = [
        Self::Lock,
        Self::LogOut,
        Self::SwitchUser,
        Self::Suspend,
        Self::Restart,
        Self::ShutDown,
    ];

    /// The effect's name, as the reading and the verbs spell it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::LogOut => "log-out",
            Self::SwitchUser => "switch-user",
            Self::Suspend => "suspend",
            Self::Restart => "restart",
            Self::ShutDown => "shut-down",
        }
    }

    /// The glyph the menu draws for it: the same one every other list of the
    /// same effect wears.
    #[must_use]
    pub(crate) const fn icon(self) -> Icon {
        match self {
            Self::Lock => Icon::Lock,
            Self::LogOut => Icon::ArrowRight,
            Self::SwitchUser => Icon::Users,
            Self::Suspend => Icon::Moon,
            Self::Restart => Icon::RotateCw,
            Self::ShutDown => Icon::Power,
        }
    }
}

/// What this session may offer: plain data the session works out from the machine
/// it draws on and the password it can check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offers {
    /// The display manager can open a greeter beside this session. Without it,
    /// Switch user is not offered at all rather than failing.
    pub user_switching: bool,
    /// This session owns the machine's power. A headless or nested session shares
    /// the host with another desktop: it offers no switching user, suspending or
    /// restarting, and shutting down ends only itself.
    pub host_power: bool,
    /// A password unlocks this session, and a screen takes it, so its lock can be
    /// left again. Without either, locking is not offered at all rather than
    /// leaving the person on a screen nothing opens.
    pub locking: bool,
}

impl Default for Offers {
    fn default() -> Self {
        Self {
            user_switching: true,
            host_power: true,
            locking: true,
        }
    }
}

impl Offers {
    /// Whether `effect` is offered here.
    pub(crate) fn offers(self, effect: Effect) -> bool {
        match effect {
            Effect::SwitchUser => self.user_switching && self.host_power,
            Effect::Suspend | Effect::Restart => self.host_power,
            Effect::Lock => self.locking,
            // Shut down is offered everywhere: a session that owns no machine ends
            // itself when it is chosen.
            Effect::LogOut | Effect::ShutDown => true,
        }
    }

    /// The offered effects, in the order the menu shows them.
    pub(crate) fn offered(self) -> Vec<Effect> {
        Effect::ALL
            .into_iter()
            .filter(|effect| self.offers(*effect))
            .collect()
    }
}
