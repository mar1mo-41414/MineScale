//! Log file: every log line is appended to a file on this computer the
//! moment it happens (one unbuffered write per event), so the record
//! survives a crash, a closed window, or a GUI that scrolled the line away.
//!
//! The file stays on the user's computer; nothing here sends it anywhere.
//!
//! Where (per user, created on first run):
//!   - macOS:   `~/Library/Logs/MineScale/<app>.log`
//!   - Windows: `%LOCALAPPDATA%\MineScale\logs\<app>.log`
//!   - Linux:   `$XDG_STATE_HOME/minescale/<app>.log` (default `~/.local/state/minescale/`)
//!
//! Lines are `<UTC time> <LEVEL> <target>: <message> key=value…`. Each run
//! adds a `=== … started ===` header. A file over 5 MiB is moved to
//! `<app>.log.1` (replacing the previous one) when the next run starts.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{
    field::{Field, Visit},
    Event, Level, Subscriber,
};
use tracing_subscriber::{layer::Context, Layer};

const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Directory the log files live in, if the platform gives us one.
pub fn log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Logs/MineScale"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(base).join("MineScale").join("logs"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
            return Some(PathBuf::from(state).join("minescale"));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".local/state/minescale"))
    }
}

pub struct FileLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl FileLog {
    /// Open `<log_dir>/<app>.log` for appending. Returns `None` (and the app
    /// carries on without a file) if the directory or file can't be used.
    pub fn open(app: &str, version: &str) -> Option<Arc<Self>> {
        Self::open_in(&log_dir()?, app, version)
    }

    pub fn open_in(dir: &Path, app: &str, version: &str) -> Option<Arc<Self>> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(format!("{app}.log"));

        // Keep one previous generation instead of growing without bound.
        if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
            let _ = std::fs::rename(&path, dir.join(format!("{app}.log.1")));
        }

        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let log = Arc::new(Self { path, file: Mutex::new(file) });
        log.write_line(&format!(
            "=== {app} v{version} started — {} {} ===",
            std::env::consts::OS,
            std::env::consts::ARCH,
        ));
        Some(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line right now. `File` is unbuffered, so this is on disk
    /// (in the OS cache) when the call returns; `flush` is belt and braces.
    fn write_line(&self, text: &str) {
        let mut line = String::with_capacity(text.len() + 22);
        line.push_str(&utc_timestamp(SystemTime::now()));
        line.push(' ');
        line.push_str(text);
        line.push('\n');
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }
}

/// A `tracing` layer that writes events to a [`FileLog`].
pub struct FileLayer {
    log: Arc<FileLog>,
    /// Mirror the GUI pane: only this crate's events, INFO and above.
    /// `false` = record whatever the subscriber's own filter lets through.
    app_info_only: bool,
}

impl FileLayer {
    pub fn new(log: Arc<FileLog>, app_info_only: bool) -> Self {
        Self { log, app_info_only }
    }
}

impl<S: Subscriber> Layer<S> for FileLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if self.app_info_only
            && (!meta.target().starts_with("mc_share") || *meta.level() > Level::INFO)
        {
            return;
        }
        let mut v = LineVisitor::default();
        event.record(&mut v);
        self.log
            .write_line(&format!("{:<5} {}: {}{}", meta.level(), meta.target(), v.message, v.fields));
    }
}

#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: String,
}

impl Visit for LineVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            self.fields.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push_str(&format!(" {}={}", field.name(), value));
        }
    }
}

/// `2023-11-14T22:13:20Z` — std only, no date crate.
fn utc_timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tracing_subscriber::layer::SubscriberExt;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mscale-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(utc_timestamp(UNIX_EPOCH + Duration::from_secs(0)), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(UNIX_EPOCH + Duration::from_secs(1_700_000_000)), "2023-11-14T22:13:20Z");
        // leap day
        assert_eq!(utc_timestamp(UNIX_EPOCH + Duration::from_secs(1_709_164_800)), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn events_are_on_disk_as_soon_as_they_happen() {
        let dir = tmpdir("immediate");
        let log = FileLog::open_in(&dir, "t", "9.9.9").unwrap();
        let sub = tracing_subscriber::registry().with(FileLayer::new(log.clone(), false));
        tracing::subscriber::with_default(sub, || {
            tracing::info!("first line");
            // Read back *while the subscriber and file are still open*:
            // nothing may be sitting in a buffer.
            let so_far = std::fs::read_to_string(log.path()).unwrap();
            assert!(so_far.contains("INFO"), "{so_far}");
            assert!(so_far.contains("first line"), "{so_far}");
            tracing::warn!(room = "abc123", "second line");
        });
        let all = std::fs::read_to_string(log.path()).unwrap();
        assert!(all.contains("=== t v9.9.9 started"), "{all}");
        assert!(all.contains("WARN") && all.contains("second line") && all.contains("room=abc123"), "{all}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_across_runs() {
        let dir = tmpdir("append");
        { let a = FileLog::open_in(&dir, "t", "1").unwrap(); a.write_line("run one"); }
        { let b = FileLog::open_in(&dir, "t", "2").unwrap(); b.write_line("run two"); }
        let all = std::fs::read_to_string(dir.join("t.log")).unwrap();
        assert!(all.contains("run one") && all.contains("run two") && all.matches("started").count() == 2, "{all}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_log_rotates_to_dot_one() {
        let dir = tmpdir("rotate");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.log"), vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        let log = FileLog::open_in(&dir, "t", "1").unwrap();
        assert!(dir.join("t.log.1").exists());
        assert!(std::fs::metadata(log.path()).unwrap().len() < 1024); // fresh file
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gui_mode_keeps_only_app_info_and_above() {
        let dir = tmpdir("filter");
        let log = FileLog::open_in(&dir, "t", "1").unwrap();
        let sub = tracing_subscriber::registry().with(FileLayer::new(log.clone(), true));
        tracing::subscriber::with_default(sub, || {
            tracing::debug!(target: "mc_share::x", "too chatty");
            tracing::info!(target: "winit::event", "other crate");
            tracing::info!(target: "mc_share::x", "kept");
        });
        let all = std::fs::read_to_string(log.path()).unwrap();
        assert!(all.contains("kept") && !all.contains("too chatty") && !all.contains("other crate"), "{all}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
