//! # lagom-core
//!
//! The transport-less transform + policy engine at the heart of lagom: it turns
//! a full upstream MCP tool surface into a narrowed [`Policy`]-defined
//! projection. No I/O, no JSON-RPC, no process spawning — pure functions over
//! data, so this crate compiles to native *and* wasm and is the foundation every
//! face (CLI, bindings) depends on.
//!
//! See `SPEC.md` for the full design and `CONTEXT.md` for the glossary.
//!
//! ## The four operations
//!
//! - [`project`] — forward transform: upstream surface → narrowed surface.
//! - [`rewrite`] — inverse transform: projected call → upstream call, or
//!   [`Reject`].
//! - [`merge`] — narrow-only composition of an end-user overlay onto an
//!   integrator base ([`MergeError`] on any widening).
//! - [`validate`] — drift check of a policy against the live upstream
//!   ([`DriftError`] per stale reference).

mod merge;
mod project;
mod rewrite;
mod validate;

pub mod policy;
pub mod tooldef;

pub use merge::{MergeError, merge};
pub use policy::{ArgPolicy, Constraint, DescriptionPolicy, Policy, Presence, ToolPolicy};
pub use project::project;
pub use rewrite::{Reject, rewrite};
pub use tooldef::{ToolCall, ToolDef};
pub use validate::{DriftError, validate};
