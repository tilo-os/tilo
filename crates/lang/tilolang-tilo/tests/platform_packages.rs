//! Every package the platform publishes loads into one host, the session's own package
//! included: the shell and the keyboard read the session from the description they were
//! compiled against, and a host describes the same app again from the platform's table.

use tilolang_runtime::Actor;
use tilolang_tilo::host::Host;
use tilolang_tilo::platform_apps;

/// A host with no workspace: the code's authorship decides, as a window's does.
fn host() -> Host {
    Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host")
}

/// Every platform package loads, and the session's own package loads beside it: the shell and a
/// window read the session from the description they were compiled against, and the host
/// describes the same app again from the platform's table. The two are one package, so a host
/// holding one takes the other without a declaration of its own.
#[test]
fn every_platform_package_loads_and_the_session_is_one_package() {
    let host = host();
    // The session's description as the shell and the keyboard compile against it.
    let session = platform_apps::session_package().expect("the session's package reads");
    assert_eq!(session.app, platform_apps::SESSION_APP);
    host.runtime()
        .load_foreign(session)
        .expect("the session's own package loads");
    // Every platform package a window names, loaded where a host already holds the session.
    let apps: Vec<String> = platform_apps::platform_packages()
        .expect("the platform's packages read")
        .into_iter()
        .map(|meta| meta.app)
        .collect();
    assert!(!apps.is_empty(), "the platform publishes packages");
    let loaded = platform_apps::load(&host, &apps);
    assert_eq!(
        loaded, apps,
        "every platform package loads: a package left out is one a view reading it cannot open"
    );
}

/// Each platform package declares each type once: a description that names a type twice puts one
/// id on two declarations, and a host that keeps its types by id holds one of them and reads
/// whichever it met first.
#[test]
fn every_platform_package_declares_each_type_once() {
    for meta in platform_apps::platform_packages().expect("the platform's packages read") {
        let mut seen = std::collections::BTreeSet::new();
        for ty in &meta.types {
            assert!(
                seen.insert(ty.id.clone()),
                "{} declares the type `{}` twice",
                meta.app,
                ty.id
            );
        }
    }
}
