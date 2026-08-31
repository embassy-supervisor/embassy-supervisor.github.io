//! In-browser playground for embassy-supervisor.
//!
//! The real supervisor runtime (crates.io `embassy-supervisor`) runs on
//! embassy-executor's wasm platform with embassy-time's mock driver, so the
//! page owns virtual time (play / pause / speed / step). The DSL is parsed
//! by the real `embassy-supervisor-syntax` crate, and the graph it describes
//! is rebuilt at runtime from the supervisor's public constructor surface
//! ([`build`]). JS talks to it through the `#[wasm_bindgen]` surface in
//! [`api`]; the native test harness drives [`build`] + [`behavior`] directly
//! on embassy-executor's std platform.

#[cfg(target_arch = "wasm32")]
mod api;
pub mod behavior;
pub mod build;
pub mod events;
pub mod health;
pub mod model;
pub mod parse;
pub mod registry;
pub mod trace_hooks;

#[cfg(target_arch = "wasm32")]
pub use api::*;
