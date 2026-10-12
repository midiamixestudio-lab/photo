//! Channel transforms preview through the normal compositor and current channel eyes.
//! The captured proxy is immutable; geometry is resampled only when it changes, never per frame.

use std::sync::Arc;

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use photocraft_doc::{Document, LayerId};
use photocraft_engine::channel_cmds::ChannelView;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::TransformSession;

pub(crate) struct Preview {
    base: Arc<Document>,
    layer: Option<LayerId>,
    background: [f32; 4],
    factor: u32,
    geometry: Value,
    shown: Arc<Document>,
    view: Option<(ChannelView, bool, u64)>,
    texture: Option<TextureHandle>,
}

impl Preview {
    pub(crate) fn new(doc: &Document, layer: Option<LayerId>, background: [f32; 4], reduced: bool) -> Self {
        // Effects and vector masks need document-space geometry, so retain full resolution there.
        let factor = if photocraft_compose::proxy::proxy_faithful(doc) { crate::proxy::preview_factor(doc, reduced) } else { 1 };
        let mut base = crate::proxy::proxy_document(doc, factor);
        if factor > 1
            && let Some(mask) = base.quick_mask.as_mut()
        {
            mask.surface = crate::proxy::downsample(&mask.surface, factor);
        }
        let base = Arc::new(base);
        Self { shown: base.clone(), base, layer, background, factor, geometry: Value::Null, view: None, texture: None }
    }

    pub(crate) fn background(&self) -> [f32; 4] {
        self.background
    }

    fn update(&mut self, t: &TransformSession) -> Result<(), String> {
        let k = f64::from(self.factor);
        let mut params = json!({"target": t.target, "rect": t.rect.map(|v| v / k), "interpolation": t.interpolation});
        if let Some(layer) = self.layer {
            params["layer"] = json!(layer.0);
        }
        let command = if let Some(warp) = &t.warp {
            let mut warp = warp.clone();
            warp.bounds = warp.bounds.map(|v| v / k);
            warp.mesh = warp.mesh.map(|m| m.map_points(|p| [p[0] / k, p[1] / k]));
            params["warp"] = json!(warp);
            "edit.transform.warp"
        } else {
            params["quad"] = json!(t.quad.map(|p| p.map(|v| v / k)));
            "edit.transform"
        };
        if self.geometry == params {
            return Ok(());
        }
        let mut session = photocraft_engine::Session::new();
        session.tools.background = self.background;
        session.add_document((*self.base).clone(), None);
        if let Some(layer) = self.layer {
            session.select_layer(layer).map_err(|e| e.to_string())?;
        }
        session.execute(command, params.clone()).map_err(|e| e.to_string())?;
        self.shown = session.active().ok_or("no channel preview document")?.doc.clone();
        self.geometry = params;
        self.view = None;
        Ok(())
    }

    fn image(&self, view: &ChannelView, in_color: bool, display: Option<&photocraft_engine::display_color::CanvasDisplay>) -> ColorImage {
        let buffer = photocraft_compose::flatten(&self.shown);
        let mut image = crate::canvas::display_image(display, &buffer);
        let pixels = &mut image.pixels;
        if let Some(overlay) = crate::channel_view::render(&self.shown, view, self.shown.bounds(), 1, in_color, self.layer) {
            for (base, over) in pixels.iter_mut().zip(overlay) {
                let weight = u32::from(255 - over.a());
                let b = base.to_array();
                let o = over.to_array();
                let result = std::array::from_fn::<_, 4, _>(|i| (u32::from(o[i]) + (u32::from(b[i]) * weight + 127) / 255).min(255) as u8);
                *base = Color32::from_rgba_premultiplied(result[0], result[1], result[2], result[3]);
            }
        }
        image
    }
}

/// Keep uploads within the renderer's limit, including very long, narrow documents.
fn limit_image(image: ColorImage, max_side: usize) -> ColorImage {
    let k = image.size.into_iter().max().unwrap_or(1).div_ceil(max_side.max(1)).max(1);
    if k == 1 {
        return image;
    }
    let size = image.size.map(|v| v.div_ceil(k));
    let pixels = image.pixels.chunks(image.size[0].max(1)).step_by(k).flat_map(|row| row.iter().step_by(k).copied()).collect();
    ColorImage::new(size, pixels)
}

/// Current preview texture: the eyes affect rendering only, never the captured source or target.
pub(crate) fn texture(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize) -> Option<egui::TextureId> {
    if app.session.active_index() != Some(idx) {
        return None;
    }
    let (display, display_key) = crate::canvas::canvas_display(app, &app.session.documents().get(idx)?.doc, None);
    let t = app.ui.transform.as_ref()?;
    let pv = app.transform_preview.as_mut()?;
    let preview = pv.channel.as_mut()?;
    let st = app.session.documents().get(idx)?;
    if st.doc.id != pv.doc.id {
        return None;
    }
    if let Err(error) = preview.update(t) {
        app.ui.status = error;
        app.ui.status_error = true;
        return preview.texture.as_ref().map(TextureHandle::id);
    }
    let view = st.channel_view.clone();
    let in_color = app.session.prefs().interface.show_channels_in_color;
    if preview.view.as_ref() != Some(&(view.clone(), in_color, display_key)) {
        let image = limit_image(preview.image(&view, in_color, display.as_deref()), ctx.input(|i| i.max_texture_side));
        match preview.texture.as_mut() {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => preview.texture = Some(ctx.load_texture(format!("channel-transform-{}", t.session), image, TextureOptions::LINEAR)),
        }
        preview.view = Some((view, in_color, display_key));
    }
    preview.texture.as_ref().map(TextureHandle::id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{Color, ColorMode, SampleType};
    use photocraft_doc::{LayerMask, Size};
    use photocraft_engine::{Session, channel_cmds::ChannelTarget};
    use photocraft_geom::{
        Rect,
        warp::{BezierMesh, Warp},
    };

    fn app() -> PhotocraftApp {
        let mut session = Session::new();
        let mut doc = Document::with_background("channel preview", Size::new(32, 24), ColorMode::Rgb, SampleType::F32, Color::WHITE);
        let s = doc.layers[0].surface_mut().unwrap();
        s.fill_rect(Rect::new(0, 0, 32, 24), &[0.2, 0.4, 0.6, 0.75]);
        s.fill_rect(Rect::new(4, 4, 16, 16), &[0.8, 0.1, 0.3, 0.5]);
        session.add_document(doc, None);
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.sync_views();
        app
    }

    #[test]
    fn narrow_preview_obeys_texture_limit() {
        let image = ColorImage::new([9000, 2], vec![Color32::WHITE; 18000]);
        let result = limit_image(image, 4096);
        assert_eq!(result.size, [3000, 1]);
        assert_eq!(result.pixels.len(), 3000);
    }

    #[test]
    fn visible_mask_overlay_never_overrides_the_channels_colour_target() {
        for warp in [false, true] {
            let mut a = app();
            a.run("layer.layerMask.revealAll", json!({})).unwrap();
            a.run("view.layerMask", json!({"mode":"overlay"})).unwrap();
            a.run("channel.target", json!({"channel":"green"})).unwrap();
            a.ui.mask_target = true; // A Layers thumbnail or a stale UI target must not win.
            let before = a.session.active().unwrap().doc.clone();
            let view = a.session.active().unwrap().channel_view.clone();
            let ctx = egui::Context::default();
            crate::transform_tool::begin(&mut a, &ctx).unwrap();
            assert_eq!(a.ui.transform.as_ref().unwrap().target, Some(json!({"color":1})));
            assert_eq!(a.session.active().unwrap().channel_view, view);
            let t = a.ui.transform.as_mut().unwrap();
            t.quad = t.quad.map(|[x, y]| [x + 3.0, y]);
            if warp {
                t.warp = Some(Warp::custom(BezierMesh::identity(t.rect, 1, 1).map_points(|p| [p[0] + 3.0, p[1]]), t.rect));
            }
            let t = a.ui.transform.clone().unwrap();
            let pv = a.transform_preview.as_mut().unwrap().channel.as_mut().unwrap();
            pv.update(&t).unwrap();
            let shown = pv.shown.clone();
            crate::transform_tool::commit(&mut a);
            assert!(!a.ui.status_error, "{}", a.ui.status);
            let after = &a.session.active().unwrap().doc;
            assert_eq!(after.layers[0].mask, before.layers[0].mask);
            assert_eq!(after.layers, shown.layers);
            assert_ne!(after.layers[0].surface(), before.layers[0].surface());
            for y in 0..24 {
                for x in 0..32 {
                    for k in [0, 2, 3] {
                        assert_eq!(after.layers[0].surface().unwrap().sample_channel(x, y, k), before.layers[0].surface().unwrap().sample_channel(x, y, k));
                    }
                }
            }
        }
    }

    #[test]
    fn inline_warp_grid_and_splits_work_for_every_captured_scalar_target() {
        for target in [json!("green"), json!("mask"), json!(0), json!("quickMask")] {
            let mut a = app();
            match target.as_str() {
                Some("mask") => {
                    a.run("layer.layerMask.revealAll", json!({})).unwrap();
                }
                Some("quickMask") => {
                    a.run("select.editInQuickMaskMode", json!({"on":true})).unwrap();
                }
                _ if target == 0 => {
                    a.run("channel.new", json!({})).unwrap();
                }
                _ => {}
            }
            if target != "quickMask" {
                a.run("channel.target", json!({"channel":target})).unwrap();
            }
            if target == "mask" {
                a.run("view.layerMask", json!({"mode":"overlay"})).unwrap();
            }
            if target == 0 {
                a.session.active_mut().unwrap().active_layer = None;
            }
            let before = a.session.active().unwrap().doc.clone();
            let ctx = egui::Context::default();
            crate::transform_tool::begin(&mut a, &ctx).unwrap();
            crate::transform_tool::enter_warp(&mut a, &ctx);
            let captured = a.ui.transform.as_ref().unwrap().target.clone();
            crate::transform_tool::edit_session_warp(&mut a, "edit.transform.warpGrid", &json!({"size":3})).unwrap();
            assert_eq!(a.ui.transform.as_ref().unwrap().warp.as_ref().unwrap().mesh.as_ref().unwrap().us.len(), 4);
            for id in [
                "edit.transform.splitWarpCrosswise",
                "edit.transform.splitWarpHorizontally",
                "edit.transform.splitWarpVertically",
                "edit.transform.removeWarpSplit",
            ] {
                crate::transform_tool::edit_session_warp(&mut a, id, &json!({})).unwrap();
                assert_eq!(a.ui.transform.as_ref().unwrap().target, captured);
                assert!(a.session.active().unwrap().channel_transform.is_some());
                assert!(Arc::ptr_eq(&before, &a.session.active().unwrap().doc));
            }
            crate::transform_tool::cancel(&mut a);
            assert!(a.session.active().unwrap().channel_transform.is_none());
        }
    }

    #[test]
    fn cmyk_preview_and_commit_share_the_embedded_background_conversion() {
        for warp in [false, true] {
            let mut session = Session::new();
            let mut doc = Document::with_background("CMYK preview", Size::new(32, 24), ColorMode::Cmyk, SampleType::F32, Color::WHITE);
            doc.icc_profile = Some(
                photocraft_cms::synth::cmyk_profile(&photocraft_cms::synth::CmykParams {
                    description: "Preview Uncoated".into(),
                    tvi: [0.26, 0.26, 0.26, 0.3],
                    grid_a2b: 5,
                    grid_b2a: 9,
                    ..Default::default()
                })
                .to_bytes(),
            );
            let bounds = doc.bounds();
            doc.layers[0].surface_mut().unwrap().fill_rect(bounds, &[0.1, 0.2, 0.3, 0.4, 1.0]);
            session.tools.background = [0.8, 0.2, 0.4, 1.0];
            let space = photocraft_compose::cmyk_space(&doc).unwrap();
            let expected =
                photocraft_color::convert::with_cmyk_space(Some(&space), || photocraft_raster::from_rgba(&doc.pixel_format(), session.tools.background))[1];
            session.add_document(doc, None);
            let mut a = PhotocraftApp::new(session, crate::Services::default());
            a.run("channel.target", json!({"channel":{"color":1}})).unwrap();
            crate::transform_tool::begin(&mut a, &egui::Context::default()).unwrap();
            let t = a.ui.transform.as_mut().unwrap();
            t.quad = t.quad.map(|[x, y]| [x + 3.0, y]);
            if warp {
                t.warp = Some(Warp::custom(BezierMesh::identity(t.rect, 1, 1).map_points(|p| [p[0] + 3.0, p[1]]), t.rect));
            }
            let t = a.ui.transform.clone().unwrap();
            let pv = a.transform_preview.as_mut().unwrap().channel.as_mut().unwrap();
            pv.update(&t).unwrap();
            let shown = pv.shown.clone();
            assert!((shown.layers[0].surface().unwrap().sample_channel(0, 0, 1) - expected).abs() < 1e-5);
            crate::transform_tool::commit(&mut a);
            assert!(!a.ui.status_error, "{}", a.ui.status);
            assert_eq!(a.session.active().unwrap().doc.layers, shown.layers);
        }
    }

    #[test]
    fn reduced_warp_preview_preserves_samples_outside_a_fractional_selection_frame() {
        let mut session = Session::new();
        let mut doc = Document::with_background("Reduced Warp", Size::new(2400, 2400), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        doc.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 2400, 2400), &[0.2, 0.4, 0.6, 1.0]);
        session.add_document(doc, None);
        let mut a = PhotocraftApp::new(session, crate::Services::default());
        a.run("channel.target", json!({"channel":"green"})).unwrap();
        a.run("select.rect", json!({"x":101,"y":101,"width":100,"height":100})).unwrap();
        crate::transform_tool::begin(&mut a, &egui::Context::default()).unwrap();
        let t = a.ui.transform.as_mut().unwrap();
        t.warp = Some(Warp::custom(BezierMesh::identity(t.rect, 1, 1).map_points(|p| [p[0] + 4.0, p[1]]), t.rect));
        let t = a.ui.transform.clone().unwrap();
        let pv = a.transform_preview.as_mut().unwrap().channel.as_mut().unwrap();
        assert_eq!(pv.factor, 2);
        pv.update(&t).unwrap();
        let shown = pv.shown.layers[0].surface().unwrap().clone();
        let original = pv.base.layers[0].surface().unwrap();
        assert_eq!(shown.pixel(10, 10), original.pixel(10, 10));
        assert!((shown.sample_channel(51, 70, 1) - 1.0).abs() < 0.01, "vacated channel uses the captured background");
        assert!((shown.sample_channel(80, 70, 1) - 0.4).abs() < 0.01);
        crate::transform_tool::commit(&mut a);
        assert!(!a.ui.status_error, "{}", a.ui.status);
        let committed = crate::proxy::downsample(a.session.active().unwrap().doc.layers[0].surface().unwrap(), 2);
        for (x, y) in [(10, 10), (51, 70), (80, 70), (104, 70)] {
            assert_eq!(shown.pixel(x, y), committed.pixel(x, y), "proxy and full-resolution commit at ({x},{y})");
        }
    }

    #[test]
    fn hidden_target_refuses_to_start_without_changing_eyes() {
        for target in [json!("red"), json!(0), json!("mask"), json!("composite")] {
            let mut a = app();
            if target == 0 {
                a.run("channel.new", json!({})).unwrap();
            }
            if target == "mask" {
                a.run("layer.layerMask.revealAll", json!({})).unwrap();
            }
            if target == "composite" {
                a.run("select.editInQuickMaskMode", json!({"on":true})).unwrap();
            }
            a.run("channel.target", json!({"channel":target})).unwrap();
            if target == "red" {
                a.run("channel.setVisible", json!({"channel":"green","visible":true})).unwrap();
            }
            if target == "mask" {
                a.run("view.layerMask", json!({"mode":"off"})).unwrap();
            } else {
                a.run("channel.setVisible", json!({"channel": if target=="composite" { json!("quickMask") } else { target.clone() },"visible":false})).unwrap();
            }
            let before = a.session.active().unwrap().channel_view.clone();
            assert!(
                crate::transform_tool::begin(&mut a, &egui::Context::default()).unwrap_err().contains(photocraft_engine::channel_transform::HIDDEN),
                "{target}"
            );
            assert_eq!(a.session.active().unwrap().channel_view, before);
            assert!(a.session.active().unwrap().channel_transform.is_none());
            assert!(a.ui.transform.is_none());
        }
    }

    #[test]
    fn eyes_recompose_captured_preview_and_persist_through_commit_cancel_and_undo() {
        for selection in 0..4 {
            for warp in [false, true] {
                for commit in [false, true] {
                    let mut a = app();
                    if selection == 1 {
                        a.run("select.all", json!({})).unwrap();
                    }
                    if selection >= 2 {
                        a.run("select.rect", json!({"x":4,"y":4,"width":12,"height":12})).unwrap();
                    }
                    if selection == 3 {
                        a.run("select.modify.feather", json!({"radius":1})).unwrap();
                    }
                    a.run("channel.new", json!({"name":"overlay"})).unwrap();
                    a.run("channel.target", json!({"channel":"red"})).unwrap();
                    a.run("channel.setVisible", json!({"channel":"green","visible":true})).unwrap();
                    a.run("channel.setVisible", json!({"channel":0,"visible":true})).unwrap();
                    let before = a.session.active().unwrap().doc.clone();
                    let view = a.session.active().unwrap().channel_view.clone();
                    let history = a.session.active().unwrap().history.past_len();
                    crate::transform_tool::begin(&mut a, &egui::Context::default()).unwrap();
                    assert_eq!(a.session.active().unwrap().channel_view, view, "start preserves every eye");
                    let t = a.ui.transform.as_mut().unwrap();
                    t.quad = t.quad.map(|[x, y]| [x + 3.0, y]);
                    if warp {
                        t.warp = Some(Warp::custom(BezierMesh::identity(t.rect, 1, 1).map_points(|p| [p[0] + 3.0, p[1]]), t.rect));
                    }
                    let t = a.ui.transform.clone().unwrap();
                    let pv = a.transform_preview.as_mut().unwrap().channel.as_mut().unwrap();
                    pv.update(&t).unwrap();
                    let shown = pv.shown.clone();
                    let expected_image = pv.image(&view, false, None);
                    for (cmd, p) in [
                        ("channel.target", json!({"channel":"green"})),
                        ("layer.select", json!({"layer":t.layer})),
                        ("channel.delete", json!({"channel":0})),
                        ("layer.delete", json!({})),
                        ("select.editInQuickMaskMode", json!({})),
                    ] {
                        assert!(a.run(cmd, p).is_err(), "{cmd}");
                        assert_eq!(a.ui.transform.as_ref().unwrap().quad, t.quad);
                    }
                    let ctx = egui::Context::default();
                    let (req, _) = crate::control::ControlRequest::new("ui.set", json!({"maskTarget":true}));
                    let crate::control::Outcome::Done(reply) = crate::control::handle(&mut a, &ctx, &req) else { panic!("immediate reply") };
                    assert_eq!(reply["ok"], false);
                    a.run("channel.setVisible", json!({"channel":"red","visible":false})).unwrap();
                    assert!(a.ui.transform.is_some(), "hiding target keeps handles active");
                    let hidden = a.session.active().unwrap().channel_view.clone();
                    let pv = a.transform_preview.as_mut().unwrap().channel.as_mut().unwrap();
                    pv.update(&t).unwrap();
                    assert!(Arc::ptr_eq(&shown, &pv.shown), "eyes do not resample or change captured geometry");
                    assert_ne!(pv.image(&hidden, false, None).pixels, expected_image.pixels);
                    a.run("channel.setVisible", json!({"channel":"red","visible":true})).unwrap();
                    let restored = a.session.active().unwrap().channel_view.clone();
                    assert_eq!(a.transform_preview.as_ref().unwrap().channel.as_ref().unwrap().image(&restored, false, None).pixels, expected_image.pixels);
                    a.run("channel.setVisible", json!({"channel":"composite","visible":false})).unwrap();
                    a.run("channel.setVisible", json!({"channel":0,"visible":false})).unwrap();
                    let final_view = a.session.active().unwrap().channel_view.clone();
                    assert_eq!(final_view.visible_colors(3), 0);
                    assert_eq!(a.session.active().unwrap().channel_view.target, ChannelTarget::Color(0));
                    assert!(Arc::ptr_eq(&before, &a.session.active().unwrap().doc), "live preview never edits the document");
                    if commit {
                        crate::transform_tool::commit(&mut a);
                    } else {
                        crate::transform_tool::cancel(&mut a);
                    }
                    assert!(!a.ui.status_error, "{}", a.ui.status);
                    let st = a.session.active().unwrap();
                    assert!(st.channel_transform.is_none());
                    assert_eq!(st.channel_view, final_view);
                    assert_eq!(st.history.past_len(), history + usize::from(commit));
                    if commit {
                        assert_eq!(st.doc.layers[0].surface(), shown.layers[0].surface(), "native preview and commit agree");
                        a.session.undo();
                        assert_eq!(a.session.active().unwrap().doc.layers[0].surface(), before.layers[0].surface());
                        assert_eq!(a.session.active().unwrap().channel_view, final_view, "undo does not undo eyes");
                        a.session.redo();
                        assert_eq!(a.session.active().unwrap().doc.layers[0].surface(), shown.layers[0].surface());
                    } else {
                        assert!(Arc::ptr_eq(&before, &st.doc));
                    }
                    a.run("channel.target", json!({"channel":"blue"})).unwrap();
                }
            }
        }
    }

    #[test]
    fn failed_commit_and_document_switch_release_target_and_preserve_pixels() {
        let mut a = app();
        a.run("channel.target", json!({"channel":"red"})).unwrap();
        let before = a.session.active().unwrap().doc.clone();
        let ctx = egui::Context::default();
        crate::transform_tool::begin(&mut a, &ctx).unwrap();
        a.ui.transform.as_mut().unwrap().quad = [[0.0; 2]; 4];
        a.session.tools.background = [0.25, 0.5, 0.75, 1.0];
        crate::transform_tool::commit(&mut a);
        assert!(a.ui.status_error);
        assert!(a.session.active().unwrap().channel_transform.is_none());
        assert!(Arc::ptr_eq(&before, &a.session.active().unwrap().doc));
        assert_eq!(a.session.tools.background, [0.25, 0.5, 0.75, 1.0]);
        crate::transform_tool::begin(&mut a, &ctx).unwrap();
        a.session.add_document(Document::new("other", Size::new(8, 8), ColorMode::Rgb, SampleType::U8), None);
        crate::transform_tool::end_if_left(&mut a);
        assert!(a.ui.transform.is_none());
        assert!(a.session.documents().iter().all(|st| st.channel_transform.is_none()));
        assert!(Arc::ptr_eq(&before, &a.session.documents()[0].doc));
    }

    #[test]
    fn targeted_mask_can_hide_reappear_and_warp_without_retargeting() {
        let mut a = app();
        a.session
            .edit("mask", |doc, active| {
                let mut mask = LayerMask::reveal_all();
                mask.surface.fill_rect(Rect::new(4, 4, 16, 16), &[0.0]);
                doc.layer_mut(active.unwrap()).unwrap().mask = Some(mask);
                Ok(())
            })
            .unwrap();
        a.run("channel.target", json!({"channel":"mask"})).unwrap();
        a.run("view.layerMask", json!({"mode":"overlay"})).unwrap();
        let ctx = egui::Context::default();
        crate::transform_tool::begin(&mut a, &ctx).unwrap();
        crate::transform_tool::enter_warp(&mut a, &ctx);
        let t = a.ui.transform.clone().unwrap();
        a.run("view.layerMask", json!({"mode":"off"})).unwrap();
        a.sync_mask_targets();
        assert!(a.ui.mask_target);
        assert_eq!(a.ui.transform.as_ref().unwrap().target, t.target);
        assert!(a.ui.transform.as_ref().unwrap().warp.is_some());
        a.run("view.layerMask", json!({"mode":"overlay"})).unwrap();
        crate::transform_tool::cancel(&mut a);
        assert!(photocraft_engine::mask_view_cmds::current(a.session.active().unwrap()).is_some());
    }

    #[test]
    fn every_nonmask_eye_reveals_its_channel_during_a_grayscale_mask_transform() {
        for channel in [json!("composite"), json!("green"), json!(0), json!("quickMask")] {
            let mut a = app();
            a.run("channel.new", json!({})).unwrap();
            a.run("channel.target.composite", json!({})).unwrap();
            a.run("select.editInQuickMaskMode", json!({"on":true})).unwrap();
            a.run("layer.layerMask.revealAll", json!({})).unwrap();
            a.run("channel.target", json!({"channel":"mask"})).unwrap();
            a.run("view.layerMask", json!({"mode":"gray"})).unwrap();
            let ctx = egui::Context::default();
            crate::transform_tool::begin(&mut a, &ctx).unwrap();
            let t = a.ui.transform.clone().unwrap();
            let before = a.session.active().unwrap().doc.clone();
            let eyes = a.session.active().unwrap().channel_view.clone();
            assert!(a.run("channel.setVisible", json!({"channel":"mask","visible":true})).is_err());
            assert_eq!(a.session.active().unwrap().channel_view, eyes, "invalid eyes cannot change the mask view");
            a.run("channel.setVisible", json!({"channel":channel,"visible":true})).unwrap();
            let st = a.session.active().unwrap();
            assert_eq!(photocraft_engine::mask_view_cmds::current(st).unwrap().mode, photocraft_engine::mask_view_cmds::MaskViewMode::Overlay);
            assert_eq!(st.channel_view.target, ChannelTarget::LayerMask);
            assert_eq!(a.ui.transform.as_ref().unwrap().target, t.target);
            assert!(Arc::ptr_eq(&before, &st.doc));
            let final_eyes = st.channel_view.clone();
            crate::transform_tool::cancel(&mut a);
            assert_eq!(a.session.active().unwrap().channel_view, final_eyes);
        }
    }
}
