//! Test that ReplyError derive macro works with empty enums.

use serde::{Deserialize, Serialize};
use zlink_macros::ReplyError;

#[derive(Debug, Clone, PartialEq, ReplyError)]
#[zlink(interface = "org.example.Empty")]
pub enum EmptyError {}

#[test]
fn empty_enum_compiles() {
    // This test just verifies that the derive macro compiles with an empty enum
    // The enum can never be instantiated, so we can't test serialization

    // Verify that the type exists and has the expected traits
    fn assert_traits<
        T: std::fmt::Debug + Clone + PartialEq + Serialize + for<'de> Deserialize<'de>,
    >() {
    }
    assert_traits::<EmptyError>();
}
