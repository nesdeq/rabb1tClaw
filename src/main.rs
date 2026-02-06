//! rabb1tClaw - Minimal Rust LLM gateway for Rabbit R1 and other devices.

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
