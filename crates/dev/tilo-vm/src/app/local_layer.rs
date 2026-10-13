use tilo_vm_protocol::{LayerImport, LayerSource, LayerVerify};

use super::{App, Outcome};
use crate::cli::LocalLayerImport;
use crate::error::CliError;
use crate::local_layer::Prepared;
use crate::remote::Remote;

fn import(remote: &mut Remote, prepared: &mut Prepared) -> Result<(String, bool), CliError> {
    let upload = remote.upload(prepared.archive.as_file_mut())?;
    let imported = remote.call(LayerImport {
        source: LayerSource::Upload(upload),
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
        // Freeze and validate inputs before any transport or daemon mutation.
        let mut prepared = crate::local_layer::prepare(request)?;
        let vm = self.resolve_vm(None)?;
        // Importing an inert layer never starts or wakes a stopped VM.
        let mut remote = self.connect(&vm)?;
        let expected_target = format!("{}-unknown-linux-gnu", remote.hello.arch);
        if prepared.provenance.target.as_deref() != Some(expected_target.as_str()) {
            return Err(CliError::InvalidInput(
                "local layer target does not match the selected VM".into(),
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
    use super::*;
    use crate::hypervisor::CommandLine;

    const PEER: &str = r#"
import base64,json,sys
hash,corrupt,log,protocol=sys.argv[1:]
print(json.dumps({"tilo_device_admin":{"device":"test","label":"test","route":"direct","via":"test","session":1}}),flush=True)
received=0
provenance=None
for line in sys.stdin:
 request=json.loads(line);method=request['method']
 with open(log,'a') as file:file.write(method+'\n')
 if method=='hello':result={"protocol":int(protocol),"daemon_version":"test","root":"test","arch":"x86_64"}
 elif method=='layer.upload.begin':result={"upload":"test"}
 elif method=='layer.upload.chunk':
  received+=len(base64.b64decode(request['params']['data']));result={"received":received}
 elif method=='layer.import':
  provenance=request['params']['provenance'];result={"hash":'c'*64 if corrupt=='content' else hash,"existed":False}
 elif method=='layer.verify':
  if corrupt=='unsupported':
   print(json.dumps({"jsonrpc":"2.0","id":request['id'],"error":{"code":-32601,"message":"layer.verify unavailable","hint":"use a daemon with native layer verification","see":None}}),flush=True)
   continue
  if corrupt=='yes':provenance['source_commit']='f'*40
  result={"hash":hash,"provenance":provenance}
 else:raise RuntimeError(method)
 print(json.dumps({"jsonrpc":"2.0","id":request['id'],"result":result}),flush=True)
"#;

    #[test]
    fn native_import_verifies_provenance_without_build_wake_or_swap() {
        for corrupt in ["no", "yes"] {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("calls");
            let hash = "a".repeat(64);
            let line = CommandLine::new("python3", &["-c", PEER])
                .arg(hash.clone())
                .arg(corrupt)
                .arg(log.display().to_string())
                .arg(tilo_vm_protocol::PROTOCOL_VERSION.to_string());
            let (mut remote, _) = Remote::over_peer("test", &line).unwrap();
            let mut archive = tempfile::NamedTempFile::new().unwrap();
            std::io::Write::write_all(&mut archive, b"actual frozen bytes").unwrap();
            std::io::Seek::rewind(&mut archive).unwrap();
            let mut provenance = tilo_vm_protocol::Provenance::local("launcher");
            provenance.source_commit = Some("b".repeat(40));
            let mut prepared = Prepared {
                archive,
                provenance,
                hash: hash.clone(),
            };
            let result = import(&mut remote, &mut prepared);
            if corrupt == "yes" {
                assert!(
                    result.is_err(),
                    "a changed producer must not be reported as imported"
                );
            } else {
                assert_eq!(result.unwrap(), (hash, false));
            }
            let calls = std::fs::read_to_string(log).unwrap();
            assert_eq!(
                calls,
                "hello\nlayer.upload.begin\nlayer.upload.chunk\nlayer.import\nlayer.verify\n"
            );
        }
    }

    #[test]
    fn local_layer_import_refuses_changed_content_and_unavailable_verification() {
        for mode in ["content", "unsupported"] {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("calls");
            let hash = "a".repeat(64);
            let line = CommandLine::new("python3", &["-c", PEER])
                .arg(hash.clone())
                .arg(mode)
                .arg(log.display().to_string())
                .arg(tilo_vm_protocol::PROTOCOL_VERSION.to_string());
            let (mut remote, _) = Remote::over_peer("test", &line).unwrap();
            let mut archive = tempfile::NamedTempFile::new().unwrap();
            std::io::Write::write_all(&mut archive, b"frozen bytes").unwrap();
            std::io::Seek::rewind(&mut archive).unwrap();
            let mut prepared = Prepared {
                archive,
                provenance: tilo_vm_protocol::Provenance::local("launcher"),
                hash,
            };
            let error = import(&mut remote, &mut prepared).unwrap_err();
            let calls = std::fs::read_to_string(log).unwrap();
            let prefix = "hello\nlayer.upload.begin\nlayer.upload.chunk\nlayer.import\n";
            if mode == "content" {
                assert!(error.to_string().contains("differs"), "{error}");
                assert_eq!(calls, prefix);
            } else {
                assert!(error.to_string().contains("unavailable"), "{error}");
                assert_eq!(calls, format!("{prefix}layer.verify\n"));
            }
        }
    }

    #[test]
    fn local_layer_import_cli_requires_pinned_inputs_and_explains_application() {
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
