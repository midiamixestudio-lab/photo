//! Remaining pixel-aspect display gaps after #2718.
use super::*;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 120, "height": 80})).unwrap();
    app.sync_views();
    app.ui.extras.rulers = false;
    app.ui.views[0].fit_pending = false;
    app.ui.views[0].center = [60.0, 40.0];
    app
}

fn frame(app: &mut PhotocraftApp, ctx: &egui::Context, ppp: f32) -> egui::FullOutput {
    let rect = Rect::from_min_size(pos2(20.0, 30.0), vec2(640.0, 480.0));
    app.last_canvas_rect = rect;
    let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
    input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(ppp);
    let mut output = ctx.run_ui(input, |ui| {
        let v = app.ui.views[0].clone();
        app.ui.views[0] = canvas_view(app, ui, 0, rect, v, true);
    });
    output.textures_delta.clear();
    output
}

#[test]
#[ignore = "requires a wgpu adapter; run explicitly for visual validation"]
fn aspect_rendered_cpu_gpu_extent_at_both_display_scales() {
    for gpu in [false, true] {
        for ppp in [1.0, 2.0] {
            for (preset, aspect, rotation) in [
                ("square", 1.0_f32, 0.0),
                ("anamorphic2To1", 2.0, 0.0),
                ("d1DvNtsc", 0.91, 0.0),
                ("custom:1.25:Test", 1.25, 0.0),
                ("anamorphic2To1", 2.0, 90.0),
            ] {
                let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 700.0)).with_pixels_per_point(ppp).wgpu().build_eframe(move |cc| {
                    PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                    let mut app = app();
                    app.ui.views[0].zoom = 1.0;
                    app.run("edit.fill", json!({"color": "#11df47"})).unwrap();
                    app.ui.view.pixel_aspect = preset.into();
                    app.ui.view.pixel_aspect_correction = true;
                    app.ui.views[0].rotation = rotation;
                    if gpu {
                        app.set_wgpu(cc.wgpu_render_state.clone().unwrap());
                    }
                    app
                });
                h.run_steps(5);
                assert_eq!(h.state().perf.gpu, gpu);
                let pixels = h.render().unwrap();
                let canvas = ViewXform::active(h.state()).unwrap().rect;
                let mut min = [u32::MAX; 2];
                let mut max = [0; 2];
                for (x, y, p) in pixels.enumerate_pixels() {
                    if canvas.contains(pos2((x as f32 + 0.5) / ppp, (y as f32 + 0.5) / ppp))
                        && p[0].abs_diff(17) <= 2
                        && p[1].abs_diff(223) <= 2
                        && p[2].abs_diff(71) <= 2
                    {
                        min = [min[0].min(x), min[1].min(y)];
                        max = [max[0].max(x), max[1].max(y)];
                    }
                }
                assert_ne!(min[0], u32::MAX);
                let extent = [max[0] - min[0] + 1, max[1] - min[1] + 1];
                let want = if rotation == 0.0 { [(120.0 * aspect).round() as u32, 80] } else { [80, (120.0 * aspect).round() as u32] };
                assert!(
                    extent[0].abs_diff(want[0]) <= 1 && extent[1].abs_diff(want[1]) <= 1,
                    "gpu={gpu}, ppp={ppp}, {preset}, rotation={rotation}: {extent:?} != {want:?}"
                );
                if let Some(dir) = std::env::var_os("PHOTOCRAFT_ASPECT_SNAPSHOTS") {
                    let dir = std::path::PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    pixels.save(dir.join(format!("aspect-{}-{ppp}-{aspect}-{rotation}.png", if gpu { "gpu" } else { "cpu" }))).unwrap();
                }
            }
        }
    }
}

#[test]
fn aspect_supported_mesh_and_pins_toggle_the_real_puppet_overlay() {
    let mut app = app();
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    app.run("layer.new.layer", json!({})).unwrap();
    app.run("select.rect", json!({"x": 20, "y": 20, "width": 60, "height": 40})).unwrap();
    app.run("edit.fill", json!({"color": "#11df47"})).unwrap();
    app.run("select.deselect", json!({})).unwrap();
    crate::puppet_ui::begin(&mut app, &ctx, "edit.puppetWarp").unwrap();
    crate::puppet_ui::pointer(&mut app, ToolEvent::Down { x: 45.0, y: 35.0, pressure: 1.0 }, egui::Modifiers::NONE);
    crate::puppet_ui::pointer(&mut app, ToolEvent::Up { x: 45.0, y: 35.0 }, egui::Modifiers::NONE);
    let count = |app: &mut PhotocraftApp| {
        let out = frame(app, &ctx, 1.0);
        let mesh_edges =
            out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::LineSegment { stroke, .. } if (stroke.width - 0.6).abs() < 0.001)).count();
        let pins = out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Circle(c) if c.fill == Color32::from_rgb(255, 214, 10))).count();
        (mesh_edges, pins)
    };
    for (id, on) in [("view.show.mesh", true), ("view.show.editPins", true)] {
        crate::menus::invoke(&mut app, &ctx, id, json!({"on": on})).unwrap();
    }
    let shown = count(&mut app);
    assert!(shown.0 > 0 && shown.1 > 0);
    crate::menus::invoke(&mut app, &ctx, "view.show.mesh", json!({"on": false})).unwrap();
    assert_eq!(count(&mut app), (0, shown.1));
    crate::menus::invoke(&mut app, &ctx, "view.show.editPins", json!({"on": false})).unwrap();
    assert_eq!(count(&mut app), (0, 0));
    crate::menus::invoke(&mut app, &ctx, "view.show.all", json!({})).unwrap();
    assert_eq!(count(&mut app), shown);
    crate::menus::invoke(&mut app, &ctx, "view.extras", json!({"on": false})).unwrap();
    assert_eq!(count(&mut app), (0, 0));
    assert_eq!(app.distort.puppet.as_ref().unwrap().warp.pins.len(), 1, "visibility doesn't delete pins");
}

#[test]
fn aspect_unavailable_extras_options_reject_cosmetic_changes() {
    let mut app = app();
    let ctx = egui::Context::default();
    let before = app.ui.view.show;
    for field in ["brush_preview", "artboard_guides"] {
        let mut params = json!({});
        params[field] = json!(false);
        assert!(crate::menus::invoke(&mut app, &ctx, "view.show.showExtrasOptions", params).is_err());
        assert_eq!(app.ui.view.show, before);
    }
}

#[test]
#[ignore = "requires a wgpu adapter; run explicitly for visual validation"]
fn aspect_grid_rendered_cpu_gpu_threshold_uses_smaller_physical_axis() {
    for gpu in [false, true] {
        for ppp in [1.0, 2.0] {
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 700.0)).with_pixels_per_point(ppp).wgpu().build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = app();
                app.run("edit.fill", json!({"color": "#11df47"})).unwrap();
                app.ui.view.pixel_aspect = "d1DvNtsc".into();
                app.ui.view.pixel_aspect_correction = true;
                if gpu {
                    app.set_wgpu(cc.wgpu_render_state.clone().unwrap());
                }
                app
            });
            for (zoom, shown) in [(5.25, false), (6.0, true)] {
                h.state_mut().ui.views[0].zoom = zoom;
                h.run_steps(5);
                assert_eq!(h.state().perf.gpu, gpu);
                let pixels = h.render().unwrap();
                let center = ViewXform::active(h.state()).unwrap().rect.center() * ppp;
                let mut grid_pixels = 0;
                for (x, y, p) in pixels.enumerate_pixels() {
                    if (x as f32 - center.x).abs() < 20.0
                        && (y as f32 - center.y).abs() < 20.0
                        && (p[0].abs_diff(17) > 4 || p[1].abs_diff(223) > 4 || p[2].abs_diff(71) > 4)
                    {
                        grid_pixels += 1;
                    }
                }
                assert_eq!(grid_pixels > 0, shown, "gpu={gpu}, ppp={ppp}, zoom={zoom}, grid pixels={grid_pixels}");
            }
        }
    }
}

#[test]
fn aspect_review_red_eye_draws_native_search_ellipse() {
    for aspect in [2.0, 0.91] {
        for rotation in [0.0, 90.0] {
            for ppp in [1.0, 2.0] {
                let mut a = app();
                a.ui.views[0].zoom = 1.0;
                a.ui.tool = Tool::RedEye;
                a.ui.view.pixel_aspect = format!("custom:{aspect}");
                a.ui.view.pixel_aspect_correction = true;
                a.ui.views[0].rotation = rotation;
                let ctx = egui::Context::default();
                PhotocraftApp::setup_context(&ctx, Default::default());
                frame(&mut a, &ctx, ppp);
                let rect = a.last_canvas_rect;
                let pointer = rect.center();
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                    events: vec![egui::Event::PointerMoved(pointer)],
                    ..Default::default()
                };
                input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(ppp);
                let mut out = ctx.run_ui(input, |ui| {
                    let v = a.ui.views[0].clone();
                    a.ui.views[0] = canvas_view(&mut a, ui, 0, rect, v, true);
                });
                out.textures_delta.clear();
                let size = out
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Path(p) if p.stroke.color == egui::epaint::ColorMode::Solid(Color32::from_white_alpha(220)) && p.closed => {
                            Some(s.shape.visual_bounding_rect().size())
                        }
                        egui::Shape::Circle(p) if p.stroke.color == Color32::from_white_alpha(220) => Some(s.shape.visual_bounding_rect().size()),
                        _ => None,
                    })
                    .expect("Red Eye footprint");
                let d = 2.0 * photocraft_algo::redeye::search_radius(a.ui.tool_options.red_eye_pupil_size) / ppp;
                let expected = if rotation == 0.0 { vec2(d * aspect + 1.0, d + 1.0) } else { vec2(d + 1.0, d * aspect + 1.0) };
                assert!((size - expected).length() < 0.1, "{aspect} {rotation} {ppp}: {size:?} vs {expected:?}");
            }
        }
    }
}

#[test]
#[ignore = "requires a wgpu adapter; run explicitly for visual validation"]
fn aspect_review_rendered_secondary_footprints_at_both_display_scales() {
    let dir = std::env::var_os("PHOTOCRAFT_ASPECT_SNAPSHOTS").map(std::path::PathBuf::from).expect("snapshot directory");
    std::fs::create_dir_all(&dir).unwrap();
    for ppp in [1.0, 2.0] {
        for rotation in [0.0, 90.0] {
            for tool in [Tool::Brush, Tool::RedEye] {
                let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 700.0)).with_pixels_per_point(ppp).wgpu().build_eframe(move |cc| {
                    PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                    let mut a = app();
                    a.run("edit.fill", json!({"color":"#506080"})).unwrap();
                    a.run("tools.setBrush", json!({"brush":{"size":20.0}})).unwrap();
                    a.run("prefs.set", json!({"path":"cursors.painting","value":"standard"})).unwrap();
                    a.ui.view.pixel_aspect = "anamorphic2To1".into();
                    a.ui.view.pixel_aspect_correction = true;
                    a.ui.views[0].rotation = rotation;
                    a.ui.tool = tool;
                    a.last_stroke_end = Some((a.session.active().unwrap().doc.id, [20.0, 20.0, 1.0]));
                    a
                });
                h.run_steps(5);
                let at = ViewXform::active(h.state()).unwrap().to_screen(70.0, 45.0);
                if tool == Tool::Brush {
                    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
                }
                h.event(egui::Event::PointerMoved(at));
                h.run_steps(3);
                h.render().unwrap().save(dir.join(format!("aspect-followup-{tool:?}-{ppp}-{rotation}.png"))).unwrap();
                if tool == Tool::Brush {
                    crate::brush_resize::pointer(
                        h.state_mut(),
                        ToolEvent::Down { x: 70.0, y: 45.0, pressure: 1.0 },
                        egui::Modifiers { ctrl: true, alt: true, ..Default::default() },
                        false,
                    );
                    h.run_steps(2);
                    h.render().unwrap().save(dir.join(format!("aspect-brush-hud-{ppp}-{rotation}.png"))).unwrap();
                }
            }
        }
    }
}

#[test]
fn unavailable_view_overlays_reject_menu_invocations() {
    let mut app = app();
    let ctx = egui::Context::default();
    for id in ["view.show.brushPreview", "view.show.artboardGuides"] {
        assert!(!crate::menus::is_enabled(&app, id), "{id}");
        assert!(crate::menus::invoke(&mut app, &ctx, id, json!({"on":true})).is_err());
    }
}

#[test]
fn puppet_show_mesh_automation_synchronizes_global_flag_and_extras() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.run("edit.fill", json!({"color":"#11df47"})).unwrap();
    crate::puppet_ui::begin(&mut app, &ctx, "edit.puppetWarp").unwrap();
    app.ui.view.extras = false;
    app.ui.view.show.mesh = false;
    crate::menus::invoke(&mut app, &ctx, "edit.puppetWarp", json!({"ui":{"showMesh":true}})).unwrap();
    assert!(app.ui.view.extras && app.ui.view.show.mesh);
    assert!(app.distort.puppet.as_ref().unwrap().show_mesh);
    crate::menus::invoke(&mut app, &ctx, "edit.puppetWarp", json!({"ui":{"showMesh":false}})).unwrap();
    assert!(!app.ui.view.show.mesh && !app.distort.puppet.as_ref().unwrap().show_mesh);
    assert!(app.ui.view.extras, "hiding the mesh leaves the other Extras visible");
}
