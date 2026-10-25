//! Human-readable, per-process manual validation report. Never used by downloads.
use engine_workspace::storage;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static SESSION_FILE: OnceLock<PathBuf> = OnceLock::new();

fn stamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    let (year, month, day) = now.to_calendar_date();
    format!(
        "{year:04}-{:02}-{day:02}_{:02}-{:02}-{:02}",
        u8::from(month),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn log_dir() -> PathBuf {
    storage::data_dir().join("ValidationLogs")
}

pub(super) fn current() -> Option<PathBuf> {
    SESSION_FILE.get().cloned()
}

pub(super) fn directory() -> PathBuf {
    log_dir()
}

pub(super) fn append(
    library: &str,
    root: &Path,
    asset: &Path,
    method: &str,
    outcome: &str,
    detail: &str,
) -> Result<PathBuf, String> {
    let directory = log_dir();
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let log = SESSION_FILE
        .get_or_init(|| {
            directory.join(format!("Validation_{}_{}.log", stamp(), std::process::id()))
        })
        .clone();
    let new_log = !log.exists();
    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    if new_log {
        writeln!(out, "DoNothingAndWin - Manual Library Validation Log\nSession: {} (UTC)\n==============================================================================", stamp())
            .map_err(|e| e.to_string())?;
    }
    writeln!(out, "\n[{} UTC] {outcome}\nLibrary: {library}\nCache root: {}\nAsset path: {}\nMethod: {method}\nDetails: {detail}\n------------------------------------------------------------------------------",
        stamp().replace('_', " "), root.display(), asset.display())
        .map_err(|e| e.to_string())?;
    out.sync_all().map_err(|e| e.to_string())?;
    // One file per application session, at most ten retained. Never remove active log.
    let mut logs = fs::read_dir(&directory)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.starts_with("Validation_") && name.ends_with(".log"))
        })
        .collect::<Vec<_>>();
    logs.sort();
    while logs.len() > 10 {
        let oldest = logs.remove(0);
        if oldest != log {
            let _ = fs::remove_file(oldest);
        }
    }
    Ok(log)
}
