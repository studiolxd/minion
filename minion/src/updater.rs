//! Updating Minion itself.
//!
//! `packs.rs` downloads vocabulary; this downloads the application. The
//! shape is deliberately the same — `/usr/bin/curl` over https rather than
//! an HTTP client crate, `ditto` to unpack, nothing kept until it hashes
//! to what was published — because the trust question is the same one and
//! there is no reason for two answers to it.
//!
//! What is published is written by `release.sh`: a notarised, stapled
//! `Minion-<version>.zip` and an `appcast.json` beside it saying which
//! version it is and what it must hash to. Both are release assets, so
//! the fixed `releases/latest/download/…` URL always points at the newest
//! one and this module never has to read GitHub's JSON.
//!
//! The repository is private for now, so an unauthenticated request for
//! the latest release comes back 404. That is an expected answer, not an
//! error: [`Check::NotPublicYet`] says so in Spanish and nobody sees a
//! failure dialog for it.
//!
//! Replacing a running application cannot be a copy over itself. The new
//! bundle is unpacked next to the old one, the old one is renamed to
//! `Minion.app.previous`, and only then does the new one take its place —
//! so a failure at any point leaves a complete Minion on disk. The
//! previous copy stays until the new one has started and cleaned it up,
//! which is the one moment we know for certain the replacement works.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

const RELEASES_LATEST_API: &str = "https://api.github.com/repos/studiolxd/minion/releases/latest";
const APPCAST_URL: &str =
    "https://github.com/studiolxd/minion/releases/latest/download/appcast.json";

/// How long an automatic check waits before looking again.
const DAILY: Duration = Duration::from_secs(24 * 60 * 60);

/// `appcast.json`, as `release.sh` writes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Appcast {
    /// The published version, e.g. `"0.3.0"`.
    pub version: String,
    /// Where the zip is.
    pub url: String,
    /// Lower-case hex SHA-256 of that zip.
    pub sha256: String,
    /// One line for the person deciding whether to install it.
    #[serde(default)]
    pub notes: String,
    /// When it was published, ISO-8601. Recorded, not acted on.
    #[serde(default)]
    pub published: String,
}

/// What [`check`] found.
pub enum Check {
    /// Nothing newer than what is running.
    UpToDate,
    /// A newer version, ready to install.
    Available(Appcast),
    /// The repository is still private, so there is nothing to find yet.
    NotPublicYet,
    /// No network, an unreadable appcast, anything else.
    Failed(String),
}

impl Check {
    /// What to put in front of someone who asked for this by hand. An
    /// automatic check says nothing at all unless there is an update.
    pub fn message(&self) -> String {
        match self {
            Check::UpToDate => format!(
                "Minion {} está al día.",
                env!("CARGO_PKG_VERSION")
            ),
            Check::Available(release) => format!(
                "Minion {} disponible. ¿Instalar?",
                release.version
            ),
            Check::NotPublicYet => {
                "No hay actualizaciones disponibles públicamente todavía.".to_string()
            }
            Check::Failed(reason) => format!("No se pudo comprobar si hay actualizaciones: {reason}"),
        }
    }
}

/// Asks the release page what the newest published version is.
///
/// Runs on a worker thread: two `curl` calls, each with its own short
/// timeout, so a slow network cannot hold up the run loop that asked.
pub fn check() -> Check {
    match release_status() {
        Ok(404) => return Check::NotPublicYet,
        Ok(200..=299) => {}
        Ok(other) => return Check::Failed(format!("GitHub respondió con el código {other}")),
        Err(reason) => return Check::Failed(reason),
    }

    let Some(staging) = staging_dir() else {
        return Check::Failed("no se encontró la carpeta personal".to_string());
    };
    let _ = std::fs::create_dir_all(&staging);
    let path = staging.join("appcast.json");
    if let Err(reason) = download(APPCAST_URL, &path) {
        let _ = std::fs::remove_dir_all(&staging);
        return Check::Failed(reason);
    }
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_dir_all(&staging);
    let text = match text {
        Ok(text) => text,
        Err(e) => return Check::Failed(format!("no se pudo leer appcast.json: {e}")),
    };
    match parse_appcast(&text) {
        Ok(release) if is_newer(&release.version, env!("CARGO_PKG_VERSION")) => {
            Check::Available(release)
        }
        Ok(_) => Check::UpToDate,
        Err(reason) => Check::Failed(reason),
    }
}

/// Reads an appcast, rejecting one that names no zip or no hash — both
/// are what makes the download safe to keep, and a release missing either
/// is a broken release, not one to install hopefully.
pub fn parse_appcast(text: &str) -> Result<Appcast, String> {
    let release: Appcast = serde_json::from_str(text)
        .map_err(|e| format!("appcast.json no se pudo leer: {e}"))?;
    if release.version.trim().is_empty() {
        return Err("appcast.json no dice qué versión es".to_string());
    }
    if !release.url.starts_with("https://") {
        return Err("appcast.json no apunta a una descarga https".to_string());
    }
    if release.sha256.len() != 64 || !release.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("appcast.json no trae un SHA-256 válido".to_string());
    }
    Ok(release)
}

/// Whether `candidate` is a later version than `current`.
///
/// Semver enough for what is published here: three numbers, an optional
/// pre-release after `-`, build metadata after `+` ignored. A release
/// beats its own pre-releases (0.3.0 > 0.3.0-beta.1), and anything
/// unparseable loses rather than being installed on a guess.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(new), Some(now)) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    new > now
}

/// A version as something orderable: the three numbers, then a
/// pre-release marker where "no pre-release" must sort *after* every
/// pre-release — hence the `bool` before the string, true for a final
/// release.
fn parse_version(version: &str) -> Option<(u64, u64, u64, bool, String)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split('+').next().unwrap_or(version);
    let (numbers, pre) = match version.split_once('-') {
        Some((numbers, pre)) => (numbers, pre.to_string()),
        None => (version, String::new()),
    };
    let mut parts = numbers.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre.is_empty(), pre))
}

/// Downloads, verifies and installs a release, reporting progress the
/// same way `packs::update` and `models::fetch` do so all three can feed
/// the same tooltip.
///
/// Returns the bundle that is now installed. The caller restarts; see
/// `relaunch()` in `main.rs`, which asks launchd to do it.
pub fn install<F: Fn(&str)>(release: &Appcast, report: F) -> Result<PathBuf, String> {
    let bundle = installed_bundle().ok_or_else(|| "no se encontró Minion.app".to_string())?;
    let staging = staging_dir().ok_or_else(|| "no se encontró la carpeta personal".to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("no se pudo crear la carpeta temporal: {e}"))?;
    let cleanup = |result| {
        let _ = std::fs::remove_dir_all(&staging);
        result
    };

    report(&format!("descargando Minion {}…", release.version));
    let zip = staging.join("Minion.zip");
    if let Err(e) = download(&release.url, &zip) {
        return cleanup(Err(e));
    }

    report("verificando la descarga…");
    let actual = match crate::models::hash_file(&zip) {
        Ok(hash) => crate::models::hex(&hash),
        Err(e) => return cleanup(Err(format!("no se pudo leer la descarga: {e}"))),
    };
    if !actual.eq_ignore_ascii_case(&release.sha256) {
        return cleanup(Err(format!(
            "la descarga no coincide con su hash publicado (se obtuvo {actual}); \
             la actualización se ha cancelado"
        )));
    }

    report("instalando…");
    let extracted = staging.join("extracted");
    if let Err(e) = unzip(&zip, &extracted) {
        return cleanup(Err(e));
    }
    let new_bundle = match bundle_inside(&extracted) {
        Some(path) => path,
        None => return cleanup(Err("el archivo descargado no contiene Minion.app".to_string())),
    };

    // The old copy is kept, under a name nothing launches, until the new
    // one has started — see `discard_previous`.
    let previous = previous_path(&bundle);
    let _ = std::fs::remove_dir_all(&previous);
    if bundle.exists() {
        if let Err(e) = std::fs::rename(&bundle, &previous) {
            return cleanup(Err(format!("no se pudo apartar la copia anterior: {e}")));
        }
    }
    if let Err(e) = copy_bundle(&new_bundle, &bundle) {
        // Put back what was working: an update that fails must not leave
        // the Mac with no Minion at all.
        let _ = std::fs::remove_dir_all(&bundle);
        let _ = std::fs::rename(&previous, &bundle);
        return cleanup(Err(e));
    }
    cleanup(Ok(bundle))
}

/// Where the copy kept during an update sits: `Minion.app.previous`,
/// beside the one it was replaced by.
///
/// A sibling, not a temporary directory: `/Applications` and the staging
/// directory can be on different volumes, and a rename across volumes
/// fails — which is exactly the moment there would be no Minion at all.
pub fn previous_path(bundle: &Path) -> PathBuf {
    let mut name = bundle.file_name().unwrap_or_default().to_os_string();
    name.push(".previous");
    bundle.with_file_name(name)
}

/// Removes the copy an update left behind. Called once the new version is
/// running, which is the only proof the replacement worked.
pub fn discard_previous() {
    if let Some(bundle) = installed_bundle() {
        let previous = previous_path(&bundle);
        if previous.exists() {
            match std::fs::remove_dir_all(&previous) {
                Ok(()) => crate::note!("updater  removed the previous copy"),
                Err(e) => crate::note!("updater  could not remove the previous copy: {e}"),
            }
        }
    }
}

/// The bundle this copy is running from, or the installed one if Minion
/// was started as a bare binary (`cargo run`, a test build) — in which
/// case there is nothing to replace unless one is installed.
fn installed_bundle() -> Option<PathBuf> {
    if let Ok(executable) = std::env::current_exe() {
        if let Some(app) =
            executable.ancestors().find(|path| path.extension().is_some_and(|k| k == "app"))
        {
            return Some(app.to_path_buf());
        }
    }
    let installed = PathBuf::from("/Applications/Minion.app");
    installed.exists().then_some(installed)
}

/// The `Minion.app` inside an unpacked zip: at the top level, since
/// `ditto -c -k --keepParent` puts it there, but looked for one level
/// down as well rather than trusting the shape of somebody's zip.
fn bundle_inside(directory: &Path) -> Option<PathBuf> {
    let is_bundle = |path: &Path| path.extension().is_some_and(|k| k == "app");
    let entries = std::fs::read_dir(directory).ok()?;
    let mut nested = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if is_bundle(&path) {
            return Some(path);
        }
        if path.is_dir() {
            nested.push(path);
        }
    }
    nested.into_iter().find_map(|dir| {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| is_bundle(path))
    })
}

/// Whether an automatic check is due: never more than once a day, and
/// never at all if the setting is off.
pub fn daily_check_due(config: &crate::config::Config) -> bool {
    if !config.check_updates {
        return false;
    }
    let last = last_check_path()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok());
    due(last, SystemTime::now())
}

/// The rule itself, with the clock passed in: never checked means due,
/// otherwise a day since the last one. A timestamp in the future (a
/// clock that moved backwards) counts as "not yet", not as overdue every
/// tick from now on.
fn due(last: Option<SystemTime>, now: SystemTime) -> bool {
    match last {
        None => true,
        Some(last) => now.duration_since(last).map(|since| since >= DAILY).unwrap_or(false),
    }
}

/// Writes down that a check just happened, so the next one waits a day.
pub fn record_check() {
    if let Some(path) = last_check_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, "");
    }
}

/// Where that timestamp lives — the file's own mtime is the timestamp,
/// so there is no format to parse or get wrong.
fn last_check_path() -> Option<PathBuf> {
    // Tests never touch the user's Application Support: see the note in
    // `enroll::save_profile_for` for the same rule.
    if cfg!(test) {
        return None;
    }
    let mut path = crate::config::path()?;
    path.set_file_name("last-update-check");
    Some(path)
}

/// Somewhere to download into, beside the configuration rather than in
/// `/tmp`: a partly-downloaded application is Minion's own business, and
/// the same volume means the install is a rename, not a copy.
fn staging_dir() -> Option<PathBuf> {
    let mut path = crate::config::path()?;
    path.set_file_name(".update");
    Some(path)
}

/// The HTTP status of the latest release, which is all `check` needs to
/// tell "still private" from "something is wrong".
fn release_status() -> Result<u16, String> {
    let output = Command::new("/usr/bin/curl")
        .args([
            "-sS",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "10",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
        ])
        .arg(RELEASES_LATEST_API)
        .output()
        .map_err(|e| format!("no se pudo ejecutar curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("no se pudo contactar con {RELEASES_LATEST_API}"));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u16>()
        .map_err(|_| "código de estado ilegible".to_string())
}

/// Fetches one file by way of a `.partial` name, so an interrupted
/// download can never be mistaken for a finished one.
fn download(url: &str, target: &Path) -> Result<(), String> {
    let partial = target.with_extension("partial");
    let status = Command::new("/usr/bin/curl")
        .args(["-fL", "--proto", "=https", "--tlsv1.2", "--max-time", "300", "--retry", "3", "-o"])
        .arg(&partial)
        .arg(url)
        .status()
        .map_err(|e| format!("no se pudo ejecutar curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("no se pudo descargar {url}"));
    }
    std::fs::rename(&partial, target).map_err(|e| format!("no se pudo guardar la descarga: {e}"))
}

/// `ditto -x -k`, the counterpart of the `ditto -c -k --keepParent` that
/// made the zip: it is the only unpacker that keeps a signature intact.
fn unzip(zip: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("no se pudo crear la carpeta: {e}"))?;
    let status = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(dest)
        .status()
        .map_err(|e| format!("no se pudo ejecutar ditto: {e}"))?;
    if !status.success() {
        return Err("no se pudo descomprimir la actualización".to_string());
    }
    Ok(())
}

/// Copies a bundle, signature and all — `ditto` again rather than a
/// recursive copy by hand, for the same reason.
fn copy_bundle(from: &Path, to: &Path) -> Result<(), String> {
    let status = Command::new("/usr/bin/ditto")
        .arg(from)
        .arg(to)
        .status()
        .map_err(|e| format!("no se pudo ejecutar ditto: {e}"))?;
    if !status.success() {
        return Err("no se pudo instalar la nueva versión".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_version_wins_and_an_older_one_does_not() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        // Ten is not one: a string comparison would get this wrong.
        assert!(is_newer("0.10.0", "0.9.0"));
    }

    #[test]
    fn a_prerelease_loses_to_its_own_release() {
        assert!(is_newer("0.3.0", "0.3.0-beta.1"));
        assert!(!is_newer("0.3.0-beta.1", "0.3.0"));
        assert!(is_newer("0.3.0-beta.2", "0.3.0-beta.1"));
        // Build metadata says nothing about which is newer.
        assert!(!is_newer("0.2.0+build.7", "0.2.0"));
    }

    #[test]
    fn a_version_that_cannot_be_read_is_never_installed() {
        // Better to miss an update than to install whatever a malformed
        // appcast happens to name.
        assert!(!is_newer("mañana", "0.2.0"));
        assert!(is_newer("0.3", "0.2.0")); // two numbers: read as 0.3.0
        assert!(!is_newer("0.3.0.1", "0.2.0"));
        assert!(is_newer("v0.3.0", "0.2.0")); // a tag, tolerated
    }

    #[test]
    fn an_appcast_parses() {
        let release = parse_appcast(
            r#"{
                "version": "0.3.0",
                "url": "https://github.com/studiolxd/minion/releases/download/v0.3.0/Minion-0.3.0.zip",
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                "notes": "Actualizaciones dentro de la app.",
                "published": "2026-09-03T10:00:00Z"
            }"#,
        )
        .expect("appcast should parse");
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.notes, "Actualizaciones dentro de la app.");
    }

    #[test]
    fn an_appcast_without_a_usable_hash_or_url_is_refused() {
        let with = |sha: &str, url: &str| {
            format!(r#"{{"version":"0.3.0","url":"{url}","sha256":"{sha}"}}"#)
        };
        let good = "0".repeat(64);
        let url = "https://example.com/Minion.zip";
        assert!(parse_appcast(&with(&good, url)).is_ok());
        // Truncated, non-hex, and plain http: each one is the difference
        // between "verified" and "whatever arrived".
        assert!(parse_appcast(&with("abc", url)).is_err());
        assert!(parse_appcast(&with(&"z".repeat(64), url)).is_err());
        assert!(parse_appcast(&with(&good, "http://example.com/Minion.zip")).is_err());
        assert!(parse_appcast(r#"{"url":"https://a/b.zip","sha256":"x"}"#).is_err());
    }

    #[test]
    fn a_download_is_only_kept_when_it_hashes_to_what_was_published() {
        // The comparison `install` makes, on its own: case-insensitive,
        // like the one `packs.rs` makes against its manifest.
        let published = "AB12CD";
        assert!("ab12cd".eq_ignore_ascii_case(published));
        assert!(!"ab12ce".eq_ignore_ascii_case(published));
    }

    #[test]
    fn the_previous_copy_sits_beside_the_one_that_replaced_it() {
        let bundle = Path::new("/Applications/Minion.app");
        assert_eq!(previous_path(bundle), Path::new("/Applications/Minion.app.previous"));
        // Same volume as the bundle, whatever that volume is: a rename
        // across volumes is what would leave no Minion at all.
        assert_eq!(previous_path(bundle).parent(), bundle.parent());
    }

    #[test]
    fn a_check_is_due_once_a_day_and_not_before() {
        let now = SystemTime::now();
        assert!(due(None, now), "never checked means due");
        assert!(!due(Some(now), now));
        assert!(!due(Some(now - Duration::from_secs(23 * 3600)), now));
        assert!(due(Some(now - DAILY), now));
        assert!(due(Some(now - Duration::from_secs(72 * 3600)), now));
        // A clock that moved backwards leaves a timestamp in the future.
        // "Not yet" is the safe reading: checking every tick from now on
        // would be a request a second.
        assert!(!due(Some(now + Duration::from_secs(3600)), now));
    }

    #[test]
    fn the_bundle_is_found_at_the_top_of_the_zip_or_one_level_down() {
        let dir = std::env::temp_dir().join(format!(
            "minion-updater-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let flat = dir.join("flat");
        std::fs::create_dir_all(flat.join("Minion.app/Contents")).unwrap();
        assert_eq!(bundle_inside(&flat), Some(flat.join("Minion.app")));

        let nested = dir.join("nested");
        std::fs::create_dir_all(nested.join("payload/Minion.app/Contents")).unwrap();
        assert_eq!(bundle_inside(&nested), Some(nested.join("payload/Minion.app")));

        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(bundle_inside(&empty), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
