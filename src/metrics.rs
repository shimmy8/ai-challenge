#![allow(unused_imports)]
use crate::{agent::MetricsLogEntry, config::*};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    io::Write,
    net::SocketAddr,
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
        StartupMode::McpServer { .. } | StartupMode::Index(_) | StartupMode::RagEval(_) => {
            bail!("этот startup-режим нельзя использовать как флаг метрик")
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StartupMode {
    Interactive {
        dump_metrics: bool,
    },
    McpServer {
        kind: crate::mcp::McpServerKind,
        addr: SocketAddr,
    },
    Index(crate::rag::IndexOptions),
    RagEval(crate::rag::RagEvalOptions),
}

pub(crate) fn parse_startup_mode(args: impl IntoIterator<Item = String>) -> Result<StartupMode> {
    let args = args.into_iter().collect::<Vec<_>>();
    if args.first().is_some_and(|value| value == "--mcp-server") {
        anyhow::ensure!(
            args.len() == 4 && args[2] == "--addr",
            "использование: --mcp-server <kind> --addr <loopback:port>"
        );
        return Ok(StartupMode::McpServer {
            kind: args[1].parse()?,
            addr: crate::mcp::validate_mcp_bind_addr(&args[3])?,
        });
    }
    if args.first().is_some_and(|value| value == "index") {
        return Ok(StartupMode::Index(crate::rag::parse_index_options(
            &args[1..],
        )?));
    }
    if args.first().is_some_and(|value| value == "rag-eval") {
        return Ok(StartupMode::RagEval(crate::rag::parse_rag_eval_options(
            &args[1..],
        )?));
    }
    let mut dump_metrics = false;
    for argument in args {
        match argument.as_str() {
            "--dump-metrics" if !dump_metrics => dump_metrics = true,
            "--dump-metrics" => bail!("аргумент указан более одного раза: {argument}"),
            _ => bail!("неизвестный аргумент: {argument}. Доступны index, rag-eval, --dump-metrics и --mcp-server <kind> --addr <loopback:port>"),
        }
    }
    Ok(StartupMode::Interactive { dump_metrics })
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
