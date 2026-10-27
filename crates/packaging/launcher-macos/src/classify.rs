//! Which sandbox an app gets, decided from its code signature.
//!
//! The signature is inspected once (`crate::codesign::inspect` on macOS)
//! into a [`CodeSignature`]; [`decide`] maps that onto a [`MacLaunchMode`].
//! Keeping the decision pure means every row of the table below is a unit
//! test on the function production calls, on any host.
//!
//! | signature | trusted signer | app-sandbox entitlement | mode |
//! |---|---|---|---|
//! | none | — | — | Seatbelt |
//! | unreadable / API failure | — | — | Seatbelt |
//! | present, does not verify | — | — | Seatbelt |
//! | ad-hoc | — | any | Seatbelt |
//! | identified | no | any | Seatbelt |
//! | identified | yes | absent or not `true` | Seatbelt |
//! | identified | yes | `true` | App Sandbox |

/// The entitlement that makes the kernel confine a process to App Sandbox.
pub const APP_SANDBOX_ENTITLEMENT: &str = "com.apple.security.app-sandbox";

/// Certificate chain of an Apple Developer ID Application signature: an
/// Apple root, the Developer ID intermediate (`1.2.840.113635.100.6.2.6`)
/// and a Developer ID Application leaf (`1.2.840.113635.100.6.1.13`).
const DEVELOPER_ID_ANCHOR: &str = "anchor apple generic \
     and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
     and certificate leaf[field.1.2.840.113635.100.6.1.13] exists";

/// Whose signature is trusted to describe its own sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustRequirement {
    DeveloperId,
    /// A Developer ID Application signature issued to this team only.
    DeveloperIdTeam(TeamId),
    /// A literal requirement, so tests can reach the App Sandbox verdict
    /// with signatures that exist on every Mac (Apple's own apps).
    #[cfg(test)]
    Custom(&'static str),
}

impl TrustRequirement {
    /// The requirement a payload must satisfy on disk before it is launched
    /// outside Seatbelt: the signer's identity, and a notarization ticket —
    /// the same `notarized` clause Gatekeeper evaluates.
    pub fn static_requirement(&self) -> String {
        match self {
            #[cfg(test)]
            TrustRequirement::Custom(text) => (*text).to_string(),
            _ => format!("{} and notarized", self.identity_requirement()),
        }
    }

    /// The requirement the *running* process must satisfy before it is
    /// resumed.
    ///
    /// Identity only: what makes the kernel enforce App Sandbox is a valid
    /// signature carrying the entitlement, so that is what must hold for the
    /// image actually exec'd. Notarization is a property of the file, already
    /// checked by [`Self::static_requirement`].
    pub fn identity_requirement(&self) -> String {
        match self {
            TrustRequirement::DeveloperId => DEVELOPER_ID_ANCHOR.to_string(),
            TrustRequirement::DeveloperIdTeam(team) => format!(
                "{DEVELOPER_ID_ANCHOR} and certificate leaf[subject.OU] = \"{}\"",
                team.as_str()
            ),
            #[cfg(test)]
            TrustRequirement::Custom(text) => (*text).to_string(),
        }
    }
}

/// An Apple Developer team identifier: ten uppercase ASCII letters or digits.
///
/// Validated because it is spliced into requirement text; nothing else can
/// reach that string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamId(String);

/// A team identifier that is not ten uppercase ASCII letters or digits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not an Apple team identifier (ten uppercase letters or digits)")]
pub struct InvalidTeamId(pub String);

impl TeamId {
    /// # Errors
    ///
    /// Returns [`InvalidTeamId`] unless `raw` is exactly ten uppercase ASCII
    /// letters or digits.
    pub fn new(raw: &str) -> Result<Self, InvalidTeamId> {
        let valid = raw.len() == 10
            && raw
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if valid {
            Ok(Self(raw.to_string()))
        } else {
            Err(InvalidTeamId(raw.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What inspecting a code signature found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeSignature {
    /// Nothing at the path could be inspected, or the signing API failed
    /// before reaching a verdict.
    Unreadable { reason: String },
    /// The code carries no signature.
    Unsigned,
    /// A signature is present and does not verify: tampered, truncated,
    /// revoked or otherwise malformed. `status` is the Security framework's
    /// `OSStatus`.
    Invalid { status: i32 },
    /// The signature verifies but is ad-hoc: it names no signer, so its
    /// entitlements are the payload's own claim.
    AdHoc { app_sandbox: bool },
    /// The signature verifies and names a signer.
    Identified {
        /// The signer satisfies the configured [`TrustRequirement`],
        /// notarization included.
        trusted: bool,
        /// `com.apple.security.app-sandbox` is present and exactly `true`.
        app_sandbox: bool,
    },
}

/// How an app is launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacLaunchMode {
    /// Exec'd directly; the kernel applies the App Sandbox from the
    /// signature's entitlements.
    AppSandbox,
    /// Exec'd under the generated Seatbelt profile.
    Seatbelt(SeatbeltReason),
}

/// Why an app did not qualify for [`MacLaunchMode::AppSandbox`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatbeltReason {
    Unreadable,
    Unsigned,
    InvalidSignature,
    AdHoc,
    UntrustedSigner,
    NoAppSandboxEntitlement,
}

impl SeatbeltReason {
    pub fn describe(self) -> &'static str {
        match self {
            SeatbeltReason::Unreadable => "its code signature could not be inspected",
            SeatbeltReason::Unsigned => "it is not code-signed",
            SeatbeltReason::InvalidSignature => "its code signature does not verify",
            SeatbeltReason::AdHoc => "it is only ad-hoc signed",
            SeatbeltReason::UntrustedSigner => {
                "its signer is not a trusted, notarized Developer ID"
            }
            SeatbeltReason::NoAppSandboxEntitlement => {
                "its signature does not enable the App Sandbox entitlement"
            }
        }
    }
}

/// The launch mode for an inspected signature. Only a trusted, identified
/// signature that enables App Sandbox escapes Seatbelt.
pub fn decide(signature: &CodeSignature) -> MacLaunchMode {
    let reason = match signature {
        CodeSignature::Identified {
            trusted: true,
            app_sandbox: true,
        } => return MacLaunchMode::AppSandbox,
        CodeSignature::Identified { trusted: false, .. } => SeatbeltReason::UntrustedSigner,
        CodeSignature::Identified {
            trusted: true,
            app_sandbox: false,
        } => SeatbeltReason::NoAppSandboxEntitlement,
        CodeSignature::AdHoc { .. } => SeatbeltReason::AdHoc,
        CodeSignature::Invalid { .. } => SeatbeltReason::InvalidSignature,
        CodeSignature::Unsigned => SeatbeltReason::Unsigned,
        CodeSignature::Unreadable { .. } => SeatbeltReason::Unreadable,
    };
    MacLaunchMode::Seatbelt(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_trusted_signature_enabling_app_sandbox_escapes_seatbelt() {
        let cases = [
            (
                CodeSignature::Unreadable {
                    reason: "missing".into(),
                },
                MacLaunchMode::Seatbelt(SeatbeltReason::Unreadable),
            ),
            (
                CodeSignature::Unsigned,
                MacLaunchMode::Seatbelt(SeatbeltReason::Unsigned),
            ),
            (
                CodeSignature::Invalid { status: -67061 },
                MacLaunchMode::Seatbelt(SeatbeltReason::InvalidSignature),
            ),
            (
                CodeSignature::AdHoc { app_sandbox: false },
                MacLaunchMode::Seatbelt(SeatbeltReason::AdHoc),
            ),
            (
                CodeSignature::AdHoc { app_sandbox: true },
                MacLaunchMode::Seatbelt(SeatbeltReason::AdHoc),
            ),
            (
                CodeSignature::Identified {
                    trusted: false,
                    app_sandbox: true,
                },
                MacLaunchMode::Seatbelt(SeatbeltReason::UntrustedSigner),
            ),
            (
                CodeSignature::Identified {
                    trusted: true,
                    app_sandbox: false,
                },
                MacLaunchMode::Seatbelt(SeatbeltReason::NoAppSandboxEntitlement),
            ),
            (
                CodeSignature::Identified {
                    trusted: true,
                    app_sandbox: true,
                },
                MacLaunchMode::AppSandbox,
            ),
        ];
        for (signature, expected) in cases {
            assert_eq!(decide(&signature), expected, "{signature:?}");
        }
    }

    #[test]
    fn requirements_pin_developer_id_and_notarization() {
        let any = TrustRequirement::DeveloperId;
        assert!(any.static_requirement().ends_with(" and notarized"));
        assert!(any.static_requirement().starts_with(DEVELOPER_ID_ANCHOR));
        assert!(!any.identity_requirement().contains("notarized"));

        let team = TrustRequirement::DeveloperIdTeam(TeamId::new("ABCDE12345").unwrap());
        assert!(
            team.identity_requirement()
                .ends_with("certificate leaf[subject.OU] = \"ABCDE12345\"")
        );
        assert!(team.static_requirement().contains("ABCDE12345"));
    }

    #[test]
    fn a_team_id_cannot_inject_requirement_text() {
        for bad in [
            "",
            "ABCDE1234",
            "ABCDE123456",
            "abcde12345",
            "ABCDE\"1234",
            "ABC\" or tr",
            "ABCDE 1234",
        ] {
            assert!(TeamId::new(bad).is_err(), "{bad:?} was accepted");
        }
        assert_eq!(TeamId::new("Z9Y8X7W6V5").unwrap().as_str(), "Z9Y8X7W6V5");
    }
}
