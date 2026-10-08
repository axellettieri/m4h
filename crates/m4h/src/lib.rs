//! # M4H (Multi4Hyper)
//!
//! *Ad astra per aspera.*
//!
//! M4H is a multikernel written in Rust for hyper-scale message workloads:
//! tens of millions of WebSocket connections on a single server, one
//! lightweight BEAM process per connection.
//!
//! This umbrella crate will re-export the stable public surface of the
//! project's components as they mature. Today the components live in their
//! own crates (`m4h-ring`, `m4h-platform`, ...); see the
//! [repository](https://github.com/axellettieri/m4h) for the architecture
//! and roadmap.
#![no_std]

/// Version of the M4H project this crate belongs to.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
