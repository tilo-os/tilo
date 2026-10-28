use super::*;
use std::os::unix::fs::PermissionsExt;

struct CaptureBrowser(mpsc::SyncSender<String>);
impl BrowserOpener for CaptureBrowser {
    fn open(&self, url: &str) -> Result<(), browser::BrowserError> {
        self.0.try_send(url.to_string()).unwrap();
        Ok(())
    }
    fn close(&self) -> Result<(), browser::BrowserError> {
        Ok(())
    }
}

fn consent(provider: Provider, calendar: bool) -> Vec<String> {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let (sender, receiver) = mpsc::sync_channel(1);
    let issuer = if provider == Provider::Microsoft {
        OAuthIssuer::Microsoft
    } else {
        OAuthIssuer::Google
    };
    let mut controller = AccountsController::fixture(
        directory.path(),
        Arc::new(CaptureBrowser(sender)),
        |custody| custody.with_issuer_endpoints(issuer, server.provider()),
    )
    .unwrap();
    controller
        .command(AccountsCommand::Add(AccountRequest {
            provider,
            email: String::new(),
            server: String::new(),
            files: false,
            mail: true,
            calendar,
        }))
        .unwrap();
    let url = url::Url::parse(&tilo_test_env::recv("the authorization URL", &receiver)).unwrap();
    let scopes = url
        .query_pairs()
        .find(|(key, _)| key == "scope")
        .unwrap()
        .1
        .split_whitespace()
        .map(str::to_string)
        .collect();
    controller.command(AccountsCommand::Cancel).unwrap();
    assert!(controller.selected().is_empty());
    scopes
}

#[test]
fn google_mail_and_calendar_consent_never_requests_drive() {
    let scopes = consent(Provider::Google, true);
    assert!(
        scopes
            .iter()
            .any(|scope| scope == "https://mail.google.com/")
    );
    assert!(
        scopes
            .iter()
            .any(|scope| scope == "https://www.googleapis.com/auth/calendar")
    );
    assert!(!scopes.iter().any(|scope| scope.contains("drive")));
}

#[test]
fn microsoft_mail_starts_with_graph_identity_not_exchange_or_files() {
    let scopes = consent(Provider::Microsoft, false);
    assert!(scopes.iter().any(|scope| scope == "User.Read"));
    assert!(
        !scopes
            .iter()
            .any(|scope| scope.contains("outlook") || scope.contains("Files"))
    );
}
