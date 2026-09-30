//! The `provision_test_identity` control op: mint a throwaway collaboration
//! identity for the session's user, so a test session on a dev image can open
//! apps without the first-login flow. Only builds that honour the dev
//! environment overrides ([`super::dev_env`]) answer it; every other build
//! refuses it, and its identity host carries no minting code
//! (`tilo_identity_host::provisioning::ensure_throwaway_test_identity`).

use crate::shell::test_control::Response;

/// Error code of a build that does not carry the op.
pub const DEV_IMAGE_ONLY: &str = "dev_image_only";

/// Answer `provision_test_identity` for this session's user.
#[must_use]
pub fn respond() -> Response {
    if !super::dev_env::DEV_ENV_OVERRIDES {
        return refused();
    }
    match runtime_identity::identity_dir_from_env() {
        Ok(identity_dir) => provision(
            &identity_dir,
            &crate::firstboot::collaboration_identity_paths(),
        ),
        Err(error) => Response::Err {
            code: "provision_failed".into(),
            message: format!("no identity directory: {error}"),
        },
    }
}

fn refused() -> Response {
    Response::Err {
        code: DEV_IMAGE_ONLY.into(),
        message: "throwaway test identities exist only on dev images".into(),
    }
}

fn provision(
    identity_dir: &std::path::Path,
    paths: &tilo_user_identity::IdentityPaths,
) -> Response {
    use tilo_identity_host::provisioning::{ProvisionOutcome, THROWAWAY_TEST_IDENTITY_NAME};
    match crate::firstboot::ensure_throwaway_test_identity(identity_dir, paths) {
        Ok(outcome) => Response::Answer(serde_json::json!({
            "created": matches!(outcome, ProvisionOutcome::Created { .. }),
            "display_name": THROWAWAY_TEST_IDENTITY_NAME,
            "throwaway": true,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Response::Err {
            code: "identity_exists".into(),
            message: error.to_string(),
        },
        Err(error) => Response::Err {
            code: "provision_failed".into(),
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_build_mints_one_throwaway_and_refuses_a_real_identity() {
        use tilo_user_identity::IdentityPaths;
        let tmp = tempfile::tempdir().expect("tempdir");
        let personal = tmp.path().join("config/tilo/identity");
        let paths = IdentityPaths::under(tmp.path().join("system"));
        let Response::Answer(first) = provision(&personal, &paths) else {
            panic!("a fresh user gets a throwaway identity");
        };
        assert_eq!(first["created"], true);
        assert_eq!(first["throwaway"], true);
        assert!(tilo_identity_host::provisioning::is_throwaway_test_identity(&personal));
        let Response::Answer(second) = provision(&personal, &paths) else {
            panic!("the op is idempotent for its own throwaway");
        };
        assert_eq!(second["created"], false);

        let real = tmp.path().join("real/tilo/identity");
        crate::firstboot::ensure_collaboration_identity(&real, &paths).expect("real identity");
        let Response::Err { code, .. } = provision(&real, &paths) else {
            panic!("a real identity is never adopted");
        };
        assert_eq!(code, "identity_exists");
    }
}
