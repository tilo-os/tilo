use super::capacity_mib;
use std::error::Error;
use std::fs;
use tempfile::TempDir;

const PHYSICAL: &str = "MemTotal: 33554432 kB\nMemAvailable: 1024 kB\n";
const SLICE: &str = "LoadState=loaded\nControlGroup=/user.slice/app.slice\n";

fn tree(parent: &str, selected: &str) -> Result<TempDir, Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    fs::create_dir_all(root.path().join("user.slice/app.slice"))?;
    fs::write(root.path().join("cgroup.controllers"), "cpu memory pids\n")?;
    fs::write(root.path().join("user.slice/memory.max"), parent)?;
    fs::write(
        root.path().join("user.slice/app.slice/memory.max"),
        selected,
    )?;
    Ok(root)
}

#[test]
fn finite_ancestors_bound_capacity_without_using_free_headroom() {
    let root = tree("21474836480\n", "30064771072\n").unwrap();
    assert_eq!(capacity_mib(PHYSICAL, SLICE, root.path()).unwrap(), 20480);
    fs::write(
        root.path().join("user.slice/app.slice/memory.max"),
        "8589934592\n",
    )
    .unwrap();
    assert_eq!(capacity_mib(PHYSICAL, SLICE, root.path()).unwrap(), 8192);
    // Limits larger than physical capacity cannot create additional RAM.
    fs::write(root.path().join("user.slice/memory.max"), "68719476736\n").unwrap();
    fs::write(
        root.path().join("user.slice/app.slice/memory.max"),
        "68719476736\n",
    )
    .unwrap();
    assert_eq!(capacity_mib(PHYSICAL, SLICE, root.path()).unwrap(), 32768);
}

#[test]
fn unlimited_visible_ancestors_preserve_physical_capacity() {
    let root = tree("max\n", "max\n").unwrap();
    // The visible cgroup mount root normally has no memory.max.
    assert_eq!(capacity_mib(PHYSICAL, SLICE, root.path()).unwrap(), 32768);
    fs::write(root.path().join("memory.max"), "max\n").unwrap();
    assert_eq!(capacity_mib(PHYSICAL, SLICE, root.path()).unwrap(), 32768);
}

#[test]
fn malformed_or_missing_limits_never_fall_back_to_physical_ram() {
    let root = tree("max\n", "max\n").unwrap();
    let parent = root.path().join("user.slice/memory.max");
    for invalid in [
        "",
        "-1",
        "+21474836480",
        "max 1",
        "1 2",
        "18446744073709551616",
        "0",
    ] {
        fs::write(&parent, invalid).unwrap();
        assert!(
            capacity_mib(PHYSICAL, SLICE, root.path()).is_err(),
            "{invalid:?}"
        );
    }
    fs::remove_file(parent).unwrap();
    assert!(capacity_mib(PHYSICAL, SLICE, root.path()).is_err());
}

#[test]
fn unobservable_manager_controller_and_physical_capacity_refuse() {
    let root = tree("max", "max").unwrap();
    for properties in [
        "LoadState=not-found\nControlGroup=\n",
        "LoadState=loaded\nControlGroup=\n",
        "LoadState=loaded\n",
        "LoadState=loaded\nControlGroup=/missing\n",
        "LoadState=loaded\nControlGroup=/user.slice/app.slice\nControlGroup=/other\n",
    ] {
        assert!(capacity_mib(PHYSICAL, properties, root.path()).is_err());
    }
    for meminfo in [
        "",
        "MemTotal: x kB",
        "MemTotal: 0 kB",
        "MemTotal: 1 MB",
        "MemTotal: 1 kB\nMemTotal: 2 kB",
    ] {
        assert!(capacity_mib(meminfo, SLICE, root.path()).is_err());
    }
    fs::write(root.path().join("cgroup.controllers"), "cpu pids").unwrap();
    assert!(capacity_mib(PHYSICAL, SLICE, root.path()).is_err());
    fs::remove_file(root.path().join("cgroup.controllers")).unwrap();
    assert!(capacity_mib(PHYSICAL, SLICE, root.path()).is_err());
}

#[test]
fn foreign_paths_symlinks_and_oversized_observations_refuse() {
    use std::os::unix::fs::symlink;
    let root = tree("max", "max").unwrap();
    let foreign = tree("max", "max").unwrap();
    for group in ["/../outside", "relative", "//outside"] {
        let properties = format!("LoadState=loaded\nControlGroup={group}\n");
        assert!(capacity_mib(PHYSICAL, &properties, root.path()).is_err());
    }
    symlink(foreign.path(), root.path().join("foreign")).unwrap();
    assert!(
        capacity_mib(
            PHYSICAL,
            "LoadState=loaded\nControlGroup=/foreign\n",
            root.path()
        )
        .is_err()
    );
    let limit = root.path().join("user.slice/app.slice/memory.max");
    fs::remove_file(&limit).unwrap();
    symlink(foreign.path().join("user.slice/memory.max"), &limit).unwrap();
    assert!(capacity_mib(PHYSICAL, SLICE, root.path()).is_err());
    fs::remove_file(&limit).unwrap();
    fs::write(&limit, "1".repeat(65_537)).unwrap();
    assert!(capacity_mib(PHYSICAL, SLICE, root.path()).is_err());
}
