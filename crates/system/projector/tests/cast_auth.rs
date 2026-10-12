//! The exact production `DeviceAuth` gate, including its adversarial tests.
use getrandom as auth_random;
use google_cast_protocol as protocol;
use rand_core as auth_rng;
use rcgen as certificates;
use rsa as rsa_crypto;
use rustls_pki_types as pki;
use sha2 as auth_crypto;
use tokio_rustls as tls_runtime;
use webpki as cert_path;
use x509_parser as cert_parser;
#[path = "../../../../vendor/fcast-sender-sdk/src/chromecast_auth.rs"]
mod auth;
#[path = "../../../../vendor/fcast-sender-sdk/src/chromecast_tls.rs"]
mod tls;

use prost as wire_proto;
use time as test_clock;
