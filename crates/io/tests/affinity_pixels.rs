//! Original synthetic documents; Affinity itself never opens these test builders.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use photocraft_affinity::{
    Limits,
    model::{Kind, Pixels},
    synth::{self, F, Method, tag},
};
use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use photocraft_color::{PixelFormat, SampleType};
use photocraft_doc::LayerContent;
use photocraft_geom::Rect;
use photocraft_io::{ExportOptions, export, import};

fn document(node: F, entries: &[(String, Vec<u8>)], method: Method) -> Vec<u8> {
    let spread = F::Def(
        2,
        vec![tag(b"Sprd")],
        vec![(tag(b"SprB"), F::F64s(vec![0.0, 0.0, 300.0, 2.0])), (tag(b"SprT"), F::Bool(true)), (tag(b"Chld"), F::Shared(vec![node]))],
    );
    let doc = synth::stream(&[(tag(b"DocR"), F::Def(1, vec![tag(b"DocN")], vec![(tag(b"Chld"), F::Shared(vec![spread]))]))]);
    let mut all = vec![("doc.dat", doc.as_slice(), method)];
    all.extend(entries.iter().map(|(n, b)| (n.as_str(), b.as_slice(), method)));
    synth::container(&all, None)
}

fn rgba8() -> Image {
    Image::from_u8(2, 1, ChannelLayout::Rgba, vec![13, 79, 201, 255, 180, 27, 91, 0]).unwrap()
}

fn backed_node(bits: u8, states: &[u8], width: u32) -> F {
    let mut bitmap = vec![
        (tag(b"BmpW"), F::I32(width as i32)),
        (tag(b"BmpH"), F::I32(1)),
        (tag(b"Frmt"), F::Enum(u16::from(bits == 16), 0)),
        (tag(b"Bckg"), F::Entry("c/1".into())),
    ];
    for c in b'1'..=b'4' {
        bitmap.extend([
            (tag(&[b'T', b'W', b'i', c]), F::I32((width * u32::from(bits) / 8).div_ceil(256) as i32)),
            (tag(&[b'T', b'H', b'i', c]), F::I32(1)),
            (tag(&[b'S', b't', b'a', c]), F::U8s(states.to_vec())),
        ]);
    }
    F::Def(3, vec![tag(b"Rstr")], vec![(tag(b"Desc"), F::Str("Source pixels".into())), (tag(b"Bitm"), F::Def(4, vec![tag(b"DyBm")], bitmap))])
}

fn backing(image: &Image) -> Vec<u8> {
    let png = photocraft_codecs::encode(image, Format::Png, &EncodeOptions::default()).unwrap();
    let mut stream = synth::stream(&[(tag(b"Data"), F::Blob(png))]);
    stream[6..10].copy_from_slice(&tag(b"Blck").0.to_le_bytes());
    stream
}

fn raster_bytes(result: &photocraft_io::ImportResult, rect: Rect) -> Vec<u8> {
    let LayerContent::Raster(s) = &result.document.layers[0].content else { panic!("must remain editable pixels") };
    s.to_interleaved(rect)
}

#[test]
fn source_backed_pixels_stay_raster_and_keep_rgba_and_save_protection() {
    for method in [Method::Stored, Method::Zlib, Method::Zstd] {
        let src = rgba8();
        let bytes = document(backed_node(8, &[5], 2), &[("c/1".into(), backing(&src))], method);
        let native = photocraft_affinity::read(&bytes, Limits::default()).unwrap();
        assert!(matches!(&native.spreads[0].nodes[0].kind, Kind::Image(i) if matches!(i.pixels, Pixels::RasterFile { bits_per_channel: 8, .. })));
        let result = import("pixels.af", &bytes).unwrap();
        assert!(result.source_read_only && !result.preview_only);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(result.document.depth, SampleType::U8);
        assert_eq!(raster_bytes(&result, Rect::new(0, 0, 2, 1)), src.data());
        for extension in ["pcraft", "psd"] {
            let saved = export(&result.document, &format!("copy.{extension}"), &ExportOptions::default()).unwrap();
            let back = import(&format!("copy.{extension}"), &saved.bytes).unwrap();
            assert_eq!(raster_bytes(&back, Rect::new(0, 0, 2, 1)), src.data());
        }
        assert!(export(&result.document, "copy.af", &ExportOptions::default()).is_err());
    }
}

#[test]
fn source_backed_16_bit_precision_survives_import_and_native_and_psd_saves() {
    let values = [10001, 20002, 30003, 65535, 10002, 20003, 30004, 32769];
    let src = Image::from_u16(2, 1, ChannelLayout::Rgba, &values).unwrap();
    for method in [Method::Stored, Method::Zlib, Method::Zstd] {
        let result = import("precision.af", &document(backed_node(16, &[5], 2), &[("c/1".into(), backing(&src))], method)).unwrap();
        assert_eq!(result.document.depth, SampleType::U16);
        assert_eq!(raster_bytes(&result, Rect::new(0, 0, 2, 1)), src.data());
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        for extension in ["pcraft", "psd"] {
            let saved = export(&result.document, &format!("copy.{extension}"), &ExportOptions::default()).unwrap();
            let back = import(&format!("copy.{extension}"), &saved.bytes).unwrap();
            assert_eq!(back.document.depth, SampleType::U16);
            assert_eq!(raster_bytes(&back, Rect::new(0, 0, 2, 1)), src.data());
        }
    }
}

#[test]
fn mixed_or_incomplete_grids_never_restore_the_unedited_original() {
    for (states, width) in [(vec![5, 1], 257), (vec![5, 2], 257), (vec![5, 0], 257), (vec![5], 257), (vec![], 2)] {
        let bytes = document(backed_node(8, &states, width), &[("c/1".into(), backing(&rgba8()))], Method::Zlib);
        let native = photocraft_affinity::read(&bytes, Limits::default()).unwrap();
        assert!(matches!(&native.spreads[0].nodes[0].kind,Kind::Image(i) if matches!(i.pixels,Pixels::Rgba8(_))));
    }
}

#[test]
fn missing_corrupt_or_wrong_size_originals_warn_without_panicking() {
    let node = backed_node(8, &[5], 2);
    for entries in [vec![], vec![("c/1".into(), vec![1, 2, 3])], vec![("c/1".into(), synth::stream(&[(tag(b"Data"), F::Blob(vec![1, 2, 3]))]))]] {
        let result = import("broken.af", &document(node.clone(), &entries, Method::Zstd)).unwrap();
        assert!(!result.preview_only && !result.warnings.is_empty());
    }
    let result = import("wrong-size.af", &document(backed_node(8, &[5], 3), &[("c/1".into(), backing(&rgba8()))], Method::Stored)).unwrap();
    assert!(result.document.layers.is_empty());
    assert!(result.warnings.iter().any(|w| w.contains("different dimensions")));
}

/// 130 samples cross the 256-byte tile boundary in a 16-bit channel.
fn tiled_node(mask: bool, entries: &mut Vec<(String, Vec<u8>)>) -> F {
    let count = if mask { 1 } else { 4 };
    let mut bitmap = vec![(tag(b"BmpW"), F::I32(130)), (tag(b"BmpH"), F::I32(1)), (tag(b"Frmt"), F::Enum(if mask { 7 } else { 1 }, 0))];
    for c in 1..=count {
        let mut tiles = vec![];
        for t in 0..2 {
            let name = format!("d/{c}/{t}");
            let mut data = vec![0; 65536];
            for x in 0..128 {
                let value = if c == 4 { 65535 } else { 10000 * c + 128 * t + x + 1 };
                data[x as usize * 2..x as usize * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes());
            }
            entries.push((name.clone(), data));
            tiles.push(F::Def(20 + c * 2 + t, vec![tag(b"Blck")], vec![(tag(b"Data"), F::Entry(name))]));
        }
        let ch = b'0' + c as u8;
        bitmap.extend([
            (tag(&[b'T', b'W', b'i', ch]), F::I32(2)),
            (tag(&[b'T', b'H', b'i', ch]), F::I32(1)),
            (tag(&[b'S', b't', b'a', ch]), F::U8s(vec![4, 4])),
            (tag(&[b'I', b'd', b'x', ch]), F::Shared(tiles)),
        ]);
    }
    F::Def(3, vec![tag(if mask { b"MRst" } else { b"Rstr" })], vec![(tag(b"Bitm"), F::Def(4, vec![tag(b"DyBm")], bitmap))])
}

#[test]
fn tiled_16_bit_pixels_keep_single_step_precision_across_byte_tiles() {
    for method in [Method::Stored, Method::Zlib, Method::Zstd] {
        let mut entries = vec![];
        let node = tiled_node(false, &mut entries);
        let result = import("tiles.af", &document(node, &entries, method)).unwrap();
        assert_eq!(result.document.depth, SampleType::U16);
        let expected: Vec<u8> = (0..130).flat_map(|x| [10001 + x, 20001 + x, 30001 + x, 65535]).flat_map(u16::to_ne_bytes).collect();
        assert_eq!(raster_bytes(&result, Rect::new(0, 0, 130, 1)), expected);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }
}

#[test]
fn sixteen_bit_pixel_masks_promote_document_depth_and_keep_coverage_precision() {
    let mut entries = vec![];
    let mask = tiled_node(true, &mut entries);
    let group = F::Def(10, vec![tag(b"Grup")], vec![(tag(b"Chld"), F::Shared(vec![mask]))]);
    let result = import("mask.af", &document(group, &entries, Method::Zlib)).unwrap();
    assert_eq!(result.document.depth, SampleType::U16);
    let mask = result.document.layers[0].mask.as_ref().unwrap();
    assert_eq!(mask.surface.format(), PixelFormat::new(photocraft_color::ColorMode::Grayscale, SampleType::U16, false));
    let expected: Vec<u8> = (10001u16..10131).flat_map(u16::to_ne_bytes).collect();
    assert_eq!(mask.surface.to_interleaved(Rect::new(0, 0, 130, 1)), expected);
}

#[test]
fn transformed_16_bit_pixels_interpolate_premultiplied_alpha_without_colour_bleed() {
    let values = [10001, 20002, 30003, 65535, 50001, 60002, 40003, 0];
    let src = Image::from_u16(2, 1, ChannelLayout::Rgba, &values).unwrap();
    let mut node = backed_node(16, &[5], 2);
    if let F::Def(_, _, fields) = &mut node {
        fields.push((tag(b"Xfrm"), F::F64s(vec![1.0, 0.0, 0.5, 0.0, 1.0, 0.0])));
    }
    let result = import("shift.af", &document(node, &[("c/1".into(), backing(&src))], Method::Zlib)).unwrap();
    let expected: Vec<u8> = [10001u16, 20002, 30003, 32768, 10001, 20002, 30003, 32768, 0, 0, 0, 0].into_iter().flat_map(u16::to_ne_bytes).collect();
    assert_eq!(raster_bytes(&result, Rect::new(0, 0, 3, 1)), expected);
}

#[test]
fn mixed_source_and_edited_tiles_keep_edits_and_still_report_the_unsupported_tiles() {
    let mut entries = vec![];
    let mut node = tiled_node(false, &mut entries);
    if let F::Def(_, _, fields) = &mut node
        && let Some((_, F::Def(_, _, bitmap))) = fields.iter_mut().find(|(t, _)| *t == tag(b"Bitm"))
    {
        bitmap.push((tag(b"Bckg"), F::Entry("c/1".into())));
        *bitmap.iter_mut().find(|(t, _)| *t == tag(b"Sta1")).unwrap() = (tag(b"Sta1"), F::U8s(vec![5, 4]));
        if let Some((_, F::Shared(tiles))) = bitmap.iter_mut().find(|(t, _)| *t == tag(b"Idx1")) {
            tiles.remove(0);
        }
    }
    entries.push(("c/1".into(), backing(&rgba8())));
    let result = import("edited.af", &document(node, &entries, Method::Zlib)).unwrap();
    let surface = result.document.layers[0].surface().unwrap();
    let expected: Vec<u8> = [10129u16, 20129, 30129, 65535].into_iter().flat_map(u16::to_ne_bytes).collect();
    assert_eq!(surface.to_interleaved(Rect::new(128, 0, 129, 1)), expected);
    assert!(result.warnings.iter().any(|w| w.contains("drawn from an embedded image")));
}

#[test]
fn a_backing_file_with_a_different_model_or_depth_is_warned_and_left_out() {
    let source = Image::from_u16(2, 1, ChannelLayout::Rgba, &[1, 2, 3, 65535, 4, 5, 6, 65535]).unwrap();
    let result = import("mismatch.af", &document(backed_node(8, &[5], 2), &[("c/1".into(), backing(&source))], Method::Zlib)).unwrap();
    assert!(result.document.layers.is_empty());
    assert!(result.warnings.iter().any(|w| w.contains("different colour model or depth")));
}

#[test]
fn a_source_backing_profile_is_reported_instead_of_claiming_colour_fidelity() {
    let source = rgba8().with_icc(Some(photocraft_cms::Builtin::Srgb.profile().to_bytes().to_vec()));
    let result = import("profile.af", &document(backed_node(8, &[5], 2), &[("c/1".into(), backing(&source))], Method::Zlib)).unwrap();
    assert_eq!(raster_bytes(&result, Rect::new(0, 0, 2, 1)), source.data());
    assert!(result.warnings.iter().any(|w| w.contains("embedded colour profile")));
}

#[test]
fn an_eight_bit_layer_in_a_document_with_a_sixteen_bit_mask_is_promoted_exactly() {
    let mut entries = vec![];
    let mask = tiled_node(true, &mut entries);
    let mut image = backed_node(8, &[5], 2);
    if let F::Def(id, _, fields) = &mut image {
        *id = 30;
        if let Some((_, F::Def(bitmap_id, _, _))) = fields.iter_mut().find(|(t, _)| *t == tag(b"Bitm")) {
            *bitmap_id = 31;
        }
    }
    let group = F::Def(10, vec![tag(b"Grup")], vec![(tag(b"Chld"), F::Shared(vec![image, mask]))]);
    let source = rgba8();
    entries.push(("c/1".into(), backing(&source)));
    let result = import("mixed.af", &document(group, &entries, Method::Zlib)).unwrap();
    assert_eq!(result.document.depth, SampleType::U16);
    let LayerContent::Group(group) = &result.document.layers[0].content else { panic!() };
    let surface = group.children[0].surface().unwrap();
    assert_eq!(surface.format(), PixelFormat::RGBA16);
    let expected: Vec<u8> = source.data().iter().flat_map(|v| (u16::from(*v) * 257).to_ne_bytes()).collect();
    assert_eq!(surface.to_interleaved(Rect::new(0, 0, 2, 1)), expected);
}

#[test]
fn a_placed_image_still_imports_as_an_embedded_smart_object() {
    let mut node = backed_node(8, &[5], 2);
    if let F::Def(_, classes, _) = &mut node {
        *classes = vec![tag(b"ImgN")];
    }
    let result = import("placed.af", &document(node, &[("c/1".into(), backing(&rgba8()))], Method::Zlib)).unwrap();
    assert!(matches!(&result.document.layers[0].content, LayerContent::Smart(_)));
    assert!(result.warnings.is_empty());
}
