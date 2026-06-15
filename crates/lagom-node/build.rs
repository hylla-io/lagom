//! napi-rs build hook: sets up the N-API symbol exports the Node addon needs.
//! Mirrors the standard napi-rs crate layout (see the napi-rs docs); without it
//! the `.node` cdylib would not expose the N-API registration entry points.

fn main() {
    napi_build::setup();
}
