//! Provider delivery primitives and streaming translators.
//!
//! This crate is deliberately split between deterministic delivery policy and
//! network adapters.  The policy half is pure: callers can exercise retries,
//! circuit breaking, failover, and context prechecks without making a provider
//! request.

#![forbid(unsafe_code)]

pub mod anthropic;
pub mod anthropic_native;
pub mod openai_compat;
pub mod reliability;
pub mod sse;
