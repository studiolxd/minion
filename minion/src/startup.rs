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
