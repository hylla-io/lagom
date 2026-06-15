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
//!
//! ## The one-call helper
//!
//! [`Guard`] pairs the upstream surface with a frozen policy so an integrator
//! wires a slim, branded MCP in two calls — [`Guard::slim_defs`] for the
//! downstream `tools/list`, [`Guard::gate`] for every `tools/call` — without
//! touching the project/rewrite plumbing. The Python, Node, and Go bindings
//! mirror this shape exactly.
//!
//! ## Ephemeral mint / refire
//!
//! [`mint`] resolves an in-code base + dynamic-overlay narrowing into a
//! [`MintRecord`] (resolved policy + provenance); [`refire`] re-mints the exact
//! projection from a persisted record (`SPEC.md` §8.2). Both are pure, so they
//! are shared by every face, including the wasm/Go binding.

mod guard;
mod merge;
mod mint;
mod project;
mod rewrite;
mod validate;

pub mod policy;
pub mod tooldef;

pub use guard::Guard;
pub use merge::{MergeError, merge};
pub use mint::{MintRecord, PolicySources, ResolvedPolicy, UpstreamCommand, mint, refire};
pub use policy::{ArgPolicy, Constraint, DescriptionPolicy, Policy, Presence, ToolPolicy};
pub use project::project;
pub use rewrite::{Reject, rewrite};
pub use tooldef::{ToolCall, ToolDef};
pub use validate::{DriftError, validate};
