//! The face in the menu bar.
//!
//! Two states of the same character, drawn as SVG and rasterised at
//! startup: awake, with the eye open and a smile; asleep, with the eye shut
//! and the smile turned down. Same head in both, so it reads as one thing
//! changing rather than two different icons.
//!
//! Drawn rather than shipped as a bitmap because the menu bar wants it at
//! whatever scale the display uses, and because the two states then differ
//! by three lines of SVG instead of two image files.

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};
use tray_icon::Icon;

const AWAKE: &str = include_str!("../assets/awake.svg");
const ASLEEP: &str = include_str!("../assets/asleep.svg");
const ACTING: &str = include_str!("../assets/acting.svg");

/// Height in pixels: twice the menu bar's usable height, for Retina.
///
/// The width follows from the drawing's proportions rather than being
/// forced square — a square canvas would leave the face floating in empty
/// space and looking smaller than the room it takes up.
const HEIGHT: u32 = 40;

/// Renders one of the faces into an icon the tray can show.
///
/// The drawing is black on transparent, which is what a macOS template
/// image wants: the system reads the alpha and tints it — black on a light
/// menu bar, white on a dark one — so the icon follows the theme without
/// needing two versions.
fn render(svg: &str) -> Result<Icon> {
    let tree = Tree::from_str(svg, &Options::default())
        .map_err(|e| anyhow!("the icon will not parse: {e}"))?;

    let size = tree.size();
    let scale = HEIGHT as f32 / size.height();
    let width = (size.width() * scale).round().max(1.0) as u32;

    let mut pixmap = Pixmap::new(width, HEIGHT).ok_or_else(|| anyhow!("no room for the icon"))?;
    resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());

    Icon::from_rgba(pixmap.take(), width, HEIGHT)
        .map_err(|e| anyhow!("the icon will not load: {e}"))
}

/// Renders a face once and hands out copies of it afterwards.
///
/// The menu bar asks for a face on every state change — and the blink that
/// acknowledges a command is three changes in half a second. Parsing the
/// SVG and rasterising it each time is real work for a drawing that never
/// changes; the copy is a few kilobytes of pixels. On macOS an `Icon` is
/// just those pixels, so this is safe to keep in a static.
fn cached(slot: &'static OnceLock<Option<Icon>>, svg: &'static str) -> Result<Icon> {
    slot.get_or_init(|| render(svg).ok())
        .clone()
        .ok_or_else(|| anyhow!("the icon will not draw"))
}

pub fn awake() -> Result<Icon> {
    static AWAKE_ICON: OnceLock<Option<Icon>> = OnceLock::new();
    cached(&AWAKE_ICON, AWAKE)
}

pub fn asleep() -> Result<Icon> {
    static ASLEEP_ICON: OnceLock<Option<Icon>> = OnceLock::new();
    cached(&ASLEEP_ICON, ASLEEP)
}

/// Shown briefly when a command runs.
///
/// A command that works produces no output of its own, so without some
/// acknowledgement there is no telling whether you were heard. The sounds
/// did that job and can be turned off; this does it silently.
pub fn acting() -> Result<Icon> {
    static ACTING_ICON: OnceLock<Option<Icon>> = OnceLock::new();
    cached(&ACTING_ICON, ACTING)
}

/// Writes the app icon at every size macOS asks for.
///
/// Generated from the same drawing as the menu bar face, so the two cannot
/// drift apart. Called by build-app.sh, which turns the result into .icns.
pub fn export_iconset(directory: &str) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    let tree = Tree::from_str(AWAKE, &Options::default())
        .map_err(|e| anyhow!("the icon will not parse: {e}"))?;

    // The sizes macOS expects in an iconset, each also at 2×.
    for size in [16u32, 32, 128, 256, 512] {
        for (scale, suffix) in [(1u32, String::new()), (2, "@2x".to_string())] {
            let pixels = size * scale;
            let mut pixmap =
                Pixmap::new(pixels, pixels).ok_or_else(|| anyhow!("no room for the icon"))?;
            // A little breathing room, or the drawing touches the edges.
            let margin = pixels as f32 * 0.12;
            let drawn = pixels as f32 - margin * 2.0;
            let factor = drawn / tree.size().width().max(tree.size().height());
            let transform = Transform::from_translate(margin, margin)
                .pre_scale(factor, factor);
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            let path = format!("{directory}/icon_{size}x{size}{suffix}.png");
            pixmap
                .save_png(&path)
                .map_err(|e| anyhow!("cannot write {path}: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_renders() {
        assert!(awake().is_ok(), "the awake face should draw");
        assert!(asleep().is_ok(), "the sleeping face should draw");
        assert!(acting().is_ok(), "the acting face should draw");
    }

    #[test]
    fn asking_for_a_face_again_still_gives_one() {
        // The faces are rendered once and copied afterwards; a cache that
        // hands out nothing the second time would leave the menu bar stuck
        // on whichever face it happened to have.
        for _ in 0..5 {
            assert!(awake().is_ok());
            assert!(asleep().is_ok());
            assert!(acting().is_ok());
        }
    }

    fn draw(svg: &str) -> Vec<u8> {
        let tree = Tree::from_str(svg, &Options::default()).unwrap();
        let scale = HEIGHT as f32 / tree.size().height();
        let width = (tree.size().width() * scale).round() as u32;
        let mut pixmap = Pixmap::new(width, HEIGHT).unwrap();
        resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        pixmap.take()
    }

    #[test]
    fn the_faces_differ() {
        // Same head, different eye and mouth: the drawings must not be
        // identical, or a change of state would show nothing at all.
        assert_ne!(draw(AWAKE), draw(ASLEEP));
        assert_ne!(draw(AWAKE), draw(ACTING));
    }

    #[test]
    fn the_drawing_fills_the_canvas() {
        // Empty margins make the icon look smaller than the space it takes.
        // Check that ink reaches close to the top and bottom rows.
        let pixels = draw(AWAKE);
        let width = pixels.len() / 4 / HEIGHT as usize;
        let row_has_ink = |row: usize| {
            (0..width).any(|x| pixels[(row * width + x) * 4 + 3] > 16)
        };
        assert!(row_has_ink(1), "the drawing should reach the top");
        assert!(row_has_ink(HEIGHT as usize - 2), "and the bottom");
    }

    #[test]
    fn the_drawing_is_black_on_transparent() {
        // A template image is tinted by macOS from its alpha channel; any
        // colour of its own would fight that.
        let pixels = draw(AWAKE);
        for chunk in pixels.chunks(4) {
            if chunk[3] > 200 {
                assert!(
                    chunk[0] < 40 && chunk[1] < 40 && chunk[2] < 40,
                    "solid pixels should be black, found {chunk:?}"
                );
            }
        }
    }
}
