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

/// `packed`'s image as it was written at `format`: the same manifest and
/// payload under a root of that format, signed by `identity`.
fn at_format(
    packed: &tilo_app_package::Packed,
    format: u32,
    identity: &SigningIdentity,
) -> tilo_app_package::PackageMeta {
    let mut root = tilo_app_package::SignedRoot {
        format,
        signature: Some([0; 64]),
        ..packed.meta.root.clone()
    };
    root.signature = Some(identity.sign_signed_root(&root).unwrap());
    tilo_app_package::PackageMeta::from_root(
        root,
        packed.meta.manifest_bytes.clone(),
        packed.meta.collection_bytes.clone(),
    )
    .unwrap()
}

/// Environments name their images by id, and the id hashes the format the
/// image was written at, so an image of every format that shipped is read
/// as it is: archived, read back and unpacked under the same id.
#[test]
fn an_image_of_every_shipped_format_reads_under_its_own_id() {
    let src = tempfile::tempdir().unwrap();
    userland(src.path());
    let identity = SigningIdentity::generate();
    let packed = seal_image(src.path(), &spec(Kind::EnvRuntime, None), &identity).unwrap();
    assert_eq!(tapp::READABLE_FORMATS, [1, tapp::TAPP_FORMAT_VERSION]);
    for format in tapp::READABLE_FORMATS {
        let meta = at_format(&packed, format, &identity);
        let bytes = tapp::to_bytes(&meta, Some(&packed)).unwrap();
        let read = tapp::read(bytes.as_slice(), &mut DiscardBlobs).unwrap();
        assert_eq!(read.meta.id, meta.id, "format {format}");
        assert_eq!(read.meta.root.format, format);
        assert_eq!(read.meta.signed_root_bytes(), meta.signed_root_bytes());
        assert!(
            matches!(
                tilo_app_package::verify(&read.meta, &[identity.public_key_bytes()]),
                tilo_app_package::Verification::Trusted { .. }
            ),
            "format {format}"
        );
        let out = tempfile::tempdir().unwrap();
        unpack(&read.meta, &packed, &out.path().join("tree")).unwrap();
        assert!(out.path().join("tree/usr/bin/bash").is_file());
    }
}

/// The env-runtime Tilo OS shipped at package format 1 (44.1.0, in the
/// system store of every device installed before format 2), byte for byte:
/// its root decodes, re-encodes to the same bytes and so the same id, its
/// signature holds, and it commits to the manifest it shipped with.
#[test]
fn the_shipped_format_1_env_runtime_root_reads() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shipped/env-runtime-44.1.0-format-1");
    // The 173 bytes, hex-encoded: the repository holds text only.
    let bytes = hex::decode(
        std::fs::read_to_string(dir.join("signed-root.hex"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    let manifest = std::fs::read(dir.join("manifest.toml")).unwrap();
    let root = tilo_app_package::SignedRoot::decode(&bytes).unwrap();
    assert_eq!(root.format, 1);
    assert_eq!(root.encode().as_slice(), bytes.as_slice());
    assert_eq!(
        root.package_id().to_hex(),
        "6dbc169b5f606a9db2527dbf17620b653497f772400220b379f68b0b53ed612f"
    );
    assert_eq!(
        tilo_app_package::BlobHash::of(&manifest),
        root.manifest_hash
    );
    let publisher = ed25519_dalek::VerifyingKey::from_bytes(&root.publisher_identity).unwrap();
    publisher
        .verify_strict(
            &root.signing_bytes(),
            &ed25519_dalek::Signature::from_bytes(&root.signature.unwrap()),
        )
        .unwrap();
    let manifest =
        tilo_app_package::Manifest::from_toml_str(std::str::from_utf8(&manifest).unwrap()).unwrap();
    assert_eq!(manifest.package.kind, Kind::EnvRuntime);
}
