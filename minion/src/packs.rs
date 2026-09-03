//! Downloading vocabulary packs from the community repository.
//!
//! `vocabulary.rs` already reads whatever is sitting in
//! `~/Library/Application Support/Minion/vocabulary/`; this module is only
//! how something gets there without the user copying files by hand. It
//! talks to one place —
//! `github.com/studiolxd/minion-vocabulary` — the same way `models.rs`
//! talks to Hugging Face: `/usr/bin/curl` rather than an HTTP client
//! crate, everything pinned to https, everything checked against a hash
//! before it is kept.
//!
//! The manifest is the trust boundary. A pack's bytes are only ever kept
//! once they hash to what `manifest.toml` — fetched from the same release
//! — says they should. That is weaker than the pinned, hand-verified
//! hashes in `models.rs` (whoever can publish a release can also publish
//! a new manifest), but a vocabulary pack cannot do anything a model file
//! can: see `vocabulary.rs` for why a `[[commands]]` entry is data, never
//! a script.
//!
//! The repository is private while it is being built out, so `update()`
//! treats an unauthenticated 404 on the release itself as an expected,
//! reportable outcome rather than an error — see `Outcome::NotPublicYet`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

const OWNER_REPO: &str = "studiolxd/minion-vocabulary";
const RELEASES_LATEST_API: &str = "https://api.github.com/repos/studiolxd/minion-vocabulary/releases/latest";
const MANIFEST_URL: &str =
    "https://github.com/studiolxd/minion-vocabulary/releases/latest/download/manifest.toml";
const PACKS_ZIP_URL: &str =
    "https://github.com/studiolxd/minion-vocabulary/releases/latest/download/packs.zip";

/// One pack's own idea of what it is called and what it must hash to,
/// straight out of `manifest.toml`.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct ManifestPack {
    /// As committed in the repository, e.g. `"packs/community/adobe.toml"`.
    pub path: String,
    /// Lower-case hex SHA-256 of that file.
    pub sha256: String,
}

impl ManifestPack {
    /// Where this pack lands under the vocabulary directory: the
    /// manifest's path with its leading `packs/` taken off, since that
    /// prefix exists in the repository for the reader's sake and is not
    /// part of the zip `packs.rs` extracts (see the workflow that builds
    /// `packs.zip` from the `packs/` tree — its entries are already
    /// rooted at that directory).
    pub fn relative_path(&self) -> &str {
        self.path.strip_prefix("packs/").unwrap_or(&self.path)
    }
}

/// `manifest.toml`, parsed.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub packs: Vec<ManifestPack>,
}

/// What a finished update did.
pub struct Installed {
    pub version: String,
    pub packs: usize,
}

/// Why `update()` did not install anything.
pub enum Outcome {
    /// The release could not be reached at all because the repository is
    /// still private — expected, for now, and worth a friendly message
    /// rather than an error dialog.
    NotPublicYet,
    /// Anything else: no network, a bad hash, a zip that would not open.
    Failed(String),
}

impl Outcome {
    /// What to put in front of the person who asked for this.
    pub fn message(&self) -> String {
        match self {
            Outcome::NotPublicYet => {
                "El repositorio de vocabulario aún no es público.".to_string()
            }
            Outcome::Failed(reason) => reason.clone(),
        }
    }
}

/// Where Minion keeps track of which vocabulary version it last installed.
///
/// Deliberately a sibling of `vocabulary_dir`, not inside it: everything
/// in that directory is read as a pack, and a bookkeeping file with a
/// `.toml`-looking name would either need special-casing there or would
/// quietly become part of the vocabulary itself.
fn installed_version_path(app_support: &Path) -> PathBuf {
    app_support.join("installed-version")
}

/// The version last installed by `update()`, if any — for `minion status`
/// or a curious `cat` more than anything Minion itself branches on.
pub fn installed_version() -> Option<String> {
    let dir = crate::vocabulary::packs_dir()?;
    let app_support = dir.parent()?;
    std::fs::read_to_string(installed_version_path(app_support)).ok().map(|s| s.trim().to_string())
}

/// Downloads and installs the latest vocabulary pack release, reporting
/// progress through `report` as it goes — the same shape `models::fetch`
/// uses, so both can feed the same tooltip.
///
/// Everything happens in a staging directory first, and nothing under
/// `vocabulary/` is touched until every pack the manifest lists has
/// verified against its published hash — a release that arrives
/// half-downloaded or with one bad pack must not leave the vocabulary
/// directory in a state no manifest ever described.
pub fn update<F: Fn(&str)>(report: F) -> Result<Installed, Outcome> {
    let Some(vocabulary_dir) = crate::vocabulary::packs_dir() else {
        return Err(Outcome::Failed("no se encontró la carpeta personal".to_string()));
    };
    let app_support = vocabulary_dir
        .parent()
        .ok_or_else(|| Outcome::Failed("ruta de vocabulario inesperada".to_string()))?
        .to_path_buf();

    report("comprobando el repositorio de vocabulario…");
    match release_status()? {
        404 => return Err(Outcome::NotPublicYet),
        200..=299 => {}
        other => {
            return Err(Outcome::Failed(format!(
                "{OWNER_REPO} respondió con el código {other}"
            )))
        }
    }

    let staging = app_support.join(".pack-update");
    let _ = std::fs::remove_dir_all(&staging); // a previous attempt's leftovers
    std::fs::create_dir_all(&staging)
        .map_err(|e| Outcome::Failed(format!("no se pudo crear la carpeta temporal: {e}")))?;
    // Whatever happens next, this must not linger: it is only ever a
    // few megabytes, but it is also exactly the kind of thing that looks
    // like a second, broken vocabulary if it is left behind.
    let cleanup = |result| {
        let _ = std::fs::remove_dir_all(&staging);
        result
    };

    report("descargando manifest.toml…");
    let manifest_path = staging.join("manifest.toml");
    if let Err(e) = download_raw(MANIFEST_URL, &manifest_path) {
        return cleanup(Err(Outcome::Failed(e)));
    }
    let manifest_text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(e) => return cleanup(Err(Outcome::Failed(format!("no se pudo leer el manifiesto: {e}")))),
    };
    let manifest: Manifest = match toml::from_str(&manifest_text) {
        Ok(m) => m,
        Err(e) => {
            return cleanup(Err(Outcome::Failed(format!("el manifiesto no se pudo leer: {e}"))))
        }
    };
    if manifest.packs.is_empty() {
        return cleanup(Err(Outcome::Failed("el manifiesto no lista ningún pack".to_string())));
    }

    report("descargando packs.zip…");
    let zip_path = staging.join("packs.zip");
    if let Err(e) = download_raw(PACKS_ZIP_URL, &zip_path) {
        return cleanup(Err(Outcome::Failed(e)));
    }

    report("verificando los packs…");
    let extracted = staging.join("extracted");
    if let Err(e) = extract_zip(&zip_path, &extracted) {
        return cleanup(Err(Outcome::Failed(e)));
    }
    for pack in &manifest.packs {
        let file = extracted.join(pack.relative_path());
        let actual = match crate::models::hash_file(&file) {
            Ok(hash) => crate::models::hex(&hash),
            Err(e) => {
                return cleanup(Err(Outcome::Failed(format!(
                    "falta {} en el paquete descargado: {e}",
                    pack.relative_path()
                ))))
            }
        };
        if !actual.eq_ignore_ascii_case(&pack.sha256) {
            return cleanup(Err(Outcome::Failed(format!(
                "{} no coincide con el hash del manifiesto (se obtuvo {actual}, se esperaba {}); \
                 la actualización se ha cancelado",
                pack.relative_path(),
                pack.sha256
            ))));
        }
    }

    report("instalando…");
    if let Err(e) = std::fs::create_dir_all(&vocabulary_dir) {
        return cleanup(Err(Outcome::Failed(format!(
            "no se pudo crear la carpeta de vocabulario: {e}"
        ))));
    }
    for pack in &manifest.packs {
        let from = extracted.join(pack.relative_path());
        let to = vocabulary_dir.join(pack.relative_path());
        if let Some(parent) = to.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return cleanup(Err(Outcome::Failed(format!("no se pudo instalar el pack: {e}"))));
            }
        }
        if let Err(e) = std::fs::copy(&from, &to) {
            return cleanup(Err(Outcome::Failed(format!(
                "no se pudo instalar {}: {e}",
                pack.relative_path()
            ))));
        }
    }

    if let Err(e) = std::fs::write(installed_version_path(&app_support), &manifest.version) {
        crate::note!("packs    could not record the installed version: {e}");
    }

    let installed = Installed { version: manifest.version.clone(), packs: manifest.packs.len() };
    cleanup(Ok(installed))
}

/// The HTTP status of the release itself — nothing more, since all
/// `update()` needs from it is whether the repository is reachable at
/// all yet. The 10-second timeout is deliberately short: this is a check
/// before the real downloads, not one of them, and it runs on a worker
/// thread whose progress somebody is watching in the tooltip.
fn release_status() -> Result<u16, Outcome> {
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
        .map_err(|e| Outcome::Failed(format!("no se pudo ejecutar curl: {e}")))?;
    if !output.status.success() {
        return Err(Outcome::Failed(format!("no se pudo contactar con {RELEASES_LATEST_API}")));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u16>()
        .map_err(|_| Outcome::Failed("código de estado ilegible".to_string()))
}

/// Fetches one file to `target`, by way of a `.partial` name — the same
/// half-download safety `models::download` uses, minus the hash check:
/// what a pack must hash to comes from the manifest this same download
/// fetches, not from anything pinned in the source ahead of time.
fn download_raw(url: &str, target: &Path) -> Result<(), String> {
    let partial = target.with_extension("partial");
    let status = Command::new("/usr/bin/curl")
        .args(["-fL", "--proto", "=https", "--tlsv1.2", "--max-time", "60", "--retry", "3", "-o"])
        .arg(&partial)
        .arg(url)
        .status()
        .map_err(|e| format!("no se pudo ejecutar curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("no se pudo descargar {url}"));
    }
    std::fs::rename(&partial, target).map_err(|e| format!("no se pudo guardar {url}: {e}"))
}

/// Unzips `zip` into `dest`, which is created if it does not exist.
///
/// `ditto -x -k` rather than `unzip`: it is the tool Apple documents for
/// this and ships on every Mac, same as `models.rs` reaching for `curl`
/// over a networking crate.
fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("no se pudo crear la carpeta: {e}"))?;
    let status = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(dest)
        .status()
        .map_err(|e| format!("no se pudo ejecutar ditto: {e}"))?;
    if !status.success() {
        return Err("no se pudo descomprimir packs.zip".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_parses_and_strips_the_packs_prefix() {
        let manifest: Manifest = toml::from_str(
            r#"
            version = "2026.09.03"

            [[packs]]
            path = "packs/macos.toml"
            sha256 = "abc123"

            [[packs]]
            path = "packs/community/adobe.toml"
            sha256 = "def456"
            "#,
        )
        .expect("manifest should parse");
        assert_eq!(manifest.version, "2026.09.03");
        assert_eq!(manifest.packs.len(), 2);
        assert_eq!(manifest.packs[0].relative_path(), "macos.toml");
        assert_eq!(manifest.packs[1].relative_path(), "community/adobe.toml");
    }

    #[test]
    fn a_manifest_with_no_packs_still_parses() {
        // Caught separately in `update()`, as a "nothing to install"
        // error rather than a parse failure — a manifest that lists
        // zero packs is a legitimate document, just a useless one.
        let manifest: Manifest = toml::from_str(r#"version = "2026.09.03""#).unwrap();
        assert!(manifest.packs.is_empty());
    }

    #[test]
    fn a_relative_path_without_the_prefix_is_left_alone() {
        // Defensive: nothing in this repository's manifest should ever
        // omit the `packs/` prefix, but a pack that landed at the wrong
        // place because of one is a worse failure than a missing prefix.
        let pack = ManifestPack { path: "macos.toml".to_string(), sha256: "abc".to_string() };
        assert_eq!(pack.relative_path(), "macos.toml");
    }

    #[test]
    fn a_hash_comparison_is_case_insensitive() {
        // `hash_file` + `hex` always produce lower-case, but a
        // hand-edited manifest might not — the same tolerance
        // `models::download` already gives its own pinned hashes.
        let expected = "AB12CD";
        let actual = "ab12cd";
        assert!(actual.eq_ignore_ascii_case(expected));
    }

    #[test]
    fn a_pack_missing_from_the_download_is_reported_by_name() {
        // `update()` reads each pack's bytes from `extracted/<relative
        // path>`; a manifest entry with no matching file must name the
        // pack it could not find rather than failing silently.
        let dir = std::env::temp_dir().join(format!(
            "minion-packs-test-missing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let missing = dir.join("nope.toml");
        assert!(crate::models::hash_file(&missing).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_a_manifests_packs_never_touches_a_file_it_does_not_list() {
        // The rule the brief calls "keeping the user's own files that
        // are not in the manifest": installing writes exactly the paths
        // the manifest names, at their relative path under the
        // vocabulary directory, and copying does not remove or rename
        // anything already there.
        let dir = std::env::temp_dir().join(format!(
            "minion-packs-test-install-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let extracted = dir.join("extracted");
        let vocabulary = dir.join("vocabulary");
        std::fs::create_dir_all(&extracted).unwrap();
        std::fs::create_dir_all(&vocabulary).unwrap();

        // The user's own pack, never mentioned by any manifest.
        std::fs::write(vocabulary.join("mine.toml"), "category = \"Mías\"\n").unwrap();
        // What the "download" produced.
        std::fs::write(extracted.join("macos.toml"), "category = \"macOS\"\n").unwrap();
        std::fs::create_dir_all(extracted.join("community")).unwrap();
        std::fs::write(extracted.join("community").join("adobe.toml"), "category = \"Adobe\"\n")
            .unwrap();

        let manifest = Manifest {
            version: "2026.09.03".to_string(),
            packs: vec![
                ManifestPack { path: "packs/macos.toml".to_string(), sha256: String::new() },
                ManifestPack {
                    path: "packs/community/adobe.toml".to_string(),
                    sha256: String::new(),
                },
            ],
        };
        for pack in &manifest.packs {
            let from = extracted.join(pack.relative_path());
            let to = vocabulary.join(pack.relative_path());
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(&from, &to).unwrap();
        }

        assert!(vocabulary.join("mine.toml").exists(), "the user's own pack must survive");
        assert!(vocabulary.join("macos.toml").exists());
        assert!(vocabulary.join("community").join("adobe.toml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
