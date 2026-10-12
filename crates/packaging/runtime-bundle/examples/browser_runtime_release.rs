//! Explicit native proof of the maintained Browser engine release pipeline.
//! Builds only the pinned engine and release CLIs, then verifies a signed thin package.
use std::error::Error;
use std::path::Path;
use std::process::Command;

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (source, commit) = match arguments.as_slice() {
        [] => {
            let result = Command::new("python3")
                .args(["tools/apps/browser_runtime_release.py", "source-proof"])
                .current_dir(&root)
                .output()?;
            if !result.status.success() {
                return Err(format!(
                    "source admission failed; use the maintained proof-cargo launcher: {}",
                    String::from_utf8_lossy(&result.stderr)
                )
                .into());
            }
            let proof: serde_json::Value = serde_json::from_slice(&result.stdout)?;
            (
                proof
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("source proof has no commit identity")?
                    .to_owned(),
                proof
                    .get("commit")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("source proof has no commit object")?
                    .to_owned(),
            )
        }
        [source, commit] => (source.clone(), commit.clone()),
        _ => return Err("expected source revision and actual encoded commit object".into()),
    };
    let architecture = std::env::consts::ARCH;
    let output = format!("target/browser-runtime-release-proof-{source}");
    let status = Command::new("python3")
        .current_dir(&root)
        .args([
            "tools/apps/browser_runtime_release.py",
            "native-proof",
            "--out",
            &output,
            "--arch",
            architecture,
            "--source",
            &source,
            "--source-commit",
            &commit,
        ])
        .env("REQUESTED_PUBLISH", "false")
        .status()?;
    if !status.success() {
        return Err(format!("native engine release proof failed: {status}").into());
    }
    let package = root.join(&output).join("package");
    let payload_blob = package
        .read_dir()?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "blob"))
        .ok_or("signed engine package contains no payload blob")?;
    let hidden_blob = payload_blob.with_extension("missing");
    let thin_package = package
        .read_dir()?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "tapp"))
        .ok_or("signed engine package contains no thin archive")?;
    std::fs::rename(&payload_blob, &hidden_blob)?;
    let result = Command::new(root.join(&output).join("bin/tilo-app-package"))
        .args(["entry", "--arch", architecture, "--objects"])
        .arg(&package)
        .arg("--identity")
        .arg(root.join(&output).join("work/app-identity.json"))
        .args(["--url", "https://apps.tilo.so/artifacts/engine.tapp"])
        .arg(thin_package)
        .output();
    std::fs::rename(hidden_blob, &payload_blob)?;
    let result = result?;
    if result.status.success() {
        return Err("canonical engine verifier accepted a missing signed payload object".into());
    }
    let stderr = String::from_utf8(result.stderr)?;
    let removed_hash = payload_blob
        .file_stem()
        .ok_or("payload object has no hash")?
        .to_string_lossy();
    if !stderr.contains(removed_hash.as_ref())
        || !(stderr.contains("no payload object for blob") || stderr.contains(" is missing"))
    {
        return Err(format!(
            "canonical verifier failed without identifying the missing payload: {stderr}"
        )
        .into());
    }
    println!("native signed engine pipeline passed; missing payload object rejected");
    Ok(())
}
