use tilo_device_protocol::{LayerImport, LayerSource, LayerVerify};

use super::{App, Outcome};
use crate::cli::LocalLayerImport;
use crate::error::CliError;
use crate::local_layer::Prepared;
use crate::remote::Remote;

pub(super) fn import(
    remote: &mut Remote,
    prepared: &mut Prepared,
) -> Result<(String, bool), CliError> {
    let upload = remote.put_reader(prepared.archive.as_file_mut())?;
    let imported = remote.call(LayerImport {
        source: LayerSource::Content { hash: upload },
        provenance: Box::new(prepared.provenance.clone()),
    })?;
    if imported.hash != prepared.hash {
        return Err(CliError::InvalidInput(
            "imported content differs from the prepared local layer".into(),
        ));
    }
    let verified = remote.call(LayerVerify {
        hash: imported.hash.clone(),
    })?;
    if verified.hash != prepared.hash || verified.provenance != prepared.provenance {
        return Err(CliError::InvalidInput(
            "stored local layer content or provenance differs from its producer".into(),
        ));
    }
    Ok((verified.hash, imported.existed))
}

impl App {
    pub(super) fn import_local_layer(
        &mut self,
        request: &LocalLayerImport,
    ) -> Result<Outcome, CliError> {
        let mut prepared = crate::local_layer::prepare(request)?;
        // Importing an inert layer never starts or wakes a stopped VM.
        let mut remote = if self.local {
            self.connect_here()?
        } else {
            let vm = self.resolve_vm(None)?;
            self.connect(&vm)?
        };
        let expected_target = format!("{}-unknown-linux-gnu", remote.status.arch);
        if prepared.provenance.target.as_deref() != Some(expected_target.as_str()) {
            return Err(CliError::InvalidInput(
                "local layer target does not match the machine it is imported into".into(),
            ));
        }
        let (hash, existed) = self
            .receipt
            .phase("import_local", || import(&mut remote, &mut prepared))?;
        Ok(Outcome::new(
            format!("imported {} layer {hash}", request.component),
            serde_json::json!({
                "component": request.component,
                "layer": hash,
                "provenance": prepared.provenance,
                "existed": existed,
                "applied": false,
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tilo_device_protocol::{RpcError, code};

    use super::*;
    use crate::remote::fixture::Device;

    /// A device that imports a put layer as `hash` (another hash for `content`), and answers its
    /// verification with the provenance it was imported with (changed for `changed`, refused for
    /// `unverifiable`).
    fn device(mode: &'static str, hash: &str) -> Device {
        let hash = hash.to_owned();
        let imported = Arc::new(Mutex::new(Value::Null));
        Device::serving(move |action, argument| match action {
            "layer-import" => {
                *imported.lock().unwrap() = argument["provenance"].clone();
                let hash = if mode == "content" {
                    "c".repeat(64)
                } else {
                    hash.clone()
                };
                Ok(json!({ "hash": hash, "existed": false }))
            }
            "layer-verify" if mode == "unverifiable" => Err(RpcError {
                code: code::INVALID,
                message: "layer verification unavailable".into(),
                hint: String::new(),
                see: None,
            }),
            "layer-verify" => {
                let mut provenance = imported.lock().unwrap().clone();
                if mode == "changed" {
                    provenance["source_commit"] = json!("f".repeat(40));
                }
                Ok(json!({ "hash": hash, "provenance": provenance }))
            }
            other => panic!("an import asks for no {other}"),
        })
    }

    fn prepared(bytes: &[u8], hash: &str) -> Prepared {
        let mut archive = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut archive, bytes).unwrap();
        std::io::Seek::rewind(&mut archive).unwrap();
        let mut provenance = tilo_device_protocol::Provenance::local("launcher");
        provenance.source_commit = Some("b".repeat(40));
        Prepared {
            archive,
            provenance,
            hash: hash.to_owned(),
        }
    }

    fn actions(device: &Device) -> Vec<String> {
        device
            .asked()
            .into_iter()
            .map(|asked| asked.action)
            .collect()
    }

    tilo_test_env::scenario! {
        fn a_local_layer_is_imported_only_as_verified;

        phase native_import_verifies_provenance_without_build_wake_or_swap;
        {
            for mode in ["same", "changed"] {
                let hash = "a".repeat(64);
                let device = device(mode, &hash);
                let mut remote = Remote::connect("test", &device.machine, &device.route).unwrap();
                let result = import(&mut remote, &mut prepared(b"actual frozen bytes", &hash));
                if mode == "changed" {
                    assert!(
                        result.is_err(),
                        "a changed producer must not be reported as imported"
                    );
                } else {
                    assert_eq!(result.unwrap(), (hash, false));
                }
                assert_eq!(actions(&device), ["layer-import", "layer-verify"]);
                let put = blake3::hash(b"actual frozen bytes").to_hex().to_string();
                assert_eq!(device.content.get(&put).as_deref(), Some(&b"actual frozen bytes"[..]));
                assert_eq!(
                    device.asked()[0].argument["source"],
                    json!({ "content": { "hash": put } })
                );
            }
        }

        phase local_layer_import_refuses_changed_content_and_unavailable_verification;
        {
            for mode in ["content", "unverifiable"] {
                let hash = "a".repeat(64);
                let device = device(mode, &hash);
                let mut remote = Remote::connect("test", &device.machine, &device.route).unwrap();
                let error = import(&mut remote, &mut prepared(b"frozen bytes", &hash)).unwrap_err();
                if mode == "content" {
                    assert!(error.to_string().contains("differs"), "{error}");
                    assert_eq!(actions(&device), ["layer-import"]);
                } else {
                    assert!(error.to_string().contains("unavailable"), "{error}");
                    assert_eq!(actions(&device), ["layer-import", "layer-verify"]);
                }
            }
        }

        phase local_layer_import_cli_requires_pinned_inputs_and_explains_application;
        {
            use crate::cli::{Cli, Command};
            use clap::{CommandFactory, Parser};

            let args = [
                "tilo-vm",
                "import-layer",
                "launcher",
                "--archive",
                "launcher.tar",
                "--archive-sha256",
                "producer-hash",
                "--provenance",
                "launcher.json",
                "--manifest",
                "os/components.toml",
            ];
            let parsed = Cli::try_parse_from(args).unwrap();
            let Some(Command::ImportLayer(request)) = parsed.command else {
                panic!("import-layer did not reach its maintained command");
            };
            assert_eq!(request.component, "launcher");
            assert_eq!(request.archive_sha256, "producer-hash");
            assert!(Cli::try_parse_from(&args[..5]).is_err());
            let mut command = Cli::command();
            let help = command
                .find_subcommand_mut("import-layer")
                .unwrap()
                .render_long_help()
                .to_string();
            assert!(
                help.contains("without building, waking or swapping"),
                "{help}"
            );
            assert!(help.contains("--from layer:<hash>"), "{help}");
        }
    }
}
