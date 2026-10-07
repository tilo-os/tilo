//! System interfaces: the versioned contracts between an app and the OS
//! services it calls.
//!
//! An app names each service it uses in its manifest's `[sandbox.services]`
//! table as `required@N` or `optional@N`. A required entry is the app's
//! interface requirement: it cannot work without version `N` of that
//! service's contract. The image declares, per interface, every version it
//! serves ([`ProvidedInterfaces`]), so a device can tell before installing,
//! and again before running, whether an app build fits the OS under it.
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

/// The interface versions an OS image serves, as its
/// `system-interfaces.toml` declares them:
///
/// ```toml
/// [interfaces]
/// system = [1]
/// collab = [1]
/// ```
///
/// An interface the declaration does not name is not provided at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidedInterfaces {
    #[serde(default)]
    pub interfaces: BTreeMap<String, BTreeSet<u32>>,
}

/// A requirement the OS does not meet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmet {
    /// The requirement as the app wrote it.
    pub requirement: String,
    /// The versions this OS serves of that interface; empty when it serves
    /// none, or the requirement could not be read.
    pub provided: Vec<u32>,
}

impl fmt::Display for Unmet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self
            .requirement
            .split_once('@')
            .map_or(self.requirement.as_str(), |(name, _)| name);
        match self.provided.as_slice() {
            [] => write!(
                f,
                "{} (this system has no {name} interface)",
                self.requirement
            ),
            versions => {
                let served: Vec<String> = versions.iter().map(|v| format!("{name}@{v}")).collect();
                write!(
                    f,
                    "{} (this system provides {})",
                    self.requirement,
                    served.join(", ")
                )
            }
        }
    }
}

impl ProvidedInterfaces {
    /// A declaration serving exactly `interfaces`.
    pub fn new<'a>(interfaces: impl IntoIterator<Item = (&'a str, &'a [u32])>) -> Self {
        Self {
            interfaces: interfaces
                .into_iter()
                .map(|(name, versions)| (name.to_owned(), versions.iter().copied().collect()))
                .collect(),
        }
    }

    /// Whether this OS serves `requirement`.
    pub fn provides(&self, requirement: &InterfaceRequirement) -> bool {
        self.interfaces
            .get(&requirement.name)
            .is_some_and(|versions| versions.contains(&requirement.version))
    }

    /// The versions served of `name`, oldest first.
    pub fn versions(&self, name: &str) -> Vec<u32> {
        self.interfaces
            .get(name)
            .map(|versions| versions.iter().copied().collect())
            .unwrap_or_default()
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
                provided: self.versions(&requirement.name),
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
                    provided: self.versions(&requirement.name),
                }),
                Err(_) => Some(Unmet {
                    requirement: raw.clone(),
                    provided: Vec::new(),
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

    #[test]
    fn an_os_meets_exactly_the_versions_it_declares() {
        let os = ProvidedInterfaces::new([("system", &[1, 2][..]), ("collab", &[1][..])]);
        assert!(os.provides(&req("system@1")));
        assert!(os.provides(&req("system@2")));
        assert!(!os.provides(&req("system@3")));
        assert!(!os.provides(&req("displays@1")));

        let wanted = [req("collab@1"), req("system@3"), req("displays@1")];
        let unmet = os.unmet(&wanted);
        assert_eq!(
            unmet,
            vec![
                Unmet {
                    requirement: "system@3".into(),
                    provided: vec![1, 2]
                },
                Unmet {
                    requirement: "displays@1".into(),
                    provided: vec![]
                },
            ]
        );
        assert_eq!(
            describe_unmet(&unmet),
            "system@3 (this system provides system@1, system@2); \
             displays@1 (this system has no displays interface)"
        );
    }

    #[test]
    fn wire_requirements_that_do_not_parse_are_unmet() {
        let os = ProvidedInterfaces::new([("system", &[1][..])]);
        let ok = vec!["system@1".to_string()];
        assert!(os.meets_wire(&ok));
        assert!(os.meets_wire(&Vec::<String>::new()));
        let garbled = vec!["system@1".to_string(), "system".to_string()];
        assert!(!os.meets_wire(&garbled));
        assert_eq!(os.unmet_wire(&garbled)[0].requirement, "system");
    }

    #[test]
    fn the_declaration_reads_from_json_and_refuses_strays() {
        let os: ProvidedInterfaces =
            serde_json::from_str(r#"{"interfaces":{"system":[2,1]}}"#).unwrap();
        assert_eq!(os.versions("system"), vec![1, 2]);
        assert!(serde_json::from_str::<ProvidedInterfaces>(r#"{"system":[1]}"#).is_err());
    }
}
