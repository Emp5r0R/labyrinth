//! Streaming architecture for reverse port forwarding
//!
//! This module provides the core traits, data models, and error types
//! for the new streaming-based reverse port forwarding system.

pub mod connection_manager;
pub mod errors;
pub mod metrics;
pub mod models;
pub mod recovery;
#[cfg(test)]
pub mod resource_manager;
pub mod stream_manager;
pub mod traits;

#[cfg(test)]
pub mod test_interfaces;

pub use errors::*;
pub use metrics::*;
pub use models::*;
pub use recovery::*;
pub use traits::*;

// Removed unused exports: StreamResourceManager, ResourceConfig
