use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tilo_app_package::manifest::Arch;
use tilo_app_package::{
    DiscardBlobs, ImageError, ImageSpec, Kind, ManifestError, PackError, PackageId,
    SigningIdentity, TREE_FILE, seal_image, tapp, unpack, validate_manifest,
};

fn spec(kind: Kind, runtime: Option<PackageId>) -> ImageSpec {
    ImageSpec {
        kind,
        id: "so.tilo.env-runtime".into(),
        version: "44.1.0".into(),
        name: Some("Tilo environment runtime".into()),
        arch: Arch::X86_64,
        runtime,
        packages: vec!["bash-0:5.2.37-1.fc44.x86_64".into()],
    }
}

fn userland(root: &Path) {
    std::fs::create_dir_all(root.join("usr/bin")).unwrap();
    std::fs::create_dir_all(root.join("etc/dnf/vars")).unwrap();
    std::fs::write(root.join("usr/bin/bash"), b"\x7fELF bash").unwrap();
    std::fs::set_permissions(
        root.join("usr/bin/bash"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(root.join("etc/os-release"), b"NAME=Fedora\n").unwrap();
    std::os::unix::fs::symlink("bash", root.join("usr/bin/sh")).unwrap();
    std::os::unix::fs::symlink("/usr/share/zoneinfo/UTC", root.join("etc/localtime")).unwrap();
}

#[test]
fn a_sealed_tree_unpacks_to_the_same_tree() {
    let src = tempfile::tempdir().unwrap();
    userland(src.path());
    let identity = SigningIdentity::generate();
    let packed = seal_image(src.path(), &spec(Kind::EnvRuntime, None), &identity).unwrap();
    assert!(packed.meta.collection.entries.contains_key(TREE_FILE));
    assert!(!packed.meta.collection.entries.contains_key("usr/bin/sh"));

    let bytes = tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let read = tapp::read(bytes.as_slice(), &mut DiscardBlobs).unwrap();
    assert_eq!(read.meta.id, packed.meta.id);
    assert!(read.meta.manifest.package.kind.is_image());
    validate_manifest(&read.meta.manifest).unwrap();

    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("tree");
    unpack(&packed.meta, &packed, &dest).unwrap();
    let mode = |rel: &str| {
        std::fs::metadata(dest.join(rel))
            .unwrap()
            .permissions()
            .mode()
    };
    assert_ne!(mode("usr/bin/bash") & 0o111, 0);
    assert_eq!(mode("etc/os-release") & 0o111, 0);
    assert_eq!(
        std::fs::read_link(dest.join("usr/bin/sh")).unwrap(),
        Path::new("bash")
    );
    assert_eq!(
        std::fs::read_link(dest.join("etc/localtime")).unwrap(),
        Path::new("/usr/share/zoneinfo/UTC")
    );
    assert!(dest.join("etc/dnf/vars").is_dir());
}

#[test]
fn trees_an_image_cannot_carry_are_refused() {
    let identity = SigningIdentity::generate();

    let reserved = tempfile::tempdir().unwrap();
    userland(reserved.path());
    std::fs::write(reserved.path().join(TREE_FILE), b"").unwrap();
    assert!(matches!(
        seal_image(reserved.path(), &spec(Kind::EnvRuntime, None), &identity),
        Err(ImageError::Pack(PackError::ReservedPath(_)))
    ));

    let socket = tempfile::tempdir().unwrap();
    userland(socket.path());
    let _listener = std::os::unix::net::UnixListener::bind(socket.path().join("etc/sock")).unwrap();
    assert!(matches!(
        seal_image(socket.path(), &spec(Kind::EnvRuntime, None), &identity),
        Err(ImageError::UnsupportedFile { .. })
    ));

    let tool = tempfile::tempdir().unwrap();
    userland(tool.path());
    assert!(matches!(
        seal_image(tool.path(), &spec(Kind::Tool, None), &identity),
        Err(ImageError::Pack(PackError::Manifest(
            ManifestError::ToolWithoutRuntime
        )))
    ));
    let runtime = PackageId::from_bytes([4; 32]);
    let packed = seal_image(tool.path(), &spec(Kind::Tool, Some(runtime)), &identity).unwrap();
    assert_eq!(
        packed.meta.manifest.image.as_ref().unwrap().runtime,
        Some(runtime)
    );
}
