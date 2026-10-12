//! Compile and exercise the exact vendored production TLS transport source in
//! the projector's normal CI test target; no second transport implementation.
//! The generic cancellation future is driven by isolated test channels.
use rcgen as certificates;
use rustls_pki_types as pki;
use tokio_rustls as tls_runtime;

#[path = "../../../../vendor/fcast-sender-sdk/src/chromecast_tls.rs"]
mod transport;
