//! Starting with the Mac.
//!
//! A launch agent rather than a login item: it survives a crash, starts
//! before the desktop is ready, and can be turned on and off from the menu
//! without asking anyone for a password.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.studiolxd.minion";

fn agent_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// Whether Minion is set to start with the Mac.
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

    let executable = std::env::current_exe()
        .map_err(|e| format!("cannot locate myself: {e}"))?;
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
    <key>ThrottleInterval</key> <integer>10</integer>
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
