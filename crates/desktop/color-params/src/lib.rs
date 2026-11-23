//! The parameters of a parametric `wp_color_management_v1` image description, as the protocol
//! states them, and the rules a server checks them by.
//!
//! The compositor builds its descriptions from them. The app keeper holds an app's descriptions as
//! them, so it can make each again on every desktop it shows the app on, and checks the app's
//! requests by these same rules before any desktop sees them: what the keeper accepts from an app
//! is exactly what a desktop would. Values are the protocol's own enum numbers; [`server`] reads
//! them from, and advertises them on, the protocol's server objects.

#[cfg(target_os = "linux")]
pub mod server;

/// `wp_color_manager_v1.feature` values.
pub mod feature {
    pub const ICC_V2_V4: u32 = 0;
    pub const PARAMETRIC: u32 = 1;
    pub const SET_PRIMARIES: u32 = 2;
    pub const SET_TF_POWER: u32 = 3;
    pub const SET_LUMINANCES: u32 = 4;
    pub const SET_MASTERING_DISPLAY_PRIMARIES: u32 = 5;
    pub const EXTENDED_TARGET_VOLUME: u32 = 6;
    pub const WINDOWS_SCRGB: u32 = 7;
}

/// What a `wp_color_manager_v1` tells its client it supports: its `supported_*` events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Support {
    pub intents: Vec<u32>,
    pub features: Vec<u32>,
    pub primaries: Vec<u32>,
    pub transfers: Vec<u32>,
}

impl Support {
    #[must_use]
    pub fn has(&self, feature: u32) -> bool {
        self.features.contains(&feature)
    }

    #[must_use]
    pub fn intent(&self, intent: u32) -> bool {
        self.intents.contains(&intent)
    }
}

/// The primary colour volume: named, or as CIE 1931 xy chromaticities × 1 000 000 (red, green,
/// blue, white; x then y).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Primaries {
    Named(u32),
    Chromaticities([i32; 8]),
}

/// The transfer characteristic: named, or a power curve whose exponent is × 10 000.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transfer {
    Named(u32),
    Power(u32),
}

/// One `wp_image_description_creator_params_v1` request that sets a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    TfNamed(u32),
    TfPower(u32),
    PrimariesNamed(u32),
    Primaries([i32; 8]),
    /// Minimum (cd/m² × 10 000), maximum and reference white (cd/m²).
    Luminances(u32, u32, u32),
    MasteringPrimaries([i32; 8]),
    /// Minimum (cd/m² × 10 000) and maximum (cd/m²).
    MasteringLuminance(u32, u32),
    MaxCll(u32),
    MaxFall(u32),
}

/// What a parametric creator was told, each parameter set at most once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Params {
    pub primaries: Option<Primaries>,
    pub transfer: Option<Transfer>,
    pub luminances: Option<(u32, u32, u32)>,
    pub mastering_primaries: Option<[i32; 8]>,
    pub mastering_luminance: Option<(u32, u32)>,
    pub max_cll: Option<u32>,
    pub max_fall: Option<u32>,
}

/// `wp_image_description_creator_params_v1.error`: the protocol error a request raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamError {
    IncompleteSet,
    AlreadySet,
    UnsupportedFeature,
    InvalidTf,
    InvalidPrimariesNamed,
    InvalidLuminance,
}

impl ParamError {
    /// The protocol's number for it.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::IncompleteSet => 0,
            Self::AlreadySet => 1,
            Self::UnsupportedFeature => 2,
            Self::InvalidTf => 3,
            Self::InvalidPrimariesNamed => 4,
            Self::InvalidLuminance => 5,
        }
    }

    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::IncompleteSet => "primaries and a transfer characteristic are required",
            Self::AlreadySet => "this parameter is already set",
            Self::UnsupportedFeature => "the compositor did not advertise this feature",
            Self::InvalidTf => "the transfer characteristic is not one the compositor supports",
            Self::InvalidPrimariesNamed => "the primaries are not ones the compositor supports",
            Self::InvalidLuminance => "the luminances are out of order",
        }
    }
}

/// `value` into `slot`, unless something is there already.
fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), ParamError> {
    if slot.is_some() {
        return Err(ParamError::AlreadySet);
    }
    *slot = Some(value);
    Ok(())
}

fn need(support: &Support, feature: u32) -> Result<(), ParamError> {
    if support.has(feature) {
        Ok(())
    } else {
        Err(ParamError::UnsupportedFeature)
    }
}

/// Whether the luminance `cd` (cd/m²) is above `min_scaled` (cd/m² × 10 000).
fn above(cd: u32, min_scaled: u32) -> bool {
    u64::from(cd).saturating_mul(10_000) > u64::from(min_scaled)
}

impl Params {
    /// Take one set request, checked against what the server told its client it supports.
    pub fn set(&mut self, set: Set, support: &Support) -> Result<(), ParamError> {
        match set {
            Set::TfNamed(tf) => {
                if !support.transfers.contains(&tf) {
                    return Err(ParamError::InvalidTf);
                }
                once(&mut self.transfer, Transfer::Named(tf))
            }
            Set::TfPower(eexp) => {
                need(support, feature::SET_TF_POWER)?;
                if !(10_000..=100_000).contains(&eexp) {
                    return Err(ParamError::InvalidTf);
                }
                once(&mut self.transfer, Transfer::Power(eexp))
            }
            Set::PrimariesNamed(primaries) => {
                if !support.primaries.contains(&primaries) {
                    return Err(ParamError::InvalidPrimariesNamed);
                }
                once(&mut self.primaries, Primaries::Named(primaries))
            }
            Set::Primaries(xy) => {
                need(support, feature::SET_PRIMARIES)?;
                once(&mut self.primaries, Primaries::Chromaticities(xy))
            }
            Set::Luminances(min, max, reference) => {
                need(support, feature::SET_LUMINANCES)?;
                if !above(max, min) || !above(reference, min) {
                    return Err(ParamError::InvalidLuminance);
                }
                once(&mut self.luminances, (min, max, reference))
            }
            Set::MasteringPrimaries(xy) => {
                need(support, feature::SET_MASTERING_DISPLAY_PRIMARIES)?;
                once(&mut self.mastering_primaries, xy)
            }
            Set::MasteringLuminance(min, max) => {
                need(support, feature::SET_MASTERING_DISPLAY_PRIMARIES)?;
                if !above(max, min) {
                    return Err(ParamError::InvalidLuminance);
                }
                once(&mut self.mastering_luminance, (min, max))
            }
            Set::MaxCll(cll) => once(&mut self.max_cll, cll),
            Set::MaxFall(fall) => once(&mut self.max_fall, fall),
        }
    }

    /// What `create` checks (version 1): primaries and a transfer characteristic are set, and
    /// the content light levels fit each other and the mastering range.
    pub fn check_complete(&self) -> Result<(), ParamError> {
        if self.primaries.is_none() || self.transfer.is_none() {
            return Err(ParamError::IncompleteSet);
        }
        if let (Some(cll), Some(fall)) = (self.max_cll, self.max_fall)
            && fall > cll
        {
            return Err(ParamError::InvalidLuminance);
        }
        if let Some((min, max)) = self.mastering_luminance {
            for level in [self.max_cll, self.max_fall].into_iter().flatten() {
                if !above(level, min) || level > max {
                    return Err(ParamError::InvalidLuminance);
                }
            }
        }
        Ok(())
    }

    /// The requests that set these parameters on another creator.
    #[must_use]
    pub fn requests(&self) -> Vec<Set> {
        let mut requests = Vec::new();
        requests.extend(self.primaries.map(|primaries| match primaries {
            Primaries::Named(named) => Set::PrimariesNamed(named),
            Primaries::Chromaticities(xy) => Set::Primaries(xy),
        }));
        requests.extend(self.transfer.map(|transfer| match transfer {
            Transfer::Named(named) => Set::TfNamed(named),
            Transfer::Power(eexp) => Set::TfPower(eexp),
        }));
        requests.extend(
            self.luminances
                .map(|(min, max, reference)| Set::Luminances(min, max, reference)),
        );
        requests.extend(self.mastering_primaries.map(Set::MasteringPrimaries));
        requests.extend(
            self.mastering_luminance
                .map(|(min, max)| Set::MasteringLuminance(min, max)),
        );
        requests.extend(self.max_cll.map(Set::MaxCll));
        requests.extend(self.max_fall.map(Set::MaxFall));
        requests
    }

    /// Whether a server that supports `support` takes these parameters without a protocol error.
    #[must_use]
    pub fn served_by(&self, support: &Support) -> bool {
        let mut again = Self::default();
        support.has(feature::PARAMETRIC)
            && self
                .requests()
                .into_iter()
                .all(|set| again.set(set, support).is_ok())
            && again.check_complete().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support() -> Support {
        Support {
            intents: vec![0],
            features: vec![
                feature::PARAMETRIC,
                feature::SET_PRIMARIES,
                feature::SET_MASTERING_DISPLAY_PRIMARIES,
            ],
            primaries: vec![1, 6],
            transfers: vec![2, 11],
        }
    }

    #[test]
    fn requests_are_checked_against_what_was_advertised() {
        let support = support();
        let mut params = Params::default();
        assert_eq!(params.check_complete(), Err(ParamError::IncompleteSet));
        assert_eq!(
            params.set(Set::TfNamed(13), &support),
            Err(ParamError::InvalidTf)
        );
        assert_eq!(
            params.set(Set::TfPower(22_000), &support),
            Err(ParamError::UnsupportedFeature)
        );
        assert_eq!(
            params.set(Set::PrimariesNamed(9), &support),
            Err(ParamError::InvalidPrimariesNamed)
        );
        assert_eq!(params.set(Set::TfNamed(11), &support), Ok(()));
        assert_eq!(params.set(Set::PrimariesNamed(6), &support), Ok(()));
        assert_eq!(
            params.set(Set::PrimariesNamed(1), &support),
            Err(ParamError::AlreadySet)
        );
        assert_eq!(
            params.set(Set::MasteringLuminance(50, 0), &support),
            Err(ParamError::InvalidLuminance)
        );
        assert_eq!(
            params.set(Set::MasteringLuminance(50, 1_000), &support),
            Ok(())
        );
        assert_eq!(params.set(Set::MaxCll(2_000), &support), Ok(()));
        assert_eq!(params.check_complete(), Err(ParamError::InvalidLuminance));
    }

    #[test]
    fn parameters_replay_only_where_every_one_is_supported() {
        let support = support();
        let mut params = Params::default();
        for set in [
            Set::PrimariesNamed(6),
            Set::TfNamed(11),
            Set::MasteringLuminance(50, 1_000),
            Set::MaxCll(800),
        ] {
            params.set(set, &support).expect("advertised");
        }
        assert!(params.served_by(&support));
        let without_pq = Support {
            transfers: vec![2],
            ..support.clone()
        };
        assert!(!params.served_by(&without_pq));
        let without_mastering = Support {
            features: vec![feature::PARAMETRIC],
            ..support
        };
        assert!(!params.served_by(&without_mastering));
    }
}
