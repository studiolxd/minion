//! Where Minion writes down what it heard.
//!
//! The log is not a debugging leftover: it is the only way to find out why
//! a command did not fire, and the raw material for adding aliases. So it
//! has to exist regardless of how the app was started — launched from a
//! terminal, from launchd, or by double-clicking the bundle, in which case
//! macOS sends stdout to /dev/null and anything printed is simply lost.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::Local;

/// Rotate once the file passes this, keeping one previous copy.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Path of the log file.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Logs/minion.log"))
}

fn handle() -> Option<&'static Mutex<File>> {
    FILE.get_or_init(|| {
        let path = path()?;
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Keep one generation, so a long-running session cannot fill the disk.
        if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = fs::rename(&path, path.with_extension("log.1"));
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

/// Local date and time.
///
/// Both parts matter. The log outlives the session that wrote it, so a bare
/// time of day is ambiguous the next morning — and it has to be local time,
/// or the timestamps disagree with the clock in the menu bar.
fn timestamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Writes one line to the log and to stdout.
pub fn write(line: &str) {
    let stamped = format!("{}  {line}", timestamp());
    println!("{stamped}");
    if let Some(file) = handle() {
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{stamped}");
            let _ = file.flush();
        }
    }
}

/// Convenience macro so call sites read like `println!`.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => { $crate::journal::write(&format!($($arg)*)) };
}
