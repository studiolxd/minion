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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::Local;

/// Rotate once the file passes this, keeping one previous copy.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// How often, in lines written, to check the file's size while running.
///
/// A `stat` on every line would be wasteful for a file that is checked
/// once and then grows for hours; every 200 lines is often enough that the
/// file cannot run away far past `MAX_BYTES` between checks.
const CHECK_EVERY: u32 = 200;

/// An open log file and the path it was opened from, so it can be renamed
/// and reopened at the same place when it grows too large.
struct JournalFile {
    file: File,
    path: PathBuf,
}

static FILE: OnceLock<Option<Mutex<JournalFile>>> = OnceLock::new();

/// Lines written since the size was last checked. Outside the `Mutex`
/// because it only needs to survive across calls, not be consistent with
/// any particular write.
static LINES_SINCE_CHECK: AtomicU32 = AtomicU32::new(0);

/// Path of the log file.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Logs/minion.log"))
}

/// Whether it has been long enough since the last size check to do
/// another one. Pure, so the "every 200 lines" policy is testable without
/// a filesystem.
fn due_for_size_check(lines_since_check: u32) -> bool {
    lines_since_check >= CHECK_EVERY
}

/// Whether a file this large should be rotated. Pure for the same reason.
fn over_the_limit(size: u64) -> bool {
    size > MAX_BYTES
}

fn handle() -> Option<&'static Mutex<JournalFile>> {
    FILE.get_or_init(|| {
        let path = path()?;
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Keep one generation, so a long-running session cannot fill the disk.
        if fs::metadata(&path).is_ok_and(|m| over_the_limit(m.len())) {
            let _ = fs::rename(&path, path.with_extension("log.1"));
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(|file| Mutex::new(JournalFile { file, path }))
    })
    .as_ref()
}

/// Renames the current file aside and opens a fresh one at the same path.
///
/// Just renaming would not be enough: the already-open handle would keep
/// appending to the renamed file through its old inode, so the rotation
/// would never actually stop the file from growing. Reopening is what
/// makes the new writes land in a new, empty file.
fn rotate(journal: &mut JournalFile) {
    let _ = fs::rename(&journal.path, journal.path.with_extension("log.1"));
    if let Ok(fresh) = OpenOptions::new().create(true).append(true).open(&journal.path) {
        journal.file = fresh;
    }
}

/// Local date and time.
///
/// Both parts matter. The log outlives the session that wrote it, so a bare
/// time of day is ambiguous the next morning — and it has to be local time,
/// or the timestamps disagree with the clock in the menu bar.
fn timestamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Whether stdout is a terminal, as opposed to a file or a pipe.
///
/// launchd redirects Minion's stdout to `minion-launch.log`, so printing
/// unconditionally there duplicated every line already written to
/// `minion.log` into a second, unrotated file. Printing only when someone
/// is actually watching a terminal keeps that from happening while leaving
/// interactive runs exactly as chatty as before.
fn stdout_is_a_terminal() -> bool {
    unsafe { libc::isatty(1) != 0 }
}

/// Writes one line to the log, and to stdout when stdout is a terminal.
///
/// Under test the file is never touched: the log belongs to the person
/// running Minion, and a test run should not leave entries in it.
pub fn write(line: &str) {
    let stamped = format!("{}  {line}", timestamp());
    if stdout_is_a_terminal() {
        println!("{stamped}");
    }
    if cfg!(test) {
        return;
    }
    let Some(handle) = handle() else { return };
    let Ok(mut journal) = handle.lock() else { return };

    let _ = writeln!(journal.file, "{stamped}");
    let _ = journal.file.flush();

    let lines = LINES_SINCE_CHECK.fetch_add(1, Ordering::Relaxed) + 1;
    if due_for_size_check(lines) {
        LINES_SINCE_CHECK.store(0, Ordering::Relaxed);
        if let Ok(size) = journal.file.metadata().map(|m| m.len()) {
            if over_the_limit(size) {
                rotate(&mut journal);
            }
        }
    }
}

/// Convenience macro so call sites read like `println!`.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => { $crate::journal::write(&format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_check_is_not_due_on_every_line() {
        assert!(!due_for_size_check(1));
        assert!(!due_for_size_check(CHECK_EVERY - 1));
    }

    #[test]
    fn a_size_check_is_due_once_the_interval_is_reached() {
        assert!(due_for_size_check(CHECK_EVERY));
        assert!(due_for_size_check(CHECK_EVERY + 50));
    }

    #[test]
    fn rotation_triggers_only_once_the_file_is_over_the_limit() {
        assert!(!over_the_limit(MAX_BYTES));
        assert!(over_the_limit(MAX_BYTES + 1));
    }
}
