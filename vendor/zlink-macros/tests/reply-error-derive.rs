use zlink_macros::ReplyError;

#[derive(ReplyError, Debug, PartialEq)]
#[zlink(interface = "com.example.Test")]
enum TestError<'a> {
    NotFound,
    PermissionDenied,
    InvalidInput {
        field: &'a str,
        reason: &'a str,
    },
    Timeout {
        seconds: u32,
    },
    // Test variant with renamed fields
    RenamedFields {
        #[zlink(rename = "actualName")]
        _internal_name: &'a str,
        #[zlink(rename = "errorCode")]
        _code: i32,
        #[zlink(rename = "optionalData")]
        _optional: Option<&'a str>,
    },
    // `borrow` before `rename` on one attribute must not drop the rename.
    BorrowThenRename {
        #[zlink(borrow, rename = "borrowedMessage")]
        _message: std::borrow::Cow<'a, str>,
    },
    // `rename` before `borrow` on one attribute must not drop the borrow.
    RenameThenBorrow {
        #[zlink(rename = "renamedMessage", borrow)]
        _message: std::borrow::Cow<'a, str>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_variant_serialization() {
        let error = TestError::NotFound;
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(json, r#"{"error":"com.example.Test.NotFound"}"#);
    }

    #[test]
    fn named_variant_serialization() {
        let error = TestError::InvalidInput {
            field: "username",
            reason: "too short",
        };

        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains(r#""error":"com.example.Test.InvalidInput""#));
        assert!(json.contains(r#""field":"username""#));
        assert!(json.contains(r#""reason":"too short""#));
    }

    #[test]
    fn round_trip() {
        // Test with error that has parameters
        let original = TestError::InvalidInput {
            field: "password",
            reason: "missing special character",
        };

        round_trip_serialize(&original);

        // Test with error that has no parameters
        let original = TestError::NotFound;
        round_trip_serialize(&original);
    }

    #[test]
    fn renamed_fields_serialization() {
        // Test that renamed fields are serialized with their renamed names
        let error = TestError::RenamedFields {
            _internal_name: "test_value",
            _code: 42,
            _optional: Some("optional_value"),
        };

        let json = serde_json::to_string(&error).unwrap();

        // Check that the JSON contains the renamed field names, not the original ones
        assert!(json.contains(r#""actualName":"test_value""#));
        assert!(json.contains(r#""errorCode":42"#));
        assert!(json.contains(r#""optionalData":"optional_value""#));

        // Ensure original field names are NOT in the JSON
        assert!(!json.contains("_internal_name"));
        assert!(!json.contains("_code"));
        assert!(!json.contains("_optional"));
    }

    #[test]
    fn renamed_fields_deserialization() {
        // Test deserialization with renamed fields
        let json = r#"{"error":"com.example.Test.RenamedFields","parameters":{"actualName":"test_value","errorCode":42,"optionalData":"optional_value"}}"#;

        let deserialized: TestError = serde_json::from_str(json).unwrap();
        assert_eq!(
            deserialized,
            TestError::RenamedFields {
                _internal_name: "test_value",
                _code: 42,
                _optional: Some("optional_value"),
            }
        );

        // Test with optional field missing (should deserialize as None)
        let json_no_optional = r#"{"error":"com.example.Test.RenamedFields","parameters":{"actualName":"test_value","errorCode":42}}"#;

        let deserialized: TestError = serde_json::from_str(json_no_optional).unwrap();
        assert_eq!(
            deserialized,
            TestError::RenamedFields {
                _internal_name: "test_value",
                _code: 42,
                _optional: None,
            }
        );

        // Test that using original field names fails
        let json_with_original_names = r#"{"error":"com.example.Test.RenamedFields","parameters":{"_internal_name":"test_value","_code":42}}"#;
        let result: Result<TestError, _> = serde_json::from_str(json_with_original_names);
        assert!(result.is_err());
    }

    #[test]
    fn borrow_before_rename_is_honored() {
        let error = TestError::BorrowThenRename {
            _message: std::borrow::Cow::Borrowed("hi"),
        };
        let json = serde_json::to_string(&error).unwrap();

        assert!(json.contains("borrowedMessage"), "rename dropped: {json}");
        assert!(!json.contains("_message"), "field not renamed: {json}");
    }

    #[test]
    fn rename_before_borrow_is_honored() {
        // No escapes in the value, so a zero-copy borrow is possible.
        let json =
            r#"{"error":"com.example.Test.RenameThenBorrow","parameters":{"renamedMessage":"hi"}}"#;

        let TestError::RenameThenBorrow { _message } = serde_json::from_str(json).unwrap() else {
            panic!("wrong variant deserialized");
        };

        assert!(
            matches!(_message, std::borrow::Cow::Borrowed(_)),
            "borrow dropped: field was owned instead of borrowed",
        );
    }

    #[test]
    fn renamed_fields_round_trip() {
        // Test round-trip with all fields present
        let original = TestError::RenamedFields {
            _internal_name: "round_trip_test",
            _code: 999,
            _optional: Some("with_optional"),
        };
        round_trip_serialize(&original);

        // Test round-trip with optional field as None
        let original_no_optional = TestError::RenamedFields {
            _internal_name: "no_optional",
            _code: 123,
            _optional: None,
        };
        round_trip_serialize(&original_no_optional);
    }

    #[test]
    fn field_order_agnostic_with_lifetimes() {
        // With alloc available, we no longer require a specific field order

        // Parameters-first JSON should now work
        let json_parameters_first = r#"{"parameters":{"field":"test","reason":"fail"},"error":"com.example.Test.InvalidInput"}"#;
        let result: Result<TestError, _> = serde_json::from_str(json_parameters_first);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            TestError::InvalidInput {
                field: "test",
                reason: "fail"
            }
        );

        // Error-first JSON also works
        let json_error_first = r#"{"error":"com.example.Test.InvalidInput","parameters":{"field":"test","reason":"fail"}}"#;
        let result: Result<TestError, _> = serde_json::from_str(json_error_first);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            TestError::InvalidInput {
                field: "test",
                reason: "fail"
            }
        );
    }

    #[derive(ReplyError, Debug, PartialEq)]
    #[zlink(interface = "com.example.Owned")]
    enum OwnedError {
        NotFound,
        InvalidInput { field: String, reason: String },
    }

    #[test]
    fn field_order_agnostic_without_lifetimes() {
        // With alloc available, we no longer require a specific field order

        // Parameters-first JSON should now work
        let json_parameters_first = r#"{"parameters":{"field":"test","reason":"fail"},"error":"com.example.Owned.InvalidInput"}"#;
        let result: Result<OwnedError, _> = serde_json::from_str(json_parameters_first);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            OwnedError::InvalidInput {
                field: "test".to_string(),
                reason: "fail".to_string()
            }
        );

        // Error-first JSON also works
        let json_error_first = r#"{"error":"com.example.Owned.InvalidInput","parameters":{"field":"test","reason":"fail"}}"#;
        let result: Result<OwnedError, _> = serde_json::from_str(json_error_first);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            OwnedError::InvalidInput {
                field: "test".to_string(),
                reason: "fail".to_string()
            }
        );
    }

    // Helper function for round-trip serialization test, abstracting std vs nostd differences
    fn round_trip_serialize(original: &TestError) {
        let json = serde_json::to_string(original).unwrap();
        let deserialized: TestError = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, deserialized);
    }

    mod zlink_borrow {
        use std::borrow::Cow;

        use zlink_macros::ReplyError;

        #[derive(ReplyError, Debug, PartialEq)]
        #[zlink(interface = "org.example.Calculator")]
        enum CalculatorError<'a> {
            DivisionByZero {
                #[zlink(borrow)]
                message: Cow<'a, str>,
            },
        }

        #[test]
        fn borrow_serialization() {
            let error = CalculatorError::DivisionByZero {
                message: Cow::Borrowed("Cannot divide by zero"),
            };
            let json = serde_json::to_string(&error).unwrap();
            assert!(json.contains(r#""error":"org.example.Calculator.DivisionByZero""#));
            assert!(json.contains(r#""message":"Cannot divide by zero""#));
        }

        #[test]
        fn borrow_deserialization() {
            let json = r#"{"error":"org.example.Calculator.DivisionByZero","parameters":{"message":"Cannot divide by zero"}}"#;
            let error: CalculatorError = serde_json::from_str(json).unwrap();
            assert_eq!(
                error,
                CalculatorError::DivisionByZero {
                    message: Cow::Borrowed("Cannot divide by zero"),
                }
            );
        }

        #[test]
        fn borrow_round_trip() {
            let original = CalculatorError::DivisionByZero {
                message: Cow::Borrowed("Cannot divide by zero"),
            };
            let json = serde_json::to_string(&original).unwrap();
            let deserialized: CalculatorError = serde_json::from_str(&json).unwrap();
            assert_eq!(original, deserialized);
        }
    }
}
