//! The accounts service (`tilo-account-service`): custody's operations for
//! sandboxed apps, over the descriptor the launcher hands them.
//!
//! Each connection starts with the launcher's handoff naming the app
//! ([`tilo_account_service::handoff`]); the app cannot write it, because the
//! launcher writes it before the app holds the descriptor. Every request is
//! then checked against what that app may ask
//! ([`tilo_account_service::Request::permitted_for`]) and answered by the
//! custodian, whose credentials never cross the connection.

use std::os::unix::net::{UnixListener, UnixStream};

use tilo_account_service::{
    ClientError, Frame, HANDOFF_FRAMING, RPC_VERSION, Request, Response, app_of_handoff,
    read_frame, write_frame,
};

use crate::custodian::Custodian;

/// Answer one request from `app_id`.
#[must_use]
pub fn answer(custodian: &Custodian, app_id: &str, request: &Request) -> Response {
    if !request.permitted_for(app_id) {
        return Response::refused("this app may not ask the accounts service for that");
    }
    let result = match request {
        Request::FindForgeIssue { account, marker } => custodian
            .forge_account(account)
            .and_then(|account| custodian.find_forge_issue(&account, marker)),
        Request::FileForgeIssue { account, issue } => custodian
            .forge_account(account)
            .and_then(|account| custodian.file_forge_issue(&account, issue))
            .map(Some),
    };
    match result {
        Ok(issue) => Response::issue(issue),
        Err(error) => Response::refused(error.to_string()),
    }
}

/// Serve one connection until it closes.
///
/// # Errors
///
/// When the handoff is missing or malformed, or the connection fails.
pub fn serve_client(custodian: &Custodian, mut stream: UnixStream) -> Result<(), ClientError> {
    let handoff = HANDOFF_FRAMING
        .read(&mut stream)
        .map_err(|_| ClientError::Protocol("unreadable launcher handoff".into()))?
        .ok_or_else(|| ClientError::Protocol("missing launcher handoff".into()))?;
    let app_id = app_of_handoff(&handoff)?;
    while let Some(frame) = read_frame::<Frame<Request>>(&mut stream)? {
        let response = if frame.version == RPC_VERSION {
            answer(custodian, &app_id, &frame.value)
        } else {
            Response::refused("unsupported accounts service version")
        };
        write_frame(&mut stream, &response)?;
    }
    Ok(())
}

/// Serve every connection on `listener`, each on its own thread.
///
/// # Errors
///
/// When accepting fails.
pub fn serve(custodian: &Custodian, listener: &UnixListener) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept()?;
        let custodian = custodian.clone();
        std::thread::spawn(move || {
            if let Err(error) = serve_client(&custodian, stream) {
                tracing::debug!(%error, "accounts service: connection ended");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::fake::{FakeForge, connect};
    use tilo_account_service::{Client, ForgeIssue, handoff};

    #[test]
    fn an_app_files_through_the_service_and_only_the_agent_may() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tilo");
        let forge = FakeForge::start("t");
        connect(&root, "forge", &forge);
        let custodian = Custodian::for_root(Some(root));
        let open = |app: &str| {
            let (mut app_end, service_end) = UnixStream::pair().unwrap();
            HANDOFF_FRAMING
                .write(&mut app_end, &handoff(app).unwrap())
                .unwrap();
            let custodian = custodian.clone();
            std::thread::spawn(move || serve_client(&custodian, service_end));
            Client::from_stream(app_end)
        };
        let issue = ForgeIssue {
            title: "Send does nothing".into(),
            body: "b".into(),
            marker: "m_draft".into(),
        };
        let mut agent = open("so.tilo.agent");
        let filed = agent.file_forge_issue("forge", &issue).unwrap();
        assert_eq!(
            agent.find_forge_issue("forge", "m_draft").unwrap(),
            Some(filed)
        );
        let mut notes = open("so.tilo.notes");
        assert!(matches!(
            notes.file_forge_issue("forge", &issue),
            Err(ClientError::Refused(_))
        ));
        assert_eq!(forge.issues().len(), 1);
    }
}
