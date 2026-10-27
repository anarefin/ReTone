//! Provider plumbing shared by the connection check and the main-process
//! Anthropic proxy: the connection check and its cache, keyed errors, Anthropic
//! request/response handling and LLM deadlines. HTTP itself sits behind
//! [`connection_test::ProbeTransport`], so everything here runs headless.

pub mod anthropic;
pub mod connection_cache;
pub mod connection_test;
pub mod errors;
pub mod timeouts;
pub mod urls;
