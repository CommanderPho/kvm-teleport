//! Durable runtime logging to a per-role file.
//!
//! The Windows build is GUI-subsystem: a bare launch (and the daemon
//! child it spawns) has no console, so `env_logger`'s stderr output
//! goes nowhere and a failed connect leaves no trace the user can
//! find. [`init`] installs one combined logger that keeps the usual
//! `env_logger` console behaviour and tees every record into
//! `<log dir>/mousehop-<role>.log` (with one rotated `.log.old`), so
//! dial/DNS/handshake diagnostics survive regardless of how the app
//! was started.
//!
//! The GUI and daemon share the directory but write separate files
//! (`mousehop-gui.log` / `mousehop-daemon.log`) so concurrent appends
//! never interleave mid-record.

use log::Log;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Rotate the live log once it passes this size, keeping one previous
/// generation (`*.log.old`). Bounds disk use without a date machinery.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

struct FileLogger {
    inner: Mutex<Inner>,
    level: log::LevelFilter,
}

struct Inner {
    file: Option<File>,
    path: PathBuf,
    written: u64,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "[{now} {:<5} {}] {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if inner.file.is_none() {
            return;
        }
        if inner.written + line.len() as u64 > MAX_LOG_BYTES {
            rotate(&mut inner);
        }
        if let Some(file) = inner.file.as_mut() {
            if file.write_all(line.as_bytes()).is_ok() {
                inner.written += line.len() as u64;
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(file) = inner.file.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// Move the live log to `*.log.old` (replacing any previous generation)
/// and reopen a fresh file. Best-effort: on any failure logging simply
/// stops rather than affecting the app.
fn rotate(inner: &mut Inner) {
    inner.file = None;
    let old = old_path(&inner.path);
    let _ = fs::remove_file(&old);
    if fs::rename(&inner.path, &old).is_err() {
        // Rename can fail while another reader holds the file (e.g. an
        // editor tailing it on Windows). Truncate in place instead so
        // the size bound still holds.
        let _ = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&inner.path);
    }
    inner.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&inner.path)
        .ok();
    inner.written = inner
        .file
        .as_ref()
        .and_then(|f| f.metadata().ok())
        .map(|m| m.len())
        .unwrap_or(0);
}

fn old_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

/// Platform log directory, mirroring [`crate::panic_log`]'s locations.
fn log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut p = PathBuf::from(std::env::var_os("HOME")?);
        p.push("Library/Logs/Mousehop");
        Some(p)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            let mut p = PathBuf::from(state);
            p.push("mousehop");
            return Some(p);
        }
        let mut p = PathBuf::from(std::env::var_os("HOME")?);
        p.push(".local/state/mousehop");
        Some(p)
    }
    #[cfg(windows)]
    {
        let mut p = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        p.push("Mousehop");
        p.push("logs");
        Some(p)
    }
}

/// Install the combined console + file logger. `role` separates the
/// GUI and daemon logs. Must be used *instead of* `env_logger::init`
/// — the `log` crate only accepts one global logger per process, so
/// this builds the `env_logger` instance itself and tees its output
/// into the file. Console behaviour (level, `MOUSEHOP_LOG_LEVEL` /
/// `RUST_LOG` overrides) is exactly `env_logger`'s; the file captures
/// the same records. Best-effort: if the directory or file can't be
/// opened only console logging remains.
pub fn init(env: env_logger::Env, role: &'static str) {
    let console = env_logger::Builder::from_env(env).build();
    let console_level = console.filter();
    let file = log_dir()
        .filter(|dir| fs::create_dir_all(dir).is_ok())
        .map(|dir| dir.join(format!("mousehop-{role}.log")))
        .and_then(|path| {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()?;
            let written = file.metadata().map(|m| m.len()).unwrap_or(0);
            Some(FileLogger {
                inner: Mutex::new(Inner {
                    file: Some(file),
                    path,
                    written,
                }),
                level: console_level,
            })
        });
    let max = console_level.max(
        file.as_ref()
            .map(|f| f.level)
            .unwrap_or(log::LevelFilter::Off),
    );
    let logger = Tee { console, file };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(max);
        log::info!("{role} logging initialised (console + file, level {console_level})");
    }
}

struct Tee {
    console: env_logger::Logger,
    file: Option<FileLogger>,
}

impl Log for Tee {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.console.enabled(metadata) || self.file.as_ref().is_some_and(|f| f.enabled(metadata))
    }
    fn log(&self, record: &log::Record) {
        if let Some(file) = &self.file {
            file.log(record);
        }
        self.console.log(record);
    }
    fn flush(&self) {
        if let Some(file) = &self.file {
            file.flush();
        }
        self.console.flush();
    }
}
