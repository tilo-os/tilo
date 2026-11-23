//! Plain data declarations for enrolled and current device transport.
//!
//! These types hold only public comparison facts and byte counts, with no
//! filesystem handles or secret material, so every platform shares one
//! declaration.

use serde::{Deserialize, Serialize};

/// Non-secret proof identifying the one device carried by an authenticated artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportReceipt {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub sealed_bytes: u64,
}

/// Public comparison facts; deserializing these never admits an identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentDeviceDeclaration {
    pub(super) schema: u32,
    pub(super) user_public_key: [u8; 32],
    pub(super) device_public_key: [u8; 32],
    pub(super) record_name: String,
    pub(super) record_sha256: [u8; 32],
    pub(super) certificate_sha256: [u8; 32],
    pub(super) fleet_sha256: [u8; 32],
}

impl CurrentDeviceDeclaration {
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.user_public_key
    }
    #[must_use]
    pub fn device_public_key(&self) -> [u8; 32] {
        self.device_public_key
    }
    #[must_use]
    pub fn record_name(&self) -> &str {
        &self.record_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_device_declaration_matches_literal_bytes_and_refuses_unknown_fields() {
        let declaration = CurrentDeviceDeclaration {
            schema: 1,
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            record_name: "primary.json".into(),
            record_sha256: [3; 32],
            certificate_sha256: [4; 32],
            fleet_sha256: [5; 32],
        };
        let literal = concat!(
            r#"{"schema":1,"#,
            r#""user_public_key":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"#,
            r#""device_public_key":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"#,
            r#""record_name":"primary.json","#,
            r#""record_sha256":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3],"#,
            r#""certificate_sha256":[4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4,4],"#,
            r#""fleet_sha256":[5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5]}"#
        );
        let bytes = serde_json::to_vec(&declaration).expect("serialize declaration");
        assert_eq!(bytes, literal.as_bytes());
        let parsed: CurrentDeviceDeclaration =
            serde_json::from_slice(literal.as_bytes()).expect("deserialize declaration");
        assert_eq!(parsed, declaration);
        assert_eq!(parsed.public_key(), [1; 32]);
        assert_eq!(parsed.device_public_key(), [2; 32]);
        assert_eq!(parsed.record_name(), "primary.json");

        let mut extra: serde_json::Value =
            serde_json::from_slice(literal.as_bytes()).expect("declaration json value");
        extra["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<CurrentDeviceDeclaration>(extra).is_err());
    }
}
