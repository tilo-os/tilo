#![cfg(feature = "proxy")]

//! Test that proxy macro works with both old and new attribute formats

use zlink::proxy;

// Test old format (backward compatibility)
#[proxy("org.example.old")]
pub trait OldFormat {
    async fn test(&mut self) -> zlink::Result<Result<(), TestError>>;
}

// Test new format with interface only
#[proxy(interface = "org.example.new")]
pub trait NewFormat {
    async fn test(&mut self) -> zlink::Result<Result<(), TestError>>;
}

// Test new format with interface and crate
#[proxy(interface = "org.example.full", crate = "::zlink")]
pub trait FullFormat {
    async fn test(&mut self) -> ::zlink::Result<Result<(), TestError>>;
}

#[derive(Debug, Clone, PartialEq, zlink::ReplyError)]
#[zlink(interface = "org.example.test")]
pub enum TestError {}

#[test]
fn test_compilation() {
    // This test just verifies that the code compiles
    // which means the proxy macro correctly handles all formats
}
