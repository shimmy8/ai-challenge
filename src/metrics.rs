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

pub(crate) fn parse_dump_metrics_flag(args: impl IntoIterator<Item = String>) -> Result<bool> {
    let mut dump_metrics = false;
    for argument in args {
        match argument.as_str() {
            "--dump-metrics" => dump_metrics = true,
            _ => bail!("неизвестный аргумент: {argument}. Доступен флаг --dump-metrics"),
        }
    }
    Ok(dump_metrics)
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
