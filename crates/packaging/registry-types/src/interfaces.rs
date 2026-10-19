//! System interfaces: the versioned contracts between an app and the OS
//! services it calls.
//!
//! An app names each service it uses in its manifest's `[sandbox.services]`
//! table as `required@N` or `optional@N`, `N` being the version of the
//! interface the service belongs to that the app was built against. The
//! image serves exactly one version of each interface
//! ([`ProvidedInterfaces`]); an app speaks to it only at that version, with
//! no negotiation and nothing kept for an older one
//! (docs/architecture/compatibility.md). A device can therefore tell before
//! installing, and again before running, whether an app build fits the OS
//! under it, and which of the two is behind.
//!
//! Most services belong to the runtime interface ([`RUNTIME_INTERFACE`]):
//! the image lists them, and their version is the runtime's. `shell-ui` and
//! `system` are interfaces of their own.
//!
//! A registry version carries its build's requirements in
//! [`RegistryTarget::interfaces`](crate::RegistryTarget::interfaces) in the
//! wire form `name@N`, which [`InterfaceRequirement`] parses.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The interface of the system services a device's control panel drives:
/// network, displays, power, accounts, updates and the rest, reached through
/// the image-owned system authority grant.
pub const SYSTEM_INTERFACE: &str = "system";

/// Longest interface name accepted.
pub const MAX_INTERFACE_NAME: usize = 64;

/// One required interface version: `system@1`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterfaceRequirement {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InterfaceError {
    #[error("interface requirement {0:?} is not `<name>@<version>`")]
    Shape(String),
    #[error("interface name {0:?} is not 1-64 lowercase letters, digits and dashes")]
    Name(String),
    #[error("interface version in {0:?} is not a positive integer")]
    Version(String),
}

/// Whether `name` is a well-formed interface name.
pub fn valid_interface_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_INTERFACE_NAME
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl InterfaceRequirement {
    /// A requirement on `version` of `name`.
    ///
    /// # Errors
    ///
    /// Returns an error if `name` is malformed or `version` is zero.
    pub fn new(name: &str, version: u32) -> Result<Self, InterfaceError> {
        if !valid_interface_name(name) {
            return Err(InterfaceError::Name(name.to_owned()));
        }
        if version == 0 {
            return Err(InterfaceError::Version(format!("{name}@0")));
        }
        Ok(Self {
            name: name.to_owned(),
            version,
        })
    }
}

impl fmt::Display for InterfaceRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

impl FromStr for InterfaceRequirement {
    type Err = InterfaceError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (name, version) = raw
            .split_once('@')
            .ok_or_else(|| InterfaceError::Shape(raw.to_owned()))?;
        let version = version
            .parse::<u32>()
            .ok()
            .filter(|_| version.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| InterfaceError::Version(raw.to_owned()))?;
        Self::new(name, version)
    }
}

/// The interface every session service the image lists under `[runtime]`
/// belongs to: collaboration, the filesystem, the app host, the identity
/// broker and the rest an app is connected to.
pub const RUNTIME_INTERFACE: &str = "runtime";

/// The one interface version an OS image serves of each, as its
/// `system-interfaces.toml` declares them, and the services that are part of
/// the runtime interface:
///
/// ```toml
/// [interfaces]
/// system = 1
/// runtime = 1
///
/// [runtime]
/// services = ["collab", "tilo-fs"]
/// ```
///
/// An interface or service the declaration does not name is not provided.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidedInterfaces {
    #[serde(default)]
    pub interfaces: BTreeMap<String, u32>,
    #[serde(default)]
    pub runtime: RuntimeServices,
}

/// The services that speak the runtime interface's version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServices {
    #[serde(default)]
    pub services: BTreeSet<String>,
}

/// Which side of a mismatch is behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
    /// The package was built against an older version than the OS serves:
    /// a newer build of the package fits.
    Package,
    /// The package needs a newer version than this OS serves: the OS
    /// updates first.
    System,
    /// The OS has no such interface, or the requirement could not be read.
    Unknown,
}

/// A requirement the OS does not meet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmet {
    /// The requirement as the app wrote it.
    pub requirement: String,
    /// The version this OS serves of the requirement's interface; `None`
    /// when it serves none, or the requirement could not be read.
    pub provided: Option<u32>,
}

impl Unmet {
    /// The interface the requirement names, as written.
    pub fn interface(&self) -> &str {
        self.requirement
            .split_once('@')
            .map_or(self.requirement.as_str(), |(name, _)| name)
    }

    /// Which side is behind.
    pub fn behind(&self) -> Behind {
        let wanted = self
            .requirement
            .parse::<InterfaceRequirement>()
            .ok()
            .map(|requirement| requirement.version);
        match (wanted, self.provided) {
            (Some(wanted), Some(provided)) if wanted < provided => Behind::Package,
            (Some(wanted), Some(provided)) if wanted > provided => Behind::System,
            _ => Behind::Unknown,
        }
    }
}

impl fmt::Display for Unmet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.interface();
        match self.provided {
            None => write!(
                f,
                "{} (this system has no {name} interface)",
                self.requirement
            ),
            Some(version) => write!(
                f,
                "{} (this system provides {name}@{version})",
                self.requirement
            ),
        }
    }
}

/// Whether some of `unmet` mean the package is behind and none that the OS
/// is: the package's next build fits.
pub fn package_is_behind(unmet: &[Unmet]) -> bool {
    !unmet.is_empty() && unmet.iter().all(|unmet| unmet.behind() == Behind::Package)
}

impl ProvidedInterfaces {
    /// A declaration serving exactly `interfaces`, with `runtime_services`
    /// speaking the runtime interface's version.
    pub fn new<'a>(
        interfaces: impl IntoIterator<Item = (&'a str, u32)>,
        runtime_services: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        Self {
            interfaces: interfaces
                .into_iter()
                .map(|(name, version)| (name.to_owned(), version))
                .collect(),
            runtime: RuntimeServices {
                services: runtime_services.into_iter().map(str::to_owned).collect(),
            },
        }
    }

    /// The version this OS serves of `name`: an interface's own, or the
    /// runtime's for a service the runtime interface includes.
    pub fn version(&self, name: &str) -> Option<u32> {
        self.interfaces.get(name).copied().or_else(|| {
            self.runtime
                .services
                .contains(name)
                .then(|| self.interfaces.get(RUNTIME_INTERFACE).copied())
                .flatten()
        })
    }

    /// Whether this OS serves `requirement`.
    pub fn provides(&self, requirement: &InterfaceRequirement) -> bool {
        self.version(&requirement.name) == Some(requirement.version)
    }

    /// The requirements this OS does not meet, in the order given.
    pub fn unmet<'a>(
        &self,
        requirements: impl IntoIterator<Item = &'a InterfaceRequirement>,
    ) -> Vec<Unmet> {
        requirements
            .into_iter()
            .filter(|requirement| !self.provides(requirement))
            .map(|requirement| Unmet {
                requirement: requirement.to_string(),
                provided: self.version(&requirement.name),
            })
            .collect()
    }

    /// [`ProvidedInterfaces::unmet`] over the wire form a registry version
    /// carries. A requirement that does not parse is unmet: a device cannot
    /// know it serves a contract it cannot read.
    pub fn unmet_wire<'a>(&self, requirements: impl IntoIterator<Item = &'a String>) -> Vec<Unmet> {
        requirements
            .into_iter()
            .filter_map(|raw| match raw.parse::<InterfaceRequirement>() {
                Ok(requirement) if self.provides(&requirement) => None,
                Ok(requirement) => Some(Unmet {
                    requirement: requirement.to_string(),
                    provided: self.version(&requirement.name),
                }),
                Err(_) => Some(Unmet {
                    requirement: raw.clone(),
                    provided: None,
                }),
            })
            .collect()
    }

    /// Whether every one of `requirements` (wire form) is served.
    pub fn meets_wire<'a>(&self, requirements: impl IntoIterator<Item = &'a String>) -> bool {
        self.unmet_wire(requirements).is_empty()
    }
}

/// `unmet` as one readable clause: `system@2 (this system provides system@1)`.
pub fn describe_unmet(unmet: &[Unmet]) -> String {
    unmet
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(raw: &str) -> InterfaceRequirement {
        raw.parse().unwrap()
    }

    #[test]
    fn requirements_have_one_spelling() {
        assert_eq!(
            req("system@1"),
            InterfaceRequirement {
                name: "system".into(),
                version: 1
            }
        );
        assert_eq!(req("model-gateway@12").to_string(), "model-gateway@12");
        for bad in [
            "system",
            "system@",
            "@1",
            "system@0",
            "System@1",
            "system@+1",
            "system@1.0",
            "-x@1",
            "sys tem@1",
            "system@-1",
        ] {
            assert!(bad.parse::<InterfaceRequirement>().is_err(), "{bad} parsed");
        }
        let long = format!("{}@1", "a".repeat(MAX_INTERFACE_NAME + 1));
        assert!(long.parse::<InterfaceRequirement>().is_err());
    }

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
    #[test]
    fn an_os_meets_exactly_the_one_version_it_serves() {
        let os = ProvidedInterfaces::new([("system", 2), ("runtime", 3)], ["collab"]);
        assert!(os.provides(&req("system@2")));
        assert!(
            !os.provides(&req("system@1")),
            "nothing is kept for an older version"
        );
        assert!(!os.provides(&req("system@3")));
        assert!(
            os.provides(&req("collab@3")),
            "a runtime service speaks the runtime's version"
        );
        assert!(!os.provides(&req("collab@1")));
        assert!(!os.provides(&req("displays@1")));

        let wanted = [
            req("collab@3"),
            req("system@1"),
            req("system@3"),
            req("displays@1"),
        ];
        let unmet = os.unmet(&wanted);
        assert_eq!(
            unmet,
            vec![
                Unmet {
                    requirement: "system@1".into(),
                    provided: Some(2)
                },
                Unmet {
                    requirement: "system@3".into(),
                    provided: Some(2)
                },
                Unmet {
                    requirement: "displays@1".into(),
                    provided: None
                },
            ]
        );
        assert_eq!(
            unmet.iter().map(Unmet::behind).collect::<Vec<_>>(),
            [Behind::Package, Behind::System, Behind::Unknown]
        );
        assert!(package_is_behind(&unmet[..1]));
        assert!(!package_is_behind(&unmet[..2]), "the system is behind too");
        assert!(!package_is_behind(&[]));
        assert_eq!(
            describe_unmet(&unmet),
            "system@1 (this system provides system@2); \
             system@3 (this system provides system@2); \
             displays@1 (this system has no displays interface)"
        );
    }

    #[test]
    fn wire_requirements_that_do_not_parse_are_unmet() {
        let os = ProvidedInterfaces::new([("system", 1)], []);
        let ok = vec!["system@1".to_string()];
        assert!(os.meets_wire(&ok));
        assert!(os.meets_wire(&Vec::<String>::new()));
        let garbled = vec!["system@1".to_string(), "system".to_string()];
        assert!(!os.meets_wire(&garbled));
        assert_eq!(os.unmet_wire(&garbled)[0].requirement, "system");
    }

    #[test]
    fn the_declaration_reads_from_json_and_refuses_strays() {
        let os: ProvidedInterfaces = serde_json::from_str(
            r#"{"interfaces":{"system":2,"runtime":1},"runtime":{"services":["collab"]}}"#,
        )
        .unwrap();
        assert_eq!(os.version("system"), Some(2));
        assert_eq!(os.version("collab"), Some(1));
        assert!(serde_json::from_str::<ProvidedInterfaces>(r#"{"system":1}"#).is_err());
        assert!(
            serde_json::from_str::<ProvidedInterfaces>(r#"{"interfaces":{"system":[1,2]}}"#)
                .is_err(),
            "one version per interface"
        );
    }
}
