//! What a prepared setup carries: one unit per data kind and route, kept in
//! one map. A unit's key says which kind of data it is and how it reaches the
//! destination; its value is the authenticated declaration the destination
//! admits it against. A unit that moves through stages (requested on the
//! original disk, then sealed onto the medium) keeps its stage inside its
//! value, so two stages of one unit can never be declared at once.
use super::*;
use std::collections::BTreeMap;

/// The kind of data a unit moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Home,
    Applications,
    Browser,
    Mail,
    ServiceSessions,
}

/// How a unit reaches the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
    /// Sealed into the setup package on the installer medium, including what
    /// is captured onto it from the original disk before installation.
    Medium,
    /// Left on the retained source disk and opened after installation.
    Retained,
    /// Copied from the original disk into the installed Tilo.
    InstalledCopy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnitKey {
    pub kind: Kind,
    pub route: Route,
}

impl UnitKey {
    pub const MEDIUM_HOME: Self = Self::new(Kind::Home, Route::Medium);
    pub const MEDIUM_APPLICATIONS: Self = Self::new(Kind::Applications, Route::Medium);
    pub const RETAINED_HOME: Self = Self::new(Kind::Home, Route::Retained);
    pub const INSTALLED_COPY_HOME: Self = Self::new(Kind::Home, Route::InstalledCopy);
    pub const INSTALLED_COPY_APPLICATIONS: Self =
        Self::new(Kind::Applications, Route::InstalledCopy);
    pub const BROWSER: Self = Self::new(Kind::Browser, Route::Medium);
    pub const MAIL: Self = Self::new(Kind::Mail, Route::Medium);
    pub const SERVICE_SESSIONS: Self = Self::new(Kind::ServiceSessions, Route::Medium);

    pub const fn new(kind: Kind, route: Route) -> Self {
        Self { kind, route }
    }
}

/// Where the Home on the medium stands.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum HomeCarriage {
    /// Selected; no ciphertext is bound yet.
    Planned,
    /// Requested from the original disk; the live medium captures it before
    /// installation.
    Offline(OfflineHomePlan),
    /// The exact verified ciphertext inside the completed setup package.
    Sealed(super::super::portable_home_media::Expectation),
}

/// Where the applications on the medium stand.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum ApplicationsCarriage {
    /// Requested from the original disk without a Home; the live medium
    /// captures them before installation.
    #[cfg(target_os = "linux")]
    Offline(OfflineApplicationsPlan),
    /// The exact verified signed objects inside the setup package. `alone`
    /// marks applications captured from the original disk without a Home.
    Sealed {
        expectation: super::super::flatpak_transfer::portable::Expectation,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        alone: bool,
    },
}

/// One unit's authenticated declaration. Its variant fixes its key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Unit {
    MediumHome {
        plan: PortableHomePlan,
        carriage: HomeCarriage,
    },
    MediumApplications(ApplicationsCarriage),
    RetainedHome {
        plan: RetainedHomePlan,
        /// Authenticated requested source credentials; only the protected
        /// source record and root-held RW lease can turn these bytes into
        /// scoped authority.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<super::super::home_capture::RetainedCallerDeclaration>,
    },
    #[cfg(target_os = "linux")]
    InstalledCopyHome(OriginalCopyDeclaration),
    #[cfg(target_os = "linux")]
    InstalledCopyApplications(SharedHomeApplicationsDeclaration),
    Browser(BrowserHandoff),
    /// Selected Thunderbird accounts, folders, address books and calendars.
    /// Each artifact is authenticating ciphertext inside this package; the
    /// declaration names only opaque source IDs and never a path or address.
    Mail(Vec<MailHandoff>),
    ServiceSessions(super::super::service_sessions::Declaration),
}

impl Unit {
    pub fn key(&self) -> UnitKey {
        match self {
            Self::MediumHome { .. } => UnitKey::MEDIUM_HOME,
            Self::MediumApplications(_) => UnitKey::MEDIUM_APPLICATIONS,
            Self::RetainedHome { .. } => UnitKey::RETAINED_HOME,
            #[cfg(target_os = "linux")]
            Self::InstalledCopyHome(_) => UnitKey::INSTALLED_COPY_HOME,
            #[cfg(target_os = "linux")]
            Self::InstalledCopyApplications(_) => UnitKey::INSTALLED_COPY_APPLICATIONS,
            Self::Browser(_) => UnitKey::BROWSER,
            Self::Mail(_) => UnitKey::MAIL,
            Self::ServiceSessions(_) => UnitKey::SERVICE_SESSIONS,
        }
    }
}

/// The units of one prepared setup, at most one per kind and route. On the
/// wire it is the list of units; a list naming one key twice is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Units(BTreeMap<UnitKey, Unit>);

impl Serialize for Units {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.values())
    }
}

impl<'de> Deserialize<'de> for Units {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut units = BTreeMap::new();
        for unit in Vec::<Unit>::deserialize(deserializer)? {
            if units.insert(unit.key(), unit).is_some() {
                return Err(serde::de::Error::custom("a unit is declared twice"));
            }
        }
        Ok(Self(units))
    }
}

impl Units {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, key: UnitKey) -> Option<&Unit> {
        self.0.get(&key)
    }
    pub fn contains(&self, key: UnitKey) -> bool {
        self.0.contains_key(&key)
    }
    /// Declare `unit`, replacing the unit of its kind and route.
    pub fn insert(&mut self, unit: Unit) {
        self.0.insert(unit.key(), unit);
    }
    pub fn remove(&mut self, key: UnitKey) -> Option<Unit> {
        self.0.remove(&key)
    }
    pub fn iter(&self) -> impl Iterator<Item = &Unit> {
        self.0.values()
    }

    pub fn get_mut(&mut self, key: UnitKey) -> Option<&mut Unit> {
        self.0.get_mut(&key)
    }
    pub fn portable_home_mut(&mut self) -> Option<&mut PortableHomePlan> {
        match self.get_mut(UnitKey::MEDIUM_HOME)? {
            Unit::MediumHome { plan, .. } => Some(plan),
            _ => None,
        }
    }
    pub fn offline_home_mut(&mut self) -> Option<&mut OfflineHomePlan> {
        match self.get_mut(UnitKey::MEDIUM_HOME)? {
            Unit::MediumHome {
                carriage: HomeCarriage::Offline(plan),
                ..
            } => Some(plan),
            _ => None,
        }
    }
    pub fn service_sessions_mut(
        &mut self,
    ) -> Option<&mut super::super::service_sessions::Declaration> {
        match self.get_mut(UnitKey::SERVICE_SESSIONS)? {
            Unit::ServiceSessions(sessions) => Some(sessions),
            _ => None,
        }
    }
    /// Declare the Home selection on the medium, keeping where its
    /// ciphertext stands when one is already declared.
    pub fn plan_home(&mut self, plan: PortableHomePlan) {
        let carriage = match self.remove(UnitKey::MEDIUM_HOME) {
            Some(Unit::MediumHome { carriage, .. }) => carriage,
            _ => HomeCarriage::Planned,
        };
        self.insert(Unit::MediumHome { plan, carriage });
    }
    pub fn portable_home(&self) -> Option<&PortableHomePlan> {
        match self.get(UnitKey::MEDIUM_HOME)? {
            Unit::MediumHome { plan, .. } => Some(plan),
            _ => None,
        }
    }
    fn home_carriage(&self) -> Option<&HomeCarriage> {
        match self.get(UnitKey::MEDIUM_HOME)? {
            Unit::MediumHome { carriage, .. } => Some(carriage),
            _ => None,
        }
    }
    pub fn offline_home(&self) -> Option<&OfflineHomePlan> {
        match self.home_carriage()? {
            HomeCarriage::Offline(plan) => Some(plan),
            _ => None,
        }
    }
    pub fn completed_home(&self) -> Option<&super::super::portable_home_media::Expectation> {
        match self.home_carriage()? {
            HomeCarriage::Sealed(expectation) => Some(expectation),
            _ => None,
        }
    }
    /// Move the Home on the medium to `carriage`; refused without one.
    pub fn carry_home(&mut self, to: HomeCarriage) -> io::Result<()> {
        match self.0.get_mut(&UnitKey::MEDIUM_HOME) {
            Some(Unit::MediumHome { carriage, .. }) => {
                *carriage = to;
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
    /// Replace what was requested from the original disk with what the live
    /// medium captured from it: the sealed Home, if any, and the sealed
    /// applications, `alone` when they came without a Home.
    pub fn seal_offline(
        &mut self,
        home: Option<super::super::portable_home_media::Expectation>,
        applications: Option<super::super::flatpak_transfer::portable::Expectation>,
        alone: bool,
    ) -> io::Result<()> {
        match self.0.get_mut(&UnitKey::MEDIUM_HOME) {
            Some(Unit::MediumHome { carriage, .. }) => {
                *carriage = home.map_or(HomeCarriage::Planned, HomeCarriage::Sealed);
            }
            _ if home.is_some() => return Err(invalid()),
            _ => {}
        }
        match applications {
            Some(expectation) => {
                self.insert(Unit::MediumApplications(ApplicationsCarriage::Sealed {
                    expectation,
                    alone,
                }))
            }
            None => {
                self.remove(UnitKey::MEDIUM_APPLICATIONS);
            }
        }
        Ok(())
    }
    fn applications_carriage(&self) -> Option<&ApplicationsCarriage> {
        match self.get(UnitKey::MEDIUM_APPLICATIONS)? {
            Unit::MediumApplications(carriage) => Some(carriage),
            _ => None,
        }
    }
    #[cfg(target_os = "linux")]
    pub fn offline_applications(&self) -> Option<&OfflineApplicationsPlan> {
        match self.applications_carriage()? {
            ApplicationsCarriage::Offline(plan) => Some(plan),
            ApplicationsCarriage::Sealed { .. } => None,
        }
    }
    pub fn portable_flatpak(
        &self,
    ) -> Option<&super::super::flatpak_transfer::portable::Expectation> {
        match self.applications_carriage()? {
            ApplicationsCarriage::Sealed { expectation, .. } => Some(expectation),
            #[cfg(target_os = "linux")]
            ApplicationsCarriage::Offline(_) => None,
        }
    }
    pub fn completed_applications_only(&self) -> bool {
        matches!(
            self.applications_carriage(),
            Some(ApplicationsCarriage::Sealed { alone: true, .. })
        )
    }
    pub fn retained_home(&self) -> Option<&RetainedHomePlan> {
        match self.get(UnitKey::RETAINED_HOME)? {
            Unit::RetainedHome { plan, .. } => Some(plan),
            _ => None,
        }
    }
    pub fn retained_caller(
        &self,
    ) -> Option<&super::super::home_capture::RetainedCallerDeclaration> {
        match self.get(UnitKey::RETAINED_HOME)? {
            Unit::RetainedHome { caller, .. } => caller.as_ref(),
            _ => None,
        }
    }
    /// Bind the requested source credentials to the retained Home; refused
    /// without one.
    pub fn set_retained_caller(
        &mut self,
        declaration: Option<super::super::home_capture::RetainedCallerDeclaration>,
    ) -> io::Result<()> {
        match self.0.get_mut(&UnitKey::RETAINED_HOME) {
            Some(Unit::RetainedHome { caller, .. }) => {
                *caller = declaration;
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
    #[cfg(target_os = "linux")]
    pub fn original_copy(&self) -> Option<&OriginalCopyDeclaration> {
        match self.get(UnitKey::INSTALLED_COPY_HOME)? {
            Unit::InstalledCopyHome(declaration) => Some(declaration),
            _ => None,
        }
    }
    #[cfg(target_os = "linux")]
    pub fn computer_applications(&self) -> Option<&SharedHomeApplicationsDeclaration> {
        match self.get(UnitKey::INSTALLED_COPY_APPLICATIONS)? {
            Unit::InstalledCopyApplications(declaration) => Some(declaration),
            _ => None,
        }
    }
    pub fn browser(&self) -> Option<&BrowserHandoff> {
        match self.get(UnitKey::BROWSER)? {
            Unit::Browser(browser) => Some(browser),
            _ => None,
        }
    }
    pub fn mail(&self) -> &[MailHandoff] {
        match self.get(UnitKey::MAIL) {
            Some(Unit::Mail(mail)) => mail,
            _ => &[],
        }
    }
    pub fn service_sessions(&self) -> Option<&super::super::service_sessions::Declaration> {
        match self.get(UnitKey::SERVICE_SESSIONS)? {
            Unit::ServiceSessions(sessions) => Some(sessions),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_and_route_is_declared_at_most_once() {
        let mut units = Units::default();
        units.insert(Unit::Mail(Vec::new()));
        units.insert(Unit::Mail(Vec::new()));
        assert_eq!(units.iter().count(), 1);
        let wire = serde_json::to_string(&units).unwrap();
        assert_eq!(wire, r#"[{"mail":[]}]"#);
        assert!(serde_json::from_str::<Units>(r#"[{"mail":[]},{"mail":[]}]"#).is_err());
        assert_eq!(serde_json::from_str::<Units>(&wire).unwrap(), units);
        assert!(units.carry_home(HomeCarriage::Planned).is_err());
        assert!(units.set_retained_caller(None).is_err());
    }
}
