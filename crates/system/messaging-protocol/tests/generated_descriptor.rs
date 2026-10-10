//! Exercises the public build API as a normal workspace consumer, so CI runs
//! it even though buffa-build itself is an excluded build-only dependency.
use std::path::Path;
use std::process::Command;

use buffa_build::Config;

// FileDescriptorSet for: syntax = "proto3"; package fixture; message Before {}
const DESCRIPTOR: &[u8] = b"\x0a\x2a\x0a\x0d\x66\x69\x78\x74\x75\x72\x65\x2e\x70\x72\x6f\x74\x6f\x12\x07\x66\x69\x78\x74\x75\x72\x65\x22\x08\x0a\x06\x42\x65\x66\x6f\x72\x65\x62\x06\x70\x72\x6f\x74\x6f\x33";

fn generate(config: Config, out: &Path) {
    config
        .files(&["fixture.proto"])
        .out_dir(out)
        .include_file("mod.rs")
        .compile()
        .expect("valid descriptor must generate Rust");
}

#[test]
fn descriptor_file_generation_is_reproducible_and_observes_schema_changes() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.desc");
    std::fs::write(&input, DESCRIPTOR).unwrap();
    let original = temp.path().join("original");
    let repeat = temp.path().join("repeat");
    generate(Config::new().descriptor_set(&input), &original);
    generate(Config::new().descriptor_set(&input), &repeat);
    let mut count = 0;
    for entry in std::fs::read_dir(&original).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            std::fs::read(entry.path()).unwrap(),
            std::fs::read(repeat.join(entry.file_name())).unwrap()
        );
        count += 1;
    }
    assert!(
        count >= 2,
        "Rust source and include module must be generated"
    );
    assert_eq!(std::fs::read_dir(&repeat).unwrap().count(), count);
    let mut descriptor = DESCRIPTOR.to_vec();
    let names: Vec<_> = descriptor
        .windows(b"Before".len())
        .enumerate()
        .filter_map(|(index, name)| (name == b"Before").then_some(index))
        .collect();
    assert_eq!(names.len(), 1);
    let start = names[0];
    descriptor[start..start + b"Before".len()].copy_from_slice(b"After_");
    std::fs::write(&input, descriptor).unwrap();
    let changed = temp.path().join("changed");
    generate(Config::new().descriptor_set(&input), &changed);
    let original = std::fs::read_to_string(original.join("fixture.rs")).unwrap();
    let changed = std::fs::read_to_string(changed.join("fixture.rs")).unwrap();
    assert!(original.contains("pub struct Before"));
    assert!(!changed.contains("pub struct Before"));
    assert!(changed.contains("pub struct After"));
    assert_ne!(original, changed);
}

// Each child invokes the public production API and captures its actual Cargo
// directives without mutating this test process's environment.
#[test]
fn descriptor_tracking_distinguishes_generated_outputs_from_external_inputs() {
    if let Some(input) = std::env::var_os("TILO_DESCRIPTOR_PROBE_INPUT") {
        let out = std::env::var_os("TILO_DESCRIPTOR_PROBE_OUTPUT").unwrap();
        generate(Config::new().descriptor_set(input), Path::new(&out));
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("cargo-out");
    std::fs::create_dir(&out).unwrap();
    let generated = out.join("generated.desc");
    let external = temp.path().join("external.desc");
    std::fs::write(&generated, DESCRIPTOR).unwrap();
    std::fs::write(&external, DESCRIPTOR).unwrap();
    let sibling = temp.path().join("cargo-outside");
    std::fs::create_dir(&sibling).unwrap();
    let sibling_input = sibling.join("input.desc");
    std::fs::write(&sibling_input, DESCRIPTOR).unwrap();
    let missing = temp.path().join("missing-out");
    let mut cases = vec![
        (generated.clone(), Some(out.clone()), false),
        (external.clone(), Some(out.clone()), true),
        (sibling_input, Some(out.clone()), true),
        (generated.clone(), None, true),
        (external.clone(), Some(missing), true),
    ];
    #[cfg(unix)]
    {
        let external_link = temp.path().join("external-link.desc");
        let escaping_link = out.join("escaping-link.desc");
        std::os::unix::fs::symlink(&generated, &external_link).unwrap();
        std::os::unix::fs::symlink(&external, &escaping_link).unwrap();
        cases.push((external_link, Some(out.clone()), true));
        cases.push((escaping_link, Some(out.clone()), true));
    }
    for (index, (input, cargo_out, tracked)) in cases.into_iter().enumerate() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "descriptor_tracking_distinguishes_generated_outputs_from_external_inputs",
                "--nocapture",
            ])
            .env("TILO_DESCRIPTOR_PROBE_INPUT", &input)
            .env(
                "TILO_DESCRIPTOR_PROBE_OUTPUT",
                temp.path().join(format!("result-{index}")),
            );
        match cargo_out {
            Some(path) => {
                command.env("OUT_DIR", path);
            }
            None => {
                command.env_remove("OUT_DIR");
            }
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("1 passed"),
            "child must execute the production API: {stdout}"
        );
        assert!(stdout.contains("cargo:rerun-if-env-changed=BUFFA_ELEMENT_MEMORY_LIMIT"));
        let directive = format!("cargo:rerun-if-changed={}", input.display());
        assert_eq!(
            stdout.lines().any(|line| line == directive),
            tracked,
            "{input:?}: {stdout}"
        );
    }
}
