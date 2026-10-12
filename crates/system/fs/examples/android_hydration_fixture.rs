//! Synthetic Android/FUSE journey support using production identity and store APIs.
//! Never reads an existing identity, creates platform access, fetches, or evicts.
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use fs_protocol::{
    FilePermission, FileRecordId, SHARE_CONTROL_VERSION, ShareControlOperationV1,
    ShareControlRequestV1, ShareControlResultV1,
};
use getrandom::{SysRng, rand_core::UnwrapErr};
use iroh_blobs::{Hash, api::blobs::BlobStatus};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
    persist::{write_device_cert, write_public_key, write_signing_key},
    sign_device_cert,
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// Make a fresh synthetic identity; refuse any existing directory.
    Identity {
        root: PathBuf,
        #[arg(long)]
        owner: bool,
    },
    /// Write an owned test share to a new private file, never stdout.
    Share {
        socket: PathBuf,
        record: uuid::Uuid,
        output: PathBuf,
    },
    /// Inspect the stopped recipient's actual persistent backend without fetching.
    Inspect {
        cache: PathBuf,
        expected: PathBuf,
        manifest_hash: String,
        #[arg(long)]
        resident: bool,
    },
}

fn identity(root: &Path, owner: bool) -> Result<()> {
    std::fs::DirBuilder::new().create(root)?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    let paths = IdentityPaths::under(root);
    paths.ensure_directories()?;
    let mut random = UnwrapErr(SysRng);
    let person = SigningKey::generate(&mut random);
    let device = SigningKey::generate(&mut random);
    let device = adopt_device_key(&paths, &device.to_bytes())?;
    write_public_key(&paths.user_pubkey, &person.verifying_key().to_bytes())?;
    if owner {
        write_signing_key(&paths.user_privkey, &person)?;
    }
    let mut certificate = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "owned-android-hydration-fixture".into(),
        capabilities: if owner {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, &person)?;
    write_device_cert(&paths.device_cert, &certificate)?;
    println!("synthetic identity created");
    Ok(())
}

async fn share(socket: &Path, record: uuid::Uuid, output: &Path) -> Result<()> {
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    fs::write_share_control_request(
        &mut stream,
        &ShareControlRequestV1 {
            protocol_version: SHARE_CONTROL_VERSION,
            operation: ShareControlOperationV1::CreateBearerV2 {
                record_id: FileRecordId(record),
                permission: FilePermission::View,
                expires_at_ms: tilo_clock::unix_now_ms().saturating_add(30 * 60 * 1_000),
                max_uses: None,
            },
        },
    )
    .await?;
    let response = fs::read_share_control_response(&mut stream).await?;
    let ShareControlResultV1::BearerCreatedV2 { uri } = response.result else {
        anyhow::bail!("fixture share rejected: {:?}", response.result);
    };
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    writeln!(destination, "{uri}")?;
    println!("synthetic view share stored privately");
    Ok(())
}

async fn inspect(cache: &Path, expected: &Path, hash: &str, resident: bool) -> Result<()> {
    ensure!(cache.is_dir(), "recipient backend must already exist");
    let length = std::fs::metadata(expected)?.len();
    ensure!(length > 0 && length <= 4096, "not a bounded fixture");
    let bytes = std::fs::read(expected)?;
    let (manifest, _) = fs::content::build(&bytes, None);
    ensure!(
        hex::encode(manifest.id()) == hash,
        "fixture content identity differs"
    );
    let store = fs::ContentStore::open(cache).await?;
    let mut blobs = vec![(Hash::from(manifest.id()), manifest.encode())];
    for (index, chunk) in manifest.chunks().iter().enumerate() {
        let range = manifest.chunk_range(index).context("missing chunk range")?;
        let start = usize::try_from(range.start)?;
        let end = usize::try_from(range.end)?;
        blobs.push((
            Hash::from(chunk.hash),
            bytes
                .get(start..end)
                .context("invalid chunk range")?
                .to_vec(),
        ));
    }
    for (blob, expected_bytes) in &blobs {
        let status = store.store().blobs().status(*blob).await?;
        if resident {
            ensure!(
                matches!(status, BlobStatus::Complete { .. }),
                "fixture blob is absent or partial"
            );
            let observed = store.store().blobs().get_bytes(*blob).await?;
            ensure!(
                observed.as_ref() == expected_bytes.as_slice(),
                "stored fixture bytes differ"
            );
        } else {
            ensure!(
                matches!(status, BlobStatus::NotFound),
                "evicted fixture blob remains in the backend"
            );
        }
    }
    store.shutdown().await?;
    println!(
        "{} manifest/chunk blobs {} in the persistent backend",
        blobs.len(),
        if resident { "verified" } else { "absent" }
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Operation::Identity { root, owner } => identity(&root, owner),
        Operation::Share {
            socket,
            record,
            output,
        } => share(&socket, record, &output).await,
        Operation::Inspect {
            cache,
            expected,
            manifest_hash,
            resident,
        } => inspect(&cache, &expected, &manifest_hash, resident).await,
    }
}
