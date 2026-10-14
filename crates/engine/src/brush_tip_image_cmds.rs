//! `brush.defineFromImage`: an image file (PNG, JPEG, TIFF, WebP …) becomes a sampled brush tip,
//! the file counterpart of `brush.defineFromSelection` (Edit › Define Brush Preset…).
//!
//! The file is read from `path` or from base64 `data` (like `brush.presets.importAbr`), decoded
//! at any bit depth and colour model, turned into a paint amount per pixel, cropped to the part
//! that has paint and shrunk to the largest tip size if it is bigger. The result is saved as a
//! brush preset and becomes the current brush.

use photocraft_paint::BrushPreset;
use serde_json::{Value, json};

use crate::brush_cmds::{sampled_tip_brush, upsert};
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// Largest image (in pixels) read as a tip: a tip is at most `MAX_TIP` px on a side, so a larger
/// picture is not a brush, and decoding it to floats would cost gigabytes.
const MAX_PIXELS: u64 = 16 << 20;

/// What of each pixel becomes paint.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Source {
    /// Dark pixels paint, white does not, scaled by alpha: black on white (or on transparent)
    /// is a solid tip. What Define Brush Preset does with a selection.
    Darkness,
    /// Light pixels paint: white shapes on black.
    Lightness,
    /// Only the alpha channel: a cut-out of any colour on transparent.
    Alpha,
}

impl Source {
    fn parse(p: &Value, cmd: &str) -> Result<Self> {
        let Some(v) = p.get("source") else { return Ok(Source::Darkness) };
        match v.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("darkness") => Ok(Source::Darkness),
            Some("lightness") => Ok(Source::Lightness),
            Some("alpha") => Ok(Source::Alpha),
            _ => Err(bad(cmd, "`source` must be \"darkness\", \"lightness\" or \"alpha\"")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Source::Darkness => "darkness",
            Source::Lightness => "lightness",
            Source::Alpha => "alpha",
        }
    }

    /// Paint amount of one RGBA pixel, `0..=1` (anything not finite paints nothing).
    fn amount(self, px: &[f32; 4]) -> f32 {
        let [r, g, b, a] = *px;
        let lum = photocraft_color::blend::lum([r, g, b]);
        let v = match self {
            Source::Darkness => (1.0 - lum) * a,
            Source::Lightness => lum * a,
            Source::Alpha => a,
        };
        if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
    }
}

fn define_from_image(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "brush.defineFromImage";
    let (bytes, stem) = crate::preset_import_cmds::file_bytes(p, cmd, "image file")?;
    let source = Source::parse(p, cmd)?;
    let name = match p.get("name") {
        Some(v) => v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(cmd, "`name` must be a non-empty string"))?.to_string(),
        None if !stem.trim().is_empty() => stem.trim().to_string(),
        None => "Image Tip".to_string(),
    };
    let img = photocraft_codecs::decode(&bytes).map_err(|e| bad(cmd, format!("not an image PhotoCraft can read: {e}")))?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(bad(cmd, format!("the image is {w}×{h} px; a brush tip image can hold at most {MAX_PIXELS} pixels")));
    }
    let rgba = img.to_rgba_f32();
    let v: Vec<f32> = rgba.as_chunks::<4>().0.iter().map(|px| source.amount(px)).collect();
    let (brush, tw, th) = sampled_tip_brush(cmd, &v, w as usize, h as usize, true)?;
    upsert(s, BrushPreset { name: name.clone(), brush: brush.clone(), builtin: false, group: String::new(), folder: Vec::new(), captures: Default::default() });
    s.tools.brush = brush;
    s.tools.current_preset = Some(name.clone());
    Ok(json!({ "name": name, "width": tw, "height": th, "source": source.name() }))
}

/// Brush-tip-from-image command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "brush.defineFromImage",
        label: "Import Image as Brush Tip…",
        menu: &[],
        shortcut: None,
        params: r##"{"path":image file?,"data":base64 bytes?,"name":string?=file name,"source":"darkness|lightness|alpha"="darkness" (what paints: dark pixels, light pixels or the alpha channel; always scaled by alpha except `alpha`)} → {name, width, height, source}. Saves a brush preset (a same-named one is replaced) and makes it the current brush; the tip is cropped to its paint and shrunk to 2500 px"##,
        enabled: always,
        run: define_from_image,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_codecs::{ChannelLayout, Format, Image};
    use photocraft_paint::TipShape;

    /// A PNG, `w` × `h`, of `px(x, y)` as RGBA 8-bit.
    fn png(w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut data = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&px(x, y));
            }
        }
        let img = Image::from_u8(w, h, ChannelLayout::Rgba, data).unwrap();
        photocraft_codecs::encode(&img, Format::Png, &Default::default()).unwrap()
    }

    fn b64(bytes: &[u8]) -> String {
        photocraft_paint::tile::b64_encode(bytes)
    }

    fn tip(s: &Session) -> &photocraft_paint::GrayTile {
        match &s.tools.brush.tip {
            TipShape::Sampled(t) => t,
            _ => panic!("the brush is not a sampled tip"),
        }
    }

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    /// A black 6 × 4 block at (5, 3) on white.
    fn block(x: u32, y: u32) -> [u8; 4] {
        if (5..11).contains(&x) && (3..7).contains(&y) { BLACK } else { WHITE }
    }

    #[test]
    fn a_dark_shape_on_white_becomes_a_cropped_tip() {
        let mut s = Session::new();
        let r = s.execute("brush.defineFromImage", json!({"data": b64(&png(20, 12, block)), "name": "Block"})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(6), Some(4)));
        assert_eq!(r["name"], "Block");
        assert_eq!(r["source"], "darkness");
        assert_eq!((tip(&s).width, tip(&s).height), (6, 4));
        assert_eq!(tip(&s).get(0, 0), 1.0);
        assert_eq!(s.tools.brush.size, 6.0);
        assert_eq!(s.tools.current_preset.as_deref(), Some("Block"));
        assert!(s.tools.presets.iter().any(|pr| pr.name == "Block" && !pr.builtin));
        // Without a document: the library and the session brush are all it touches.
        assert!(s.active().is_none());
    }

    #[test]
    fn the_source_picks_what_paints() {
        // White block on black, and the same block cut out of a transparent image in red.
        let white_on_black = png(20, 12, |x, y| if block(x, y) == BLACK { WHITE } else { BLACK });
        let red_cutout = png(20, 12, |x, y| if block(x, y) == BLACK { [255, 0, 0, 255] } else { [0, 0, 0, 0] });
        let mut s = Session::new();
        let r = s.execute("brush.defineFromImage", json!({"data": b64(&white_on_black), "name": "L", "source": "lightness"})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(6), Some(4)));
        let r = s.execute("brush.defineFromImage", json!({"data": b64(&red_cutout), "name": "A", "source": "alpha"})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(6), Some(4)));
        assert_eq!(tip(&s).get(0, 0), 1.0);
        // Darkness of that red (luminance ≈ 0.3) paints about 0.7, not fully.
        s.execute("brush.defineFromImage", json!({"data": b64(&red_cutout), "name": "D"})).unwrap();
        assert!((0.5..0.9).contains(&tip(&s).get(0, 0)), "{}", tip(&s).get(0, 0));
    }

    #[test]
    fn soft_edges_keep_their_gradation() {
        // A horizontal ramp from black to white: a tip that fades out to the right.
        let ramp = png(16, 4, |x, _| {
            let v = (x * 17) as u8;
            [v, v, v, 255]
        });
        let mut s = Session::new();
        s.execute("brush.defineFromImage", json!({"data": b64(&ramp), "name": "Fade"})).unwrap();
        assert!(tip(&s).get(0, 0) > 0.95);
        assert!(tip(&s).get(14, 0) < 0.15 && tip(&s).get(14, 0) > 0.0, "{}", tip(&s).get(14, 0));
    }

    #[test]
    fn paints_with_the_imported_tip() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 40, "background": "transparent"})).unwrap();
        s.execute("brush.defineFromImage", json!({"data": b64(&png(20, 12, block)), "name": "Block"})).unwrap();
        s.execute("paint.stroke", json!({"points": [[20, 20]], "color": "#ff0000"})).unwrap();
        let d = s.active().unwrap();
        let surf = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap();
        assert!(surf.rgba(20, 20)[3] > 0.9);
        assert_eq!(surf.rgba(2, 2)[3], 0.0);
    }

    #[test]
    fn a_big_image_is_shrunk_to_the_largest_tip() {
        let mut s = Session::new();
        // 5000 × 20 black bar: the longest side shrinks to 2500, the proportions stay.
        let r = s.execute("brush.defineFromImage", json!({"data": b64(&png(5000, 20, |_, _| BLACK)), "name": "Bar"})).unwrap();
        assert_eq!(r["width"].as_u64(), Some(u64::from(crate::brush_cmds::MAX_TIP)));
        assert_eq!(r["height"].as_u64(), Some(10));
        assert!(tip(&s).get(100, 5) > 0.99);
    }

    #[test]
    fn depth_and_colour_model_do_not_matter() {
        // 16-bit grey, and 8-bit RGB without alpha.
        let grey16 = Image::from_u16(8, 8, ChannelLayout::Gray, &(0..64).map(|i| if (2..6).contains(&(i % 8)) && (2..6).contains(&(i / 8)) { 0 } else { u16::MAX }).collect::<Vec<_>>()).unwrap();
        let rgb = Image::from_u8(8, 8, ChannelLayout::Rgb, (0..64).flat_map(|i| if i == 27 { [0, 0, 0] } else { [255, 255, 255] }).collect()).unwrap();
        for (img, size) in [(grey16, 4), (rgb, 1)] {
            let bytes = photocraft_codecs::encode(&img, Format::Png, &Default::default()).unwrap();
            let mut s = Session::new();
            let r = s.execute("brush.defineFromImage", json!({"data": b64(&bytes), "name": "T"})).unwrap();
            assert_eq!(r["width"].as_u64(), Some(size), "{:?}", img.layout());
        }
    }

    #[test]
    fn a_file_path_names_the_preset_and_a_name_wins() {
        let dir = std::env::temp_dir().join(format!("photocraft-tip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Splatter.png");
        std::fs::write(&path, png(20, 12, block)).unwrap();
        let mut s = Session::new();
        let r = s.execute("brush.defineFromImage", json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(r["name"], "Splatter");
        let r = s.execute("brush.defineFromImage", json!({"path": path.to_string_lossy(), "name": "  Mine "})).unwrap();
        assert_eq!(r["name"], "Mine");
        // A preset of the same name is replaced, not duplicated.
        s.execute("brush.defineFromImage", json!({"path": path.to_string_lossy(), "name": "mine"})).unwrap();
        assert_eq!(s.tools.presets.iter().filter(|pr| pr.name.eq_ignore_ascii_case("mine")).count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bad_input_is_an_error_not_a_crash() {
        let mut s = Session::new();
        let before = s.tools.presets.len();
        let white = b64(&png(8, 8, |_, _| WHITE));
        let clear = b64(&png(8, 8, |_, _| [0, 0, 0, 0]));
        let ok = b64(&png(8, 8, |_, _| BLACK));
        for p in [
            json!({}),
            json!({"data": 5}),
            json!({"data": "***not base64***"}),
            json!({"data": b64(b"definitely not an image")}),
            json!({"data": b64(b"")}),
            json!({"path": ""}),
            json!({"path": "/no/such/file.png"}),
            json!({"data": white}),
            json!({"data": clear}),
            json!({"data": ok, "name": ""}),
            json!({"data": ok, "name": 7}),
            json!({"data": ok, "source": "hue"}),
            json!({"data": ok, "source": 3}),
            json!({"data": clear, "source": "lightness"}),
        ] {
            assert!(s.execute("brush.defineFromImage", p.clone()).is_err(), "{p}");
        }
        assert_eq!(s.tools.presets.len(), before, "a failed import leaves the library alone");
        assert!(s.tools.current_preset.is_none());
        // The source name is matched without regard to case.
        assert!(s.execute("brush.defineFromImage", json!({"data": ok, "source": "ALPHA", "name": "x"})).is_ok(), "the source name is case-insensitive");
    }

    #[test]
    fn shrinking_averages_and_never_indexes_out_of_range() {
        let (out, w, h) = crate::brush_cmds::shrink_to(&[0.0, 1.0, 0.0, 1.0], 4, 1, 2).unwrap();
        assert_eq!((w, h, out.as_slice()), (2, 1, &[0.5, 0.5][..]));
        // Not smaller, empty or mismatched: nothing to do.
        assert!(crate::brush_cmds::shrink_to(&[1.0; 4], 2, 2, 2).is_none());
        assert!(crate::brush_cmds::shrink_to(&[1.0; 3], 4, 1, 2).is_none());
        assert!(crate::brush_cmds::shrink_to(&[], 0, 0, 2).is_none());
        // A one-pixel-high strip never shrinks to zero rows.
        let (out, w, h) = crate::brush_cmds::shrink_to(&vec![1.0; 10_000], 10_000, 1, 100).unwrap();
        assert_eq!((w, h, out.len()), (100, 1, 100));
    }
}
