// One test binary instead of one per file: each tests/*.rs linked the whole app
// separately. #[path] keeps the files where they are, and nextest runs each
// test in its own process, so the isolation separate binaries gave them stays.
// The files that drive the live engine keep their own `live` check.

#[path = "account_engine_loop.rs"]
mod account_engine_loop;
#[path = "google_settings.rs"]
mod google_settings;
#[path = "host_mail_session.rs"]
mod host_mail_session;
#[path = "oauth_credentials.rs"]
mod oauth_credentials;
#[path = "recipient_headers.rs"]
mod recipient_headers;
