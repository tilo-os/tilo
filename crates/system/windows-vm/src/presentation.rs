//! Explicit presentation choices for the singleton Windows appliance.
//!
//! RAIL is a property of the guest RDP server *and* the local RDP client
//! surface.  It is not something qOS can manufacture by passing `/app` to an
//! arbitrary viewer.  Keeping this decision as a small typed value prevents a
//! failed RAIL connection from silently turning into a full desktop.

use std::fmt;

/// The only normal application presentation modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationPresentation {
    /// FreeRDP's upstream RAIL client, when qOS has a RAIL-capable local
    /// surface (for example an Xwayland bridge or a future native adapter).
    NativeRail,
    /// A guest-focused SPICE surface. The guest helper starts/focuses the
    /// selected application before the viewer is attached. This is a whole
    /// guest surface (with the requested app focused), not seamless windows;
    /// it does not expose a qOS-wide desktop route.
    AppFocusedSpice,
}

impl ApplicationPresentation {
    /// Selects a normal application surface from capabilities that have been
    /// probed by qOS.  The fallback is explicit and deterministic.
    #[must_use]
    pub fn select(has_rail_surface: bool) -> Self {
        if has_rail_surface {
            Self::NativeRail
        } else {
            Self::AppFocusedSpice
        }
    }
}

impl fmt::Display for ApplicationPresentation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NativeRail => "native RAIL",
            Self::AppFocusedSpice => "app-focused whole-surface SPICE",
        })
    }
}

/// Recovery is intentionally not part of [`ApplicationPresentation`].
/// Opening a full Windows desktop must require a separate owner-authorised
/// action and can never be selected by application routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryPresentation {
    OwnerAuthorisedDesktop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_is_preferred_only_when_a_local_surface_exists() {
        assert_eq!(
            ApplicationPresentation::select(true),
            ApplicationPresentation::NativeRail
        );
        assert_eq!(
            ApplicationPresentation::select(false),
            ApplicationPresentation::AppFocusedSpice
        );
    }
}
