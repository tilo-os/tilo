// The same capability stream must retain each request's own reply.
use std::io::Read;
use std::os::unix::net::UnixStream;
use tilo_model_gateway::{Frame, RPC_VERSION, Request, Response, read_frame, write_frame};
struct WireProviderKeys(UnixStream);
impl WireProviderKeys {
    fn exchange(&self, request: Request) -> Result<Response, String> {
        let mut stream = self.0.try_clone().map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .map_err(|error| error.to_string())?;
        write_frame(
            &mut stream,
            &Frame {
                version: RPC_VERSION,
                value: request,
            },
        )
        .map_err(|error| error.to_string())?;
        read_frame(&mut stream).map_err(|error| error.to_string())
    }
}
impl ProviderKeyControl for WireProviderKeys {
    fn configured(&self, profile: &str) -> Result<bool, String> {
        self.exchange(Request::SecretStatus {
            profile_id: profile.into(),
        })
        .map(|reply| reply.configured.unwrap_or(false))
    }
    fn store(&self, _: &str, _: String) -> Result<(), String> {
        Err("not exercised".into())
    }
    fn remove(&self, profile: &str) -> Result<(), String> {
        let reply = self.exchange(Request::RemoveApiKey {
            profile_id: profile.into(),
        })?;
        if reply.ok {
            Ok(())
        } else {
            Err(reply.error.unwrap_or_default())
        }
    }
}
fn gateway_reply(configured: Option<bool>) -> Response {
    Response {
        ok: true,
        output: None,
        configured,
        error: None,
        event: None,
    }
}
#[test]
fn passive_key_status_and_mutation_keep_ownership_of_real_framed_replies() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let connection = Arc::new(WireProviderKeys(client));
    let control = Arc::new(SerialProviderKeys {
        control: connection.clone(),
    });
    let later_view = Arc::new(SerialProviderKeys {
        control: connection,
    });
    let (entered, first) = std::sync::mpsc::channel();
    let (attempt, attempted) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let first: Frame<Request> = read_frame(&mut server).unwrap();
        let is_status =
            matches!(first.value, Request::SecretStatus { profile_id } if profile_id == "fixture");
        entered.send(()).unwrap();
        attempted
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        server
            .set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        let mut length = [0; 4];
        let serialized = server.read_exact(&mut length).is_err();
        server
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        write_frame(&mut server, &gateway_reply(Some(true))).unwrap();
        let second: Frame<Request> = read_frame(&mut server).unwrap();
        let is_remove =
            matches!(second.value, Request::RemoveApiKey { profile_id } if profile_id == "fixture");
        write_frame(&mut server, &gateway_reply(None)).unwrap();
        (is_status, serialized, is_remove)
    });
    let status = std::thread::spawn(move || control.configured("fixture"));
    first
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let mutation = {
        std::thread::spawn(move || {
            attempt.send(()).unwrap();
            later_view.remove("fixture")
        })
    };
    let status = status.join().unwrap();
    let mutation = mutation.join().unwrap();
    let observed = server.join().unwrap();
    assert_eq!(status, Ok(true), "status owns its configured reply");
    assert_eq!(mutation, Ok(()), "mutation owns its separate unit reply");
    assert_eq!(
        observed,
        (true, true, true),
        "whole request/reply is serialized on the actual stream"
    );
}
#[test]
fn provider_removal_declares_a_unit_job_and_local_adoption_uses_current_profiles() {
    let profile = tempfile::tempdir().unwrap();
    let mut model = AgentModelsModel::new(
        PaneCx::in_file(profile.path().join("settings.json")),
        Arc::new(SampleManager),
    );
    model
        .change_providers(AgentModelsEffect::AddProvider {
            protocol: "anthropic".into(),
        })
        .unwrap();
    let id = model.providers.get().config.profiles[0].id.clone();
    let effect = AgentModelsEffect::RemoveProvider { provider: id };
    let spec = SettingsEffect::from(effect.clone()).bound();
    assert_eq!(spec.contract.result, tilo_actions::ResultSpec::Job);
    model
        .change_providers(AgentModelsEffect::AddProvider {
            protocol: "openai-compatible".into(),
        })
        .unwrap();
    let retained = model.providers.get().config.profiles[1].id.clone();
    model.change_providers(effect.clone()).unwrap();
    assert_eq!(model.providers.get().config.profiles.len(), 1);
    assert_eq!(model.providers.get().config.profiles[0].id, retained);
    assert!(
        model.change_providers(effect).is_err(),
        "removed/replaced profile is revalidated, never guessed"
    );
}
