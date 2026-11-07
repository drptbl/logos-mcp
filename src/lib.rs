//! logos-mcp: MCP tools over a running Logos node.
//!
//! The tools in [`server`] talk to the node through the [`node::Node`] interface.
//! The standalone binary drives it with the `logosctl` adapter in [`logosctl`].

pub mod control;
pub mod events;
pub mod guard;
pub mod logosctl;
pub mod node;
pub mod server;

#[cfg(feature = "module")]
pub mod module;
