//! Streaming architecture for reverse port forwarding
//! 
//! This module provides the core traits, data models, and error types
//! for the new streaming-based reverse port forwarding system.

pub mod traits;
pub mod models;
pub mod errors;
pub mod metrics;
pub mod recovery;
pub mod connection_manager;
pub mod stream_manager;
#[cfg(test)]
pub mod resource_manager;

#[cfg(test)]
pub mod test_interfaces;

pub use traits::*;
pub use models::*;
pub use errors::*;
pub use metrics::*;
pub use recovery::*;

// Removed unused exports: StreamResourceManager, ResourceConfig
