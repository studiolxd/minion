//! Fetching the speech models.
//!
//! They are 670 MB and never change, which makes them a poor thing to carry
//! inside the application: every rebuild copies them, every install writes
//! them again, and sharing the app means sharing them too. They are
//! downloaded once into Application Support instead, where they also
//! survive reinstalling.
//!
//! Both are fetched from a pinned commit rather than `resolve/main`, and
//! checked against a known SHA-256 before being kept: `main` is a mutable
//! ref, and a model swapped out from under this app would quietly change
//! who "the owner" is for the speaker check, or what a command transcribes
//! to.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file to fetch: where it lives, what it is called on disk, and the
/// hash it must have once downloaded.
struct Piece {
    remote: &'static str,
    local: &'static str,
    /// Roughly, for the progress line.
    megabytes: u32,
    /// Lower-case hex SHA-256 of the file this repository and revision
    /// actually serves, computed with `shasum -a 256` against a copy of
    /// the model already on disk.
    sha256: &'static str,
}

// istupakov/parakeet-tdt-0.6b-v3-onnx, pinned to the commit current when
// these hashes were taken (2026-09-03). Update both together: the SHA-256s
// below are only valid for the files this exact revision serves.
const SPEECH_REVISION: &str = "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce";
const SPEECH_BASE: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve";

// Wespeaker/wespeaker-ecapa-tdnn512-LM, same reasoning.
const SPEAKER_REVISION: &str = "a2f3dcb1c8702caccc7a55ceb57f5e8d1842112b";
const SPEAKER_URL_BASE: &str = "https://huggingface.co/Wespeaker/wespeaker-ecapa-tdnn512-LM/resolve";
const SPEAKER_SHA256: &str = "d71b85d9b48058ef68004f04f1b78acebefb9dfcf542e19b976a12a5ad1f10b0";

// snakers4/silero-vad, the voice detector that keeps the two models above
// from being woken by the dishwasher. Two megabytes, and it lives in a
// GitHub repository rather than on Hugging Face — same reasoning, a
// branch is a moving target, so the commit and the hash are both pinned.
const VAD_REVISION: &str = "867c2aa692646a1f1de3e94a15c9dd9f614c0acb";
const VAD_URL_BASE: &str = "https://raw.githubusercontent.com/snakers4/silero-vad";
const VAD_FILE: &str = "src/silero_vad/data/silero_vad.onnx";
const VAD_SHA256: &str = "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3";

const PIECES: &[Piece] = &[
    Piece {
        remote: "config.json",
        local: "config.json",
        megabytes: 1,
        sha256: "666903c76b9798caf2c210afd4f6cd60b08a8dbf9800ec8d7a3bc0d2148ac466",
    },
    Piece {
        remote: "vocab.txt",
        local: "vocab.txt",
        megabytes: 1,
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    },
    Piece {
        remote: "nemo128.onnx",
        local: "nemo128.onnx",
        megabytes: 1,
        sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
    },
    Piece {
        remote: "decoder_joint-model.int8.onnx",
        local: "decoder_joint-model.onnx",
        megabytes: 18,
        sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
    },
    Piece {
        remote: "encoder-model.int8.onnx",
        local: "encoder-model.onnx",
        megabytes: 652,
        sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
    },
];

/// Where downloaded models live.
pub fn directory() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Minion/model"))
}

/// Whether the speech model is already there.
pub fn present(directory: &Path) -> bool {
    directory.join("vocab.txt").exists() && directory.join("encoder-model.onnx").exists()
}

/// Downloads whatever is missing, reporting progress through `report`.
///
/// Uses `curl` rather than an HTTP client: it is on every Mac, handles
/// redirects and resumes, and keeps a large dependency out of a program
/// that otherwise makes no network requests at all.
pub fn fetch<F: Fn(&str)>(directory: &Path, report: F) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| format!("no se pudo crear la carpeta: {e}"))?;
    // The model directory sits inside Application Support/Minion; on a
    // first run this call is often what creates that folder, so make sure
    // it — and everything under it — is not readable by other accounts.
    if let Some(app_support) = directory.parent() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(app_support, std::fs::Permissions::from_mode(0o700));
    }

    let total: u32 = PIECES.iter().map(|p| p.megabytes).sum();
    let mut done = 0;

    for piece in PIECES {
        let target = directory.join(piece.local);
        if target.exists() {
            done += piece.megabytes;
            continue;
        }
        report(&format!(
            "descargando el modelo… {} %",
            done * 100 / total.max(1)
        ));
        download(
            &format!("{SPEECH_BASE}/{SPEECH_REVISION}/{}", piece.remote),
            &target,
            piece.sha256,
        )?;
        done += piece.megabytes;
    }

    // The speaker model is only needed to recognise who is speaking, which
    // is optional — but it is 24 MB, so it comes along rather than becoming
    // a second thing to wait for later.
    let speaker = directory.join("speaker.onnx");
    if !speaker.exists() {
        report("descargando el modelo… 98 %");
        download(
            &format!("{SPEAKER_URL_BASE}/{SPEAKER_REVISION}/voxceleb_ECAPA512_LM.onnx"),
            &speaker,
            SPEAKER_SHA256,
        )?;
    }

    // And the voice detector: 2 MB, and the reason the two models above
    // are not woken up by every noise in the room.
    if !vad_path(directory).exists() {
        report("descargando el modelo… 99 %");
        fetch_vad(directory)?;
    }

    report("modelo descargado");
    Ok(())
}

/// Where the voice detector lives, once it has been downloaded.
pub fn vad_path(directory: &Path) -> PathBuf {
    directory.join("silero_vad.onnx")
}

/// Fetches the voice detector on its own.
///
/// It arrived after the other models, so an installation made before it
/// has everything except this — and [`present`] is already satisfied, so
/// [`fetch`] is never called again to notice. Two megabytes, fetched once,
/// rather than a reason to download 670 again.
pub fn fetch_vad(directory: &Path) -> Result<PathBuf, String> {
    let target = vad_path(directory);
    if target.exists() {
        return Ok(target);
    }
    std::fs::create_dir_all(directory).map_err(|e| format!("no se pudo crear la carpeta: {e}"))?;
    download(
        &format!("{VAD_URL_BASE}/{VAD_REVISION}/{VAD_FILE}"),
        &target,
        VAD_SHA256,
    )?;
    Ok(target)
}

/// Fetches one file, to a temporary name until it is complete and verified.
///
/// A download interrupted halfway would otherwise leave a file that looks
/// present and is not, and the next start would fail in a confusing way.
/// Verifying the hash before the rename catches the same problem for a
/// download that completed but arrived corrupted, or was served by
/// something other than the model this code expects.
fn download(url: &str, target: &Path, expected_sha256: &str) -> Result<(), String> {
    let partial = target.with_extension("partial");
    let status = Command::new("/usr/bin/curl")
        .args(["-fL", "--proto", "=https", "--tlsv1.2", "--max-time", "3600", "--retry", "3", "-o"])
        .arg(&partial)
        .arg(url)
        .status()
        .map_err(|e| format!("no se pudo ejecutar curl: {e}"))?;

    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("no se pudo descargar {url}"));
    }

    let actual = match hash_file(&partial) {
        Ok(hash) => hex(&hash),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            return Err(format!("no se pudo comprobar {}: {e}", partial.display()));
        }
    };
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "{url} no coincide con el hash esperado (se obtuvo {actual}, se esperaba {expected_sha256}); \
             se ha descartado el archivo descargado"
        ));
    }

    std::fs::rename(&partial, target).map_err(|e| format!("no se pudo guardar: {e}"))?;
    let _ = std::io::stdout().flush();
    Ok(())
}

/// SHA-256 of a file's contents, read in chunks so a 650 MB model does not
/// have to be held in memory twice over.
///
/// `pub(crate)`: `packs.rs` verifies downloaded vocabulary packs against
/// the same hash the manifest publishes, and reaches for this rather than
/// hashing a second way.
pub(crate) fn hash_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = hmac_sha256::Hash::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

/// Lower-case hex, to match what `shasum -a 256` prints.
///
/// `pub(crate)`: `packs.rs` prints the same shape of hash when a manifest
/// entry does not match.
pub(crate) fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_directory_is_not_ready() {
        let empty = std::env::temp_dir().join("minion-test-empty");
        let _ = std::fs::create_dir_all(&empty);
        assert!(!present(&empty));
    }

    #[test]
    fn the_pieces_add_up_to_the_expected_size() {
        // If this changes, the progress percentages are wrong too.
        let total: u32 = PIECES.iter().map(|p| p.megabytes).sum();
        assert!((650..=700).contains(&total), "the model is about 670 MB, got {total}");
    }

    #[test]
    fn every_piece_lands_under_the_name_the_loader_expects() {
        // The int8 files are downloaded under the plain names, because that
        // is what parakeet-rs looks for.
        assert!(PIECES.iter().any(|p| p.local == "encoder-model.onnx"));
        assert!(PIECES.iter().any(|p| p.local == "vocab.txt"));
    }

    #[test]
    fn every_piece_has_a_real_looking_sha256() {
        // 64 lower-case hex characters, same shape shasum -a 256 prints.
        for piece in PIECES {
            assert_eq!(piece.sha256.len(), 64, "{} has a malformed hash", piece.local);
            assert!(
                piece.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} hash is not lower-case hex",
                piece.local
            );
        }
        assert_eq!(SPEAKER_SHA256.len(), 64);
        assert_eq!(VAD_SHA256.len(), 64);
        assert!(VAD_SHA256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn urls_are_pinned_to_a_revision_not_a_mutable_branch() {
        for piece in PIECES {
            let url = format!("{SPEECH_BASE}/{SPEECH_REVISION}/{}", piece.remote);
            assert!(!url.contains("/resolve/main/"), "must not resolve against a moving branch");
            assert!(url.contains(SPEECH_REVISION));
        }
        let vad = format!("{VAD_URL_BASE}/{VAD_REVISION}/{VAD_FILE}");
        assert!(!vad.contains("/master/") && !vad.contains("/main/"));
        assert!(vad.contains(VAD_REVISION));
    }

    #[test]
    fn hashing_a_known_file_matches_shasum() {
        let path = std::env::temp_dir().join("minion-test-hash-input");
        std::fs::write(&path, b"hello world").unwrap();
        let digest = hex(&hash_file(&path).unwrap());
        // `printf 'hello world' | shasum -a 256`
        assert_eq!(digest, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }
}
