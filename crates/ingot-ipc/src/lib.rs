//! IPC message types and service definitions for iceoryx2 shared-memory
//! transport.
//!
//! All public types in this crate are `#[repr(C)]`, `Copy`, and implement
//! `ZeroCopySend` for zero-copy inter-process communication.

pub mod convert;
pub mod services;
pub mod types;

pub use services::*;
pub use types::*;
