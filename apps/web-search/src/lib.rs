//! `tilo-web-search`: keyless web search and page reading for agents and
//! shortcuts, as a headless service.
//!
//! The service holds the network egress so the agent does not have to: the
//! agent runs without a network and acts only by invoking published actions,
//! and this service publishes two of them, `search` and `fetch`
//! ([`actions`]). Searching asks several public engines at once and merges
//! their results; fetching reduces one page to readable text. Neither needs
//! an account, an API key or a hosted service. The engines and the reader
//! live in the `tilo-web-search` crate.

pub mod actions;
pub mod service;
