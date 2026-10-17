//! Pure, non-privileged setup choices and review validation.
//!
//! Values in this module describe user intent and probed capabilities. They are
//! local application state, not authorization for installation or file access.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    Linux,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Experience {
    Desktop,
    FullOs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderMode {
    Copy,
    Connected,
    Later,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub host: HostOs,
    pub desktop_session: bool,
    pub internal_dual_boot: bool,
}

impl Capabilities {
    #[must_use]
    pub fn supports(&self, experience: Experience) -> bool {
        match (self.host, experience) {
            (HostOs::Linux, Experience::Desktop) => self.desktop_session,
            (HostOs::Windows | HostOs::Linux, Experience::FullOs) => self.internal_dual_boot,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSelection {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub bytes: u64,
    pub mode: FolderMode,
    pub readable: bool,
    pub safe_sync: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupPlan {
    pub experience: Experience,
    pub folders: Vec<FolderSelection>,
    pub system_bytes: u64,
    pub staging_bytes: u64,
    pub headroom_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderPath {
    Source,
    Destination,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewError {
    UnsupportedExperience {
        experience: Experience,
    },
    InaccessibleSource {
        folder_index: usize,
    },
    UnavailableConnection {
        folder_index: usize,
    },
    InvalidPath {
        folder_index: usize,
        path: FolderPath,
    },
    OverlappingPaths {
        first_folder_index: usize,
        first_path: FolderPath,
        second_folder_index: usize,
        second_path: FolderPath,
    },
    InsufficientSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    Overflow,
}

impl SetupPlan {
    /// Calculate estimated storage for the base system and active folders.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Overflow`] when any subtotal exceeds `u64`.
    pub fn required_bytes(&self) -> Result<u64, ReviewError> {
        let base = self
            .system_bytes
            .checked_add(self.staging_bytes)
            .and_then(|bytes| bytes.checked_add(self.headroom_bytes))
            .ok_or(ReviewError::Overflow)?;

        self.folders
            .iter()
            .filter(|folder| folder.mode != FolderMode::Later)
            .try_fold(base, |total, folder| {
                total.checked_add(folder.bytes).ok_or(ReviewError::Overflow)
            })
    }

    /// Checks the non-mutating review model against current capability facts.
    ///
    /// This is deliberately lexical validation. Before actual I/O, the platform
    /// adapter must canonicalize paths, compare volume identities, apply the
    /// filesystem's case-folding rules, and revalidate access and mount safety.
    ///
    /// # Errors
    ///
    /// Returns the first unsupported choice, unsafe or invalid folder, storage
    /// overflow, or insufficient-space condition found in the plan.
    pub fn validate(
        &self,
        capabilities: &Capabilities,
        available_bytes: u64,
    ) -> Result<(), ReviewError> {
        if !capabilities.supports(self.experience) {
            return Err(ReviewError::UnsupportedExperience {
                experience: self.experience,
            });
        }

        let active = self.active_folders();
        for (index, folder) in &active {
            if !folder.readable {
                return Err(ReviewError::InaccessibleSource {
                    folder_index: *index,
                });
            }
            if folder.mode == FolderMode::Connected && !folder.safe_sync {
                return Err(ReviewError::UnavailableConnection {
                    folder_index: *index,
                });
            }
            validate_path(*index, FolderPath::Source, &folder.source)?;
            validate_path(*index, FolderPath::Destination, &folder.destination)?;
            reject_overlap(
                *index,
                FolderPath::Source,
                &folder.source,
                *index,
                FolderPath::Destination,
                &folder.destination,
            )?;
        }

        for (position, (first_index, first)) in active.iter().enumerate() {
            for (second_index, second) in active.iter().skip(position + 1) {
                reject_overlap(
                    *first_index,
                    FolderPath::Source,
                    &first.source,
                    *second_index,
                    FolderPath::Source,
                    &second.source,
                )?;
                reject_overlap(
                    *first_index,
                    FolderPath::Destination,
                    &first.destination,
                    *second_index,
                    FolderPath::Destination,
                    &second.destination,
                )?;
                reject_overlap(
                    *first_index,
                    FolderPath::Source,
                    &first.source,
                    *second_index,
                    FolderPath::Destination,
                    &second.destination,
                )?;
                reject_overlap(
                    *first_index,
                    FolderPath::Destination,
                    &first.destination,
                    *second_index,
                    FolderPath::Source,
                    &second.source,
                )?;
            }
        }

        let required_bytes = self.required_bytes()?;
        if required_bytes > available_bytes {
            return Err(ReviewError::InsufficientSpace {
                required_bytes,
                available_bytes,
            });
        }

        Ok(())
    }

    fn active_folders(&self) -> Vec<(usize, &FolderSelection)> {
        self.folders
            .iter()
            .enumerate()
            .filter(|(_, folder)| folder.mode != FolderMode::Later)
            .collect()
    }
}

fn validate_path(
    folder_index: usize,
    path_kind: FolderPath,
    path: &Path,
) -> Result<(), ReviewError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(ReviewError::InvalidPath {
            folder_index,
            path: path_kind,
        });
    }
    Ok(())
}

fn reject_overlap(
    first_folder_index: usize,
    first_path: FolderPath,
    first: &Path,
    second_folder_index: usize,
    second_path: FolderPath,
    second: &Path,
) -> Result<(), ReviewError> {
    if first.starts_with(second) || second.starts_with(first) {
        return Err(ReviewError::OverlappingPaths {
            first_folder_index,
            first_path,
            second_folder_index,
            second_path,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    const TEST_ROOT: &str = r"C:\tilo-setup-model-tests";
    #[cfg(not(windows))]
    const TEST_ROOT: &str = "/tilo-setup-model-tests";

    fn test_path(path: &str) -> PathBuf {
        match path.strip_prefix('/') {
            Some(relative_to_test_root) => PathBuf::from(TEST_ROOT).join(relative_to_test_root),
            None => PathBuf::from(path),
        }
    }

    fn folder(source: &str, destination: &str, bytes: u64, mode: FolderMode) -> FolderSelection {
        FolderSelection {
            source: test_path(source),
            destination: test_path(destination),
            bytes,
            mode,
            readable: true,
            safe_sync: true,
        }
    }

    fn plan(folders: Vec<FolderSelection>) -> SetupPlan {
        SetupPlan {
            experience: Experience::FullOs,
            folders,
            system_bytes: 40,
            staging_bytes: 10,
            headroom_bytes: 5,
        }
    }

    fn full_os_caps() -> Capabilities {
        Capabilities {
            host: HostOs::Linux,
            desktop_session: false,
            internal_dual_boot: true,
        }
    }

    /// Desktop and full OS availability.
    #[test]
    fn experience_availability_requires_host_evidence() {
        // Phase `windows_does_not_offer_desktop_from_a_malformed_probe`.
        {
            let caps = Capabilities {
                host: HostOs::Windows,
                desktop_session: true,
                internal_dual_boot: false,
            };
            assert!(!caps.supports(Experience::Desktop));
            assert!(!caps.supports(Experience::FullOs));
        }

        // Phase `linux_desktop_requires_session_evidence`.
        {
            let unavailable = Capabilities {
                host: HostOs::Linux,
                desktop_session: false,
                internal_dual_boot: false,
            };
            let available = Capabilities {
                desktop_session: true,
                ..unavailable
            };

            assert!(!unavailable.supports(Experience::Desktop));
            assert!(available.supports(Experience::Desktop));
        }

        // Phase `full_os_requires_dual_boot_evidence_on_a_supported_host`.
        {
            for host in [HostOs::Windows, HostOs::Linux] {
                let unavailable = Capabilities {
                    host,
                    desktop_session: false,
                    internal_dual_boot: false,
                };
                let available = Capabilities {
                    internal_dual_boot: true,
                    ..unavailable
                };
                assert!(!unavailable.supports(Experience::FullOs));
                assert!(available.supports(Experience::FullOs));
            }

            let other = Capabilities {
                host: HostOs::Other,
                desktop_session: true,
                internal_dual_boot: true,
            };
            assert!(!other.supports(Experience::Desktop));
            assert!(!other.supports(Experience::FullOs));
        }

        // Phase `unsupported_experience_is_reported`.
        {
            let plan = SetupPlan {
                experience: Experience::Desktop,
                ..plan(Vec::new())
            };

            assert_eq!(
                plan.validate(&full_os_caps(), 55),
                Err(ReviewError::UnsupportedExperience {
                    experience: Experience::Desktop,
                })
            );
        }
    }

    /// Storage accounting and per-mode access requirements.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn storage_and_access_requirements_follow_each_folder_mode() {
        // Phase `storage_counts_copy_and_connected_but_not_later`.
        {
            let plan = plan(vec![
                folder("/source/copy", "/tilo/copy", 7, FolderMode::Copy),
                folder(
                    "/source/connected",
                    "/tilo/connected",
                    11,
                    FolderMode::Connected,
                ),
                folder("relative/later", "also/relative", 99, FolderMode::Later),
            ]);

            assert_eq!(plan.required_bytes(), Ok(73));
            assert_eq!(
                plan.validate(&full_os_caps(), 72),
                Err(ReviewError::InsufficientSpace {
                    required_bytes: 73,
                    available_bytes: 72,
                })
            );
            assert_eq!(plan.validate(&full_os_caps(), 73), Ok(()));
        }

        // Phase `storage_overflow_is_reported`.
        {
            let overflowing = SetupPlan {
                system_bytes: u64::MAX,
                staging_bytes: 1,
                ..plan(Vec::new())
            };

            assert_eq!(overflowing.required_bytes(), Err(ReviewError::Overflow));
            assert_eq!(
                overflowing.validate(&full_os_caps(), u64::MAX),
                Err(ReviewError::Overflow)
            );
        }

        // Phase `copy_requires_a_readable_source`.
        {
            let mut selection = folder("/source", "/tilo/source", 1, FolderMode::Copy);
            selection.readable = false;

            assert_eq!(
                plan(vec![selection]).validate(&full_os_caps(), 56),
                Err(ReviewError::InaccessibleSource { folder_index: 0 })
            );
        }

        // Phase `connected_requires_readability_and_safe_sync`.
        {
            let mut unreadable = folder("/source", "/tilo/source", 1, FolderMode::Connected);
            unreadable.readable = false;
            assert_eq!(
                plan(vec![unreadable]).validate(&full_os_caps(), 56),
                Err(ReviewError::InaccessibleSource { folder_index: 0 })
            );

            let mut unsafe_sync = folder("/source", "/tilo/source", 1, FolderMode::Connected);
            unsafe_sync.safe_sync = false;
            assert_eq!(
                plan(vec![unsafe_sync]).validate(&full_os_caps(), 56),
                Err(ReviewError::UnavailableConnection { folder_index: 0 })
            );
        }

        // Phase `later_ignores_access_paths_and_storage`.
        {
            let selection = FolderSelection {
                source: PathBuf::from("relative/../source"),
                destination: PathBuf::from("relative/destination"),
                bytes: u64::MAX,
                mode: FolderMode::Later,
                readable: false,
                safe_sync: false,
            };

            assert_eq!(plan(vec![selection]).validate(&full_os_caps(), 55), Ok(()));
        }
    }

    /// Path validity and overlap rules.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn active_paths_are_absolute_and_never_duplicate_or_overlap() {
        // Phase `active_paths_must_be_absolute_without_parent_traversal`.
        {
            let cases = [
                ("relative/source", "/tilo/source", FolderPath::Source),
                ("/source", "relative/destination", FolderPath::Destination),
                ("/source/../private", "/tilo/source", FolderPath::Source),
                ("/source", "/tilo/../private", FolderPath::Destination),
            ];

            for (source, destination, path) in cases {
                assert_eq!(
                    plan(vec![folder(source, destination, 1, FolderMode::Copy)])
                        .validate(&full_os_caps(), 56),
                    Err(ReviewError::InvalidPath {
                        folder_index: 0,
                        path,
                    })
                );
            }
        }

        // Phase `source_and_destination_must_not_overlap`.
        {
            let cases = [
                ("/home/user/Documents", "/home/user/Documents/tilo"),
                ("/home/user/Documents/tilo", "/home/user/Documents"),
                ("/home/user/Documents", "/home/user/Documents"),
            ];

            for (source, destination) in cases {
                assert_eq!(
                    plan(vec![folder(source, destination, 1, FolderMode::Copy)])
                        .validate(&full_os_caps(), 56),
                    Err(ReviewError::OverlappingPaths {
                        first_folder_index: 0,
                        first_path: FolderPath::Source,
                        second_folder_index: 0,
                        second_path: FolderPath::Destination,
                    })
                );
            }
        }

        // Phase `active_sources_must_not_duplicate_or_overlap`.
        {
            for second_source in [
                "/home/user",
                "/home/user/Documents",
                "/home/user/Documents/work",
            ] {
                let selections = vec![
                    folder("/home/user/Documents", "/tilo/first", 1, FolderMode::Copy),
                    folder(second_source, "/tilo/second", 1, FolderMode::Copy),
                ];
                assert_eq!(
                    plan(selections).validate(&full_os_caps(), 57),
                    Err(ReviewError::OverlappingPaths {
                        first_folder_index: 0,
                        first_path: FolderPath::Source,
                        second_folder_index: 1,
                        second_path: FolderPath::Source,
                    })
                );
            }
        }

        // Phase `active_destinations_must_not_duplicate_or_overlap`.
        {
            for second_destination in ["/tilo", "/tilo/first", "/tilo/first/nested"] {
                let selections = vec![
                    folder("/source/first", "/tilo/first", 1, FolderMode::Copy),
                    folder("/source/second", second_destination, 1, FolderMode::Copy),
                ];
                assert_eq!(
                    plan(selections).validate(&full_os_caps(), 57),
                    Err(ReviewError::OverlappingPaths {
                        first_folder_index: 0,
                        first_path: FolderPath::Destination,
                        second_folder_index: 1,
                        second_path: FolderPath::Destination,
                    })
                );
            }
        }

        // Phase `source_destination_overlap_across_selections_is_rejected`.
        {
            let selections = vec![
                folder("/source/first", "/tilo/first", 1, FolderMode::Copy),
                folder("/tilo", "/target/second", 1, FolderMode::Copy),
            ];

            assert_eq!(
                plan(selections).validate(&full_os_caps(), 57),
                Err(ReviewError::OverlappingPaths {
                    first_folder_index: 0,
                    first_path: FolderPath::Destination,
                    second_folder_index: 1,
                    second_path: FolderPath::Source,
                })
            );
        }

        // Phase `path_comparisons_use_components_not_string_prefixes`.
        {
            let selections = vec![
                folder("/home/a", "/tilo/a", 1, FolderMode::Copy),
                folder("/home/ab", "/tilo/ab", 1, FolderMode::Copy),
            ];

            assert_eq!(plan(selections).validate(&full_os_caps(), 57), Ok(()));
        }

        // Phase `later_entries_do_not_reserve_paths`.
        {
            let selections = vec![
                folder("/source", "/tilo/source", 1, FolderMode::Copy),
                folder("/source", "/tilo/source", u64::MAX, FolderMode::Later),
            ];

            assert_eq!(plan(selections).validate(&full_os_caps(), 56), Ok(()));
        }
    }
}
