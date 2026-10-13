#![allow(unused_imports)]
mod agent;
mod app;
mod cli;
mod config;
mod mcp;
mod memory;
mod metrics;
mod model;
mod providers;
mod rag;
mod sessions;
mod summarization;
mod tests;

pub(crate) use agent::*;
pub(crate) use cli::*;
pub(crate) use config::*;
pub(crate) use mcp::*;
pub(crate) use memory::*;
pub(crate) use metrics::*;
pub(crate) use model::*;
pub(crate) use providers::*;
pub(crate) use rag::*;
pub(crate) use sessions::*;
pub(crate) use summarization::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
