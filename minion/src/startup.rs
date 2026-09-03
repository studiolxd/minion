//! Starting with the Mac.
//!
//! A launch agent rather than a login item: it survives a crash, starts
//! before the desktop is ready, and can be turned on and off from the menu
//! without asking anyone for a password.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "com.studiolxd.minion";

/// Where the installed copy lives, if `install.sh` has put one there.
const INSTALLED_PATH: &str = "/Applications/Minion.app/Contents/MacOS/minion";

/// Which binary the login item should point launchd at.
///
/// A development build's `current_exe()` is `target/…` or a repo-local
/// `Minion.app` — a path that stops existing the moment that build is
/// cleaned, taking "start at login" down with it. The installed copy at
/// `/Applications` is what a login item should point at whenever there is
/// one, whichever copy happened to write the plist.
///
/// Pure — takes whether the installed copy exists rather than checking the
/// filesystem itself — so the choice can be tested without one.
fn login_item_path(current_exe: &Path, installed_exists: bool) -> PathBuf {
    if installed_exists {
        PathBuf::from(INSTALLED_PATH)
    } else {
        current_exe.to_path_buf()
    }
}

fn agent_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// Whether Minion is set to start with the Mac.
/// Asks launchd to stop and start the launch agent, if this copy runs
/// under it. True when launchd accepted — in which case this process is
/// about to be killed and should not bother starting anything itself.
pub fn kickstart() -> bool {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    // Only a job that launchd actually manages can be kicked; a copy opened
    // by hand while the agent is installed but not loaded is not one.
    let managed = Command::new("/bin/launchctl")
        .args(["print", &format!("{domain}/{LABEL}")])
        .output()
        .is_ok_and(|out| out.status.success());
    let started_by_launchd =
        std::env::var_os("XPC_SERVICE_NAME").as_deref() == Some(std::ffi::OsStr::new(LABEL));
    if !managed || !started_by_launchd {
        return false;
    }
    Command::new("/bin/launchctl")
        .args(["kickstart", "-k", &format!("{domain}/{LABEL}")])
        .status()
        .is_ok_and(|status| status.success())
}

pub fn enabled() -> bool {
    agent_path().is_some_and(|path| path.exists())
}

/// Turns starting at login on or off.
pub fn set(enabled: bool) -> Result<(), String> {
    let path = agent_path().ok_or("no home directory")?;
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");

    if !enabled {
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("{domain}/{LABEL}")])
            .output();
        let _ = fs::remove_file(&path);
        return Ok(());
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate myself: {e}"))?;
    let installed_exists = Path::new(INSTALLED_PATH).exists();
    let executable = login_item_path(&current_exe, installed_exists);
    if !installed_exists {
        crate::note!(
            "Iniciar al arrancar apunta a {} — no es una copia instalada en \
             /Applications; desaparecerá si se borra.",
            executable.display()
        );
    }
    let home = std::env::var("HOME").unwrap_or_default();

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>            <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array><string>{}</string></array>
    <key>RunAtLoad</key>        <true/>
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key> <integer>30</integer>
    <key>StandardOutPath</key>  <string>{home}/Library/Logs/minion-launch.log</string>
    <key>StandardErrorPath</key><string>{home}/Library/Logs/minion-launch.log</string>
</dict>
</plist>
"#,
        executable.display()
    );

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, plist).map_err(|e| format!("cannot write the agent: {e}"))?;

    // Deliberately not bootstrapped here. Loading the agent starts the
    // program, and the copy asking for this is already running — which put
    // two faces in the menu bar. Writing the file is enough: launchd reads
    // it at the next login, which is what "start at login" means.
    let _ = domain;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_copy_is_preferred_when_it_exists() {
        let dev_build = Path::new("/Users/someone/minion/target/release/minion");
        assert_eq!(
            login_item_path(dev_build, true),
            PathBuf::from(INSTALLED_PATH)
        );
    }

    #[test]
    fn a_dev_build_points_at_itself_when_nothing_is_installed() {
        let dev_build = Path::new("/Users/someone/minion/target/release/minion");
        assert_eq!(login_item_path(dev_build, false), dev_build.to_path_buf());
    }
}
