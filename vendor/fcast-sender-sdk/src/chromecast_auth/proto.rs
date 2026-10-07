use super::super::wire_proto as prost;
// DeviceAuth and revocation fields from Google Open Screen's pinned schemas.
// Copyright 2019 The Chromium Authors. BSD-3-Clause: LICENSE.OpenScreen.
// Original schemas are retained beside this file; no protoc build dependency.
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct DeviceAuth {
    #[prost(message, optional, tag = "1")]
    pub challenge: Option<Challenge>,
    #[prost(message, optional, tag = "2")]
    pub response: Option<Response>,
    #[prost(message, optional, tag = "3")]
    pub error: Option<AuthError>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Challenge {
    #[prost(int32, optional, tag = "1")]
    pub signature_algorithm: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub sender_nonce: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "3")]
    pub hash_algorithm: Option<i32>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Response {
    #[prost(bytes = "vec", required, tag = "1")]
    pub signature: Vec<u8>,
    #[prost(bytes = "vec", required, tag = "2")]
    pub client_auth_certificate: Vec<u8>,
    #[prost(bytes = "vec", repeated, tag = "3")]
    pub intermediate_certificate: Vec<Vec<u8>>,
    #[prost(int32, optional, tag = "4")]
    pub signature_algorithm: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub sender_nonce: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "6")]
    pub hash_algorithm: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub crl: Option<Vec<u8>>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct AuthError {
    #[prost(int32, required, tag = "1")]
    pub error_type: i32,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct CrlBundle {
    #[prost(message, repeated, tag = "1")]
    pub crls: Vec<Crl>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct Crl {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub tbs: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signer_cert: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub signature: Option<Vec<u8>>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct TbsCrl {
    #[prost(uint64, optional, tag = "1")]
    pub version: Option<u64>,
    #[prost(uint64, optional, tag = "2")]
    pub not_before_seconds: Option<u64>,
    #[prost(uint64, optional, tag = "3")]
    pub not_after_seconds: Option<u64>,
    #[prost(bytes = "vec", repeated, tag = "4")]
    pub revoked_public_key_hashes: Vec<Vec<u8>>,
    #[prost(message, repeated, tag = "5")]
    pub revoked_serial_number_ranges: Vec<SerialRange>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub(super) struct SerialRange {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub issuer_public_key_hash: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "2")]
    pub first_serial_number: Option<u64>,
    #[prost(uint64, optional, tag = "3")]
    pub last_serial_number: Option<u64>,
}
