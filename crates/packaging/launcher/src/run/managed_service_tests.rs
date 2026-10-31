use super::*;

fn manifest(kind: &str, entrypoint_type: &str) -> ManifestView {
    crate::manifest::test_manifest(
        &format!("id = \"so.tilo.worker\"\nversion = \"1\"\nkind = \"{kind}\""),
        &format!("[entrypoint]\nbin = \"bin/worker\"\ntype = \"{entrypoint_type}\"\n"),
    )
}

/// A page opened for an agent reaches the app as its address and then
/// the app's visitor switch, so the window browses without the person's
/// sign-ins; without the switch the app is handed the address alone.
#[test]
fn a_visitor_launch_hands_the_app_its_visitor_switch() {
    let mut opts = RunOptions {
        open_uri: Some("https://example.com/".into()),
        ..RunOptions::default()
    };
    assert_eq!(gui_open_args(&opts), ["https://example.com/"]);
    opts.open_as_visitor = true;
    assert_eq!(
        gui_open_args(&opts),
        [
            "https://example.com/",
            runtime_portal::windows::VISITOR_WINDOW_FLAG
        ]
    );
}

fn service_options() -> RunOptions {
    RunOptions {
        mode: LaunchMode::Service {
            passthrough_argv: vec!["--serve".into()],
        },
        ..RunOptions::default()
    }
}

#[test]
fn owned_service_refuses_policy_relaxation_before_touching_installation() {
    let strict = RunOptions {
        mode: LaunchMode::Service {
            passthrough_argv: vec![],
        },
        expected_package_hash: Some("not-an-installed-package".into()),
        identity: IdentitySource::None,
        ..RunOptions::default()
    };
    let mut variants = Vec::new();
    let mut opts = strict.clone();
    opts.mode = LaunchMode::Gui;
    variants.push(opts);
    let mut opts = strict.clone();
    opts.expected_package_hash = None;
    variants.push(opts);
    let mut opts = strict.clone();
    opts.identity = IdentitySource::UserStore;
    variants.push(opts);
    let mut opts = strict.clone();
    opts.landlock_enforce = false;
    variants.push(opts);
    let mut opts = strict.clone();
    opts.landlock_best_effort = true;
    variants.push(opts);
    let mut opts = strict.clone();
    opts.allow_missing_runtime = true;
    variants.push(opts);
    let mut opts = strict;
    opts.allow_unsigned_runtime = true;
    variants.push(opts);
    for opts in variants {
        let result = validate_owned_service_options(&opts);
        assert!(
            matches!(result, Err(RunError::InvalidServiceLaunch(message))
                if message == "owned service requires signed, confined, identity-free launch")
        );
    }
}

#[test]
fn owned_service_requires_original_installation() {
    let opts = RunOptions {
        mode: LaunchMode::Service {
            passthrough_argv: vec![],
        },
        expected_package_hash: Some("not-an-installed-package".into()),
        identity: IdentitySource::None,
        // Omitted app id means no installation lookup can touch user state.
        app_id: None,
        ..RunOptions::default()
    };
    validate_owned_service_options(&opts).unwrap();
    let result = verified_original_manifest(
        Path::new("/missing/owned-service.toml"),
        Path::new("/missing/payload"),
        &opts,
        "not-an-installed-package",
    );
    assert!(
        matches!(result, Err(RunError::InvalidServiceLaunch(message))
            if message == "original signed package launch changed")
    );
}

#[test]
fn signal_source_manifest_is_admitted_as_a_headless_managed_service() {
    let manifest: ManifestView = toml::from_str(include_str!("../signal-provider.toml")).unwrap();
    let options = RunOptions {
        app_id: Some("so.tilo.signal-provider".into()),
        mode: LaunchMode::Service {
            passthrough_argv: vec![],
        },
        no_services: true,
        identity: IdentitySource::None,
        ..RunOptions::default()
    };
    assert_eq!(
        manifest.entrypoint.cli_bin(),
        Some("bin/tilo-signal-provider")
    );
    validate_launch_mode(&manifest, &options).unwrap();
}

#[test]
fn managed_service_launches_are_confined_to_headless_service_packages() {
    {
        let service = manifest("service", "cli");
        assert!(validate_launch_mode(&service, &service_options()).is_ok());
        assert!(matches!(
            validate_launch_mode(&manifest("app", "cli"), &service_options()),
            Err(RunError::InvalidServiceLaunch(_))
        ));
        assert!(matches!(
            validate_launch_mode(&manifest("service", "gui"), &service_options()),
            Err(RunError::InvalidServiceLaunch(_))
        ));
    }
    {
        let manifest = manifest("service", "cli");
        let display = RunOptions {
            wayland_socket: Some("/run/user/1000/wayland-0".into()),
            ..service_options()
        };
        assert!(validate_launch_mode(&manifest, &display).is_err());
        let document = RunOptions {
            open_document: Some("/tmp/document".into()),
            ..service_options()
        };
        assert!(validate_launch_mode(&manifest, &document).is_err());
    }
    {
        let service = RunOptions {
            app_id: Some("so.tilo.other".into()),
            ..service_options()
        };
        assert!(matches!(
            validate_launch_mode(&manifest("service", "cli"), &service),
            Err(RunError::AppIdMismatch { .. })
        ));
        let gui = RunOptions {
            app_id: Some("so.tilo.other".into()),
            ..RunOptions::default()
        };
        assert!(matches!(
            validate_launch_mode(&manifest("app", "gui"), &gui),
            Err(RunError::AppIdMismatch { .. })
        ));
        let exact = RunOptions {
            app_id: Some("so.tilo.worker".into()),
            ..RunOptions::default()
        };
        assert!(validate_launch_mode(&manifest("app", "gui"), &exact).is_ok());
    }
}
