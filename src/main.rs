#![allow(unused_imports)]
mod agent;
mod app;
mod cli;
mod config;
mod memory;
mod metrics;
mod model;
mod providers;
mod sessions;
mod tests;

pub(crate) use agent::*;
pub(crate) use cli::*;
pub(crate) use config::*;
pub(crate) use memory::*;
pub(crate) use metrics::*;
pub(crate) use model::*;
pub(crate) use providers::*;
pub(crate) use sessions::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
