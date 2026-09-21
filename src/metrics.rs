#![allow(unused_imports)]
use crate::{agent::MetricsLogEntry, config::*};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) fn metrics_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("не удалось определить текущую директорию")?
        .join(METRICS_LOG_FILE))
}

#[allow(dead_code)]
pub(crate) fn parse_dump_metrics_flag(args: impl IntoIterator<Item = String>) -> Result<bool> {
    match parse_startup_mode(args)? {
        StartupMode::Interactive { dump_metrics } => Ok(dump_metrics),
        StartupMode::McpServer => bail!("режим MCP-сервера нельзя использовать как флаг метрик"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupMode {
    Interactive { dump_metrics: bool },
    McpServer,
}

pub(crate) fn parse_startup_mode(args: impl IntoIterator<Item = String>) -> Result<StartupMode> {
    let mut dump_metrics = false;
    let mut mcp_server = false;
    for argument in args {
        match argument.as_str() {
            "--dump-metrics" if !dump_metrics => dump_metrics = true,
            "--mcp-server" if !mcp_server => mcp_server = true,
            "--dump-metrics" | "--mcp-server" => {
                bail!("аргумент указан более одного раза: {argument}")
            }
            _ => bail!("неизвестный аргумент: {argument}. Доступны --dump-metrics и --mcp-server"),
        }
    }
    anyhow::ensure!(
        !(dump_metrics && mcp_server),
        "--dump-metrics и --mcp-server нельзя использовать вместе"
    );
    Ok(if mcp_server {
        StartupMode::McpServer
    } else {
        StartupMode::Interactive { dump_metrics }
    })
}

pub(crate) fn append_metrics_log(path: &Path, entry: &MetricsLogEntry<'_>) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("не удалось открыть {}", path.display()))?;
    serde_json::to_writer(&mut file, entry)?;
    writeln!(file)?;
    Ok(())
}
