//! rabb1tClaw - Minimal Rust LLM gateway for Rabbit R1 and other devices.

// Binary crate — all pub(crate) is inherently redundant (no external consumers)
#![allow(clippy::redundant_pub_crate)]

mod agent;
mod cli;
mod config;
mod connection;
mod provider;
mod protocol;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
