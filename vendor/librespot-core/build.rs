fn main() {
    // This is a crates.io source snapshot, not the enclosing Tilo Git repository.
    // Reading HEAD, the clock or SOURCE_DATE_EPOCH here changes the shared runtime
    // for unrelated app edits and invalidates otherwise reusable app collections.
    let provenance = include_str!(".cargo_vcs_info.json");
    let revision = provenance
        .lines()
        .find_map(|line| line.trim().strip_prefix("\"sha1\": \""))
        .and_then(|value| value.strip_suffix('"'))
        .expect("vendored librespot source must record its upstream revision");
    assert!(revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.cargo_vcs_info.json");
    println!("cargo:rustc-env=VERGEN_GIT_SHA={}", &revision[..10]);
    // Publication metadata carries no upstream commit/build date. Do not invent
    // one using the enclosing repository's commit date or this machine's clock.
    println!("cargo:rustc-env=VERGEN_GIT_COMMIT_DATE=unknown");
    println!("cargo:rustc-env=VERGEN_BUILD_DATE=unknown");
    println!(
        "cargo:rustc-env=LIBRESPOT_BUILD_ID={}-{revision}",
        env!("CARGO_PKG_VERSION")
    );
}
