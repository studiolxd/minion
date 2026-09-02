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

use anyhow::{anyhow, Result};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};
use tray_icon::Icon;

const AWAKE: &str = include_str!("../assets/awake.svg");
const ASLEEP: &str = include_str!("../assets/asleep.svg");

/// Drawn at twice the menu bar's 22 points, for Retina displays.
const SIZE: u32 = 44;

/// Renders one of the faces into an icon the tray can show.
fn render(svg: &str) -> Result<Icon> {
    let tree = Tree::from_str(svg, &Options::default())
        .map_err(|e| anyhow!("the icon will not parse: {e}"))?;

    let mut pixmap = Pixmap::new(SIZE, SIZE).ok_or_else(|| anyhow!("no room for the icon"))?;
    let size = tree.size();
    let scale = SIZE as f32 / size.width().max(size.height());
    resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());

    Icon::from_rgba(pixmap.take(), SIZE, SIZE)
        .map_err(|e| anyhow!("the icon will not load: {e}"))
}

pub fn awake() -> Result<Icon> {
    render(AWAKE)
}

pub fn asleep() -> Result<Icon> {
    render(ASLEEP)
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
            let margin = pixels as f32 * 0.08;
            let drawn = pixels as f32 - margin * 2.0;
            let factor = drawn / tree.size().width();
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
    fn both_faces_render() {
        assert!(awake().is_ok(), "the awake face should draw");
        assert!(asleep().is_ok(), "the sleeping face should draw");
    }

    #[test]
    fn the_faces_differ() {
        // Same head, different eye and mouth: the drawings must not be
        // identical, or pausing would show no change at all.
        let draw = |svg| {
            let tree = Tree::from_str(svg, &Options::default()).unwrap();
            let mut pixmap = Pixmap::new(SIZE, SIZE).unwrap();
            let scale = SIZE as f32 / tree.size().width();
            resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            pixmap.take()
        };
        assert_ne!(draw(AWAKE), draw(ASLEEP));
    }
}
