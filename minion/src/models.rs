//! Fetching the speech models.
//!
//! They are 670 MB and never change, which makes them a poor thing to carry
//! inside the application: every rebuild copies them, every install writes
//! them again, and sharing the app means sharing them too. They are
//! downloaded once into Application Support instead, where they also
//! survive reinstalling.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file to fetch: where it lives, and what it is called on disk.
struct Piece {
    remote: &'static str,
    local: &'static str,
    /// Roughly, for the progress line.
    megabytes: u32,
}

const SPEECH_BASE: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";
const SPEAKER_URL: &str =
    "https://huggingface.co/Wespeaker/wespeaker-ecapa-tdnn512-LM/resolve/main/voxceleb_ECAPA512_LM.onnx";

const PIECES: &[Piece] = &[
    Piece { remote: "config.json", local: "config.json", megabytes: 1 },
    Piece { remote: "vocab.txt", local: "vocab.txt", megabytes: 1 },
    Piece { remote: "nemo128.onnx", local: "nemo128.onnx", megabytes: 1 },
    Piece {
        remote: "decoder_joint-model.int8.onnx",
        local: "decoder_joint-model.onnx",
        megabytes: 18,
    },
    Piece {
        remote: "encoder-model.int8.onnx",
        local: "encoder-model.onnx",
        megabytes: 652,
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
            "Descargando el modelo de voz… {}%",
            done * 100 / total.max(1)
        ));
        download(&format!("{SPEECH_BASE}/{}", piece.remote), &target)?;
        done += piece.megabytes;
    }

    // The speaker model is only needed to recognise who is speaking, which
    // is optional — but it is 24 MB, so it comes along rather than becoming
    // a second thing to wait for later.
    let speaker = directory.join("speaker.onnx");
    if !speaker.exists() {
        report("Descargando el modelo de voz… 98%");
        download(SPEAKER_URL, &speaker)?;
    }

    report("Modelo descargado.");
    Ok(())
}

/// Fetches one file, to a temporary name until it is complete.
///
/// A download interrupted halfway would otherwise leave a file that looks
/// present and is not, and the next start would fail in a confusing way.
fn download(url: &str, target: &Path) -> Result<(), String> {
    let partial = target.with_extension("partial");
    let status = Command::new("/usr/bin/curl")
        .args(["-fL", "--retry", "3", "-o"])
        .arg(&partial)
        .arg(url)
        .status()
        .map_err(|e| format!("no se pudo ejecutar curl: {e}"))?;

    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("no se pudo descargar {url}"));
    }
    std::fs::rename(&partial, target).map_err(|e| format!("no se pudo guardar: {e}"))?;
    let _ = std::io::stdout().flush();
    Ok(())
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
}
