//! Compile and exercise the exact vendored production TLS transport source in
//! the projector's normal CI test target; no second transport implementation.
//! The generic cancellation future is driven by isolated test channels.
#[path = "../../../../vendor/fcast-sender-sdk/src/chromecast_tls.rs"]
mod transport;
