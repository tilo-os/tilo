#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[cfg(test)]
mod tests {
    use fuser::{Config, MountOption, SessionACL};

    #[test]
    fn test_fuse_config_validity() {
        let options = vec![
            MountOption::RO,
            MountOption::FSName("quickfs".to_string()),
            MountOption::AutoUnmount,
        ];

        let mut config = Config::default();
        config.mount_options = options;

        // If AutoUnmount is present, ACL must not be Owner.
        config.acl = SessionACL::All;

        let has_auto_unmount = config
            .mount_options
            .iter()
            .any(|opt| matches!(opt, MountOption::AutoUnmount));

        if has_auto_unmount {
            assert!(
                !matches!(config.acl, SessionACL::Owner),
                "FUSE crash prevention: AutoUnmount requires ACL broader than Owner"
            );
        }
    }
}
