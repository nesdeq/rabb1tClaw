//! Agent modules: dispatch, runner, session, streaming helpers.

pub mod advanced;
pub mod code;
pub mod dispatch;
pub mod events;
pub mod markers;
pub mod memory;
pub mod runner;
pub mod search;
pub mod session;
pub mod stream;
pub mod tasklog;
pub mod tracker;

pub use dispatch::dispatch_method;
