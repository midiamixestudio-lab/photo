//! Edit › Perspective Warp on the canvas.
//!
//! **Layout** mode: drag on the image to draw a plane, drag its corners onto the image's
//! perspective lines; a corner dropped near another plane's corner snaps to it, which links the
//! planes along that edge. **Warp** mode: drag corners (linked corners move together) and the
//! image follows; the options bar straightens near-vertical / near-horizontal edges. ↩ / Commit
//! runs `edit.perspectiveWarp` with the planes. The preview is the layer texture on a 40×40 mesh
//! pushed through the same piecewise-homography map the engine uses.

use std::sync::Arc;

use egui::{Color32, Pos2, Stroke, TextureHandle, pos2};
use photocraft_algo::perspective::{PerspectiveMap, Plane, Straighten, linked_corners, plane_point, straighten};
use photocraft_doc::{Document, LayerId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, ViewXform};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PerspMode {
    Layout,
    Warp,
}

enum Drag {
    NewPlane {
        start: [f64; 2],
        cur: [f64; 2],
    },
    /// Linked corners being moved, and the pointer offset from the corner.
    Corner {
        group: Vec<(usize, usize)>,
        offset: [f64; 2],
    },
}

pub struct PerspSession {
    pub key: u64,
    pub layer: LayerId,
    command: String,
    bounds: Rect,
    pub planes: Vec<Plane>,
    pub mode: PerspMode,
    drag: Option<Drag>,
    texture: TextureHandle,
    opacity: f32,
    pub preview_doc: Arc<Document>,
    /// Preview mesh: (screen-independent destination points) for a 40×40 grid over `bounds`.
    grid: Vec<[f64; 2]>,
}

const GRID: usize = 40;

impl PerspSession {
    pub fn describe(&self) -> Value {
        json!({"document": self.preview_doc.id.0, "layer": self.layer.0, "mode": self.mode, "planes": self.planes})
    }

    fn update_grid(&mut self) {
        let b = self.bounds;
        let map = if self.mode == PerspMode::Warp { PerspectiveMap::new(&self.planes) } else { None };
        self.grid.clear();
        for j in 0..=GRID {
            for i in 0..=GRID {
                let x = f64::from(b.x0) + f64::from(b.width()) * i as f64 / GRID as f64;
                let y = f64::from(b.y0) + f64::from(b.height()) * j as f64 / GRID as f64;
                let (u, v) = map.as_ref().map_or((x, y), |m| m.map(x, y));
                self.grid.push([u, v]);
            }
        }
    }

    fn corners(&self, p: usize) -> &[[f64; 2]; 4] {
        if self.mode == PerspMode::Layout { &self.planes[p].src } else { &self.planes[p].dst }
    }

    fn corner_at(&self, p: [f64; 2], tol: f64) -> Option<(usize, usize)> {
        let mut best = None;
        let mut bd = tol;
        for pi in 0..self.planes.len() {
            for (ci, c) in self.corners(pi).iter().enumerate() {
                let d = (c[0] - p[0]).hypot(c[1] - p[1]);
                if d <= bd {
                    bd = d;
                    best = Some((pi, ci));
                }
            }
        }
        best
    }

    fn snap(&self, p: [f64; 2], tol: f64, skip: &[(usize, usize)]) -> [f64; 2] {
        for (pi, pl) in self.planes.iter().enumerate() {
            for (ci, c) in pl.src.iter().enumerate() {
                if !skip.contains(&(pi, ci)) && (c[0] - p[0]).hypot(c[1] - p[1]) <= tol {
                    return *c;
                }
            }
        }
        p
    }

    fn set_corner(&mut self, group: &[(usize, usize)], p: [f64; 2]) {
        let layout = self.mode == PerspMode::Layout;
        for &(pi, ci) in group {
            let Some(pl) = self.planes.get_mut(pi) else { continue };
            if layout && let Some(c) = pl.src.get_mut(ci) {
                *c = p;
            }
            if let Some(c) = pl.dst.get_mut(ci) {
                *c = p;
            }
        }
    }
}

/// A preview belongs to the document that supplied its pixels, not necessarily the active tab.
pub(crate) fn bound_to_active_document(app: &PhotocraftApp, s: &PerspSession) -> bool {
    app.session.active().is_some_and(|state| state.doc.id == s.preview_doc.id)
}

pub fn begin(app: &mut PhotocraftApp, ctx: &egui::Context, command: &str) -> Result<(), String> {
    if app.ui.transform.is_some() {
        return Err("finish Free Transform first".into());
    }
    let (layer, surface, _) = crate::distort_ui::active_pixels(app)?;
    let st = app.session.active().ok_or("no document")?;
    let l = st.doc.layer(layer).ok_or("no layer")?;
    let bounds = surface.content_bounds();
    if bounds.is_empty() {
        return Err("Could not use Perspective Warp: the layer is empty".into());
    }
    let opacity = l.opacity * l.fill_opacity;
    let preview_doc = crate::distort_ui::without_layer(&st.doc, layer);
    let key = app.ui.alloc_id();
    let texture = ctx.load_texture(format!("persp-{key}"), crate::distort_ui::surface_image(&surface, bounds, 2048), egui::TextureOptions::LINEAR);
    let mut s = PerspSession {
        key,
        layer,
        command: command.to_string(),
        bounds,
        planes: Vec::new(),
        mode: PerspMode::Layout,
        drag: None,
        texture,
        opacity,
        preview_doc,
        grid: Vec::new(),
    };
    s.update_grid();
    app.distort.perspective = Some(s);
    app.ui.status = tl!("Perspective Warp: draw planes along the image's perspective, then switch to Warp").into();
    Ok(())
}

/// Apply the pending perspective transaction. A command error must not be reported as a
/// successful commit to an agent, and the editable preview survives failure.
pub fn commit(app: &mut PhotocraftApp) -> Result<Value, String> {
    if app.distort.perspective.as_ref().is_some_and(|s| !bound_to_active_document(app, s)) {
        let error = "Perspective Warp preview belongs to another document";
        app.ui.status = error.into();
        return Err(error.into());
    }
    let Some(s) = app.distort.perspective.take() else {
        return Err("Perspective Warp is not active".into());
    };
    if s.planes.is_empty() {
        return Ok(json!({"committed": false, "reason": "noPlanes"}));
    }
    if let Err(error) = validate_planes(&s.planes) {
        app.distort.perspective = Some(s);
        return Err(error);
    }
    if PerspectiveMap::is_identity(&s.planes) {
        return Ok(json!({"committed": false, "reason": "identity"}));
    }
    match app.run(&s.command, json!({"planes": s.planes.clone(), "layer": s.layer.0})) {
        Ok(result) => Ok(json!({"committed": true, "result": result})),
        Err(error) => {
            app.ui.status = format!("Perspective Warp: {error}");
            app.distort.perspective = Some(s);
            Err(error)
        }
    }
}

/// Validate an agent-supplied preview before mutating the UI state.
fn validate_planes(planes: &[Plane]) -> Result<(), String> {
    if planes.iter().flat_map(|p| p.src.iter().chain(p.dst.iter())).any(|point| point.iter().any(|v| !v.is_finite())) {
        return Err("plane coordinates must be finite".into());
    }
    if !planes.is_empty() && PerspectiveMap::new(planes).is_none() {
        return Err("a plane is degenerate (its corners must form a quad)".into());
    }
    Ok(())
}

fn set_mode(s: &mut PerspSession, m: PerspMode) {
    s.mode = m;
    if m == PerspMode::Warp {
        // Linked corners share one position from here on.
        photocraft_algo::perspective::unify(&mut s.planes);
    }
    s.update_grid();
}

/// Control channel: `edit.perspectiveWarp {"ui": {...}}` while the mode is active.
pub fn control(app: &mut PhotocraftApp, ui: &Value) -> Result<Value, String> {
    if ui.get("commit").and_then(Value::as_bool) == Some(true) {
        return commit(app);
    }
    if ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        app.distort.perspective = None;
        return Ok(json!({"cancelled": true}));
    }
    // A stale preview belongs to its original document even if an agent switches tabs.
    if app.distort.perspective.as_ref().is_some_and(|s| !bound_to_active_document(app, s)) {
        return Err("Perspective Warp preview belongs to another document".into());
    }
    // Parse every requested field before changing the session. A malformed request should
    // never partly switch modes, discard a corner drag or silently produce an identity preview.
    let requested_mode = match ui.get("mode") {
        None => None,
        Some(Value::String(mode)) => Some(match mode.as_str() {
            "layout" => PerspMode::Layout,
            "warp" => PerspMode::Warp,
            _ => return Err("mode must be layout|warp".into()),
        }),
        Some(_) => return Err("mode must be layout|warp".into()),
    };
    let replacement: Option<Vec<Plane>> = ui.get("planes").map(|p| serde_json::from_value(p.clone()).map_err(|e| format!("bad planes: {e}"))).transpose()?;
    let requested_straighten = match ui.get("straighten") {
        None => None,
        Some(Value::String(st)) => Some(Straighten::parse(st).ok_or("straighten: horizontal|vertical|auto")?),
        Some(_) => return Err("straighten: horizontal|vertical|auto".into()),
    };
    let s = app.distort.perspective.as_mut().ok_or(tl!("Perspective Warp is not active"))?;
    let new_mode = requested_mode.unwrap_or(s.mode);
    let replaced_planes = replacement.is_some();
    let mut new_planes = replacement.unwrap_or_else(|| s.planes.clone());
    if new_mode == PerspMode::Warp && (requested_mode == Some(PerspMode::Warp) || replaced_planes) {
        photocraft_algo::perspective::unify(&mut new_planes);
    }
    if let Some(straighten_mode) = requested_straighten {
        straighten(&mut new_planes, straighten_mode);
    }
    validate_planes(&new_planes)?;
    s.mode = new_mode;
    s.planes = new_planes;
    if replaced_planes || requested_mode.is_some() {
        // A corner drag holds indices into the previous plane list (#952).
        s.drag = None;
    }
    s.update_grid();
    Ok(s.describe())
}

pub fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, _mods: egui::Modifiers) {
    let tol = crate::distort_ui::tolerance(app);
    let Some(s) = app.distort.perspective.as_mut() else { return };
    match ev {
        ToolEvent::Down { x, y, .. } => {
            let p = [x, y];
            if let Some((pi, ci)) = s.corner_at(p, tol) {
                let c = s.corners(pi)[ci];
                // Move every corner linked to this one.
                let group = linked_corners(&s.planes).into_iter().find(|g| g.contains(&(pi, ci))).unwrap_or_else(|| vec![(pi, ci)]);
                s.drag = Some(Drag::Corner { group, offset: [c[0] - x, c[1] - y] });
            } else if s.mode == PerspMode::Layout {
                let start = s.snap(p, tol, &[]);
                s.drag = Some(Drag::NewPlane { start, cur: p });
            }
        }
        ToolEvent::Move { x, y, .. } => match &mut s.drag {
            Some(Drag::NewPlane { cur, .. }) => *cur = [x, y],
            Some(Drag::Corner { group, offset }) => {
                let group = group.clone();
                let mut q = [x + offset[0], y + offset[1]];
                if s.mode == PerspMode::Layout {
                    q = s.snap(q, tol, &group);
                }
                s.set_corner(&group, q);
                if s.mode == PerspMode::Warp {
                    s.update_grid();
                }
            }
            None => {}
        },
        ToolEvent::Up { x, y } => {
            match s.drag.take() {
                Some(Drag::NewPlane { start, .. }) => {
                    let end = s.snap([x, y], tol, &[]);
                    let (x0, y0, x1, y1) = (start[0].min(end[0]), start[1].min(end[1]), start[0].max(end[0]), start[1].max(end[1]));
                    if x1 - x0 > 4.0 && y1 - y0 > 4.0 {
                        // Keep the snapped corners exact.
                        let mut q = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
                        for c in q.iter_mut() {
                            *c = s.snap(*c, tol, &[]);
                        }
                        s.planes.push(Plane::identity(q));
                    }
                }
                Some(Drag::Corner { group, offset }) => {
                    let mut q = [x + offset[0], y + offset[1]];
                    if s.mode == PerspMode::Layout {
                        q = s.snap(q, tol, &group);
                    }
                    s.set_corner(&group, q);
                }
                None => {}
            }
            s.update_grid();
        }
    }
}

pub fn draw(s: &PerspSession, painter: &egui::Painter, xf: &ViewXform) {
    let scr = |p: [f64; 2]| xf.to_screen(p[0] as f32, p[1] as f32);
    let mut mesh = egui::Mesh::with_texture(s.texture.id());
    let tint = Color32::from_white_alpha((s.opacity.clamp(0.0, 1.0) * 255.0) as u8);
    for j in 0..=GRID {
        for i in 0..=GRID {
            let p = s.grid[j * (GRID + 1) + i];
            mesh.vertices.push(egui::epaint::Vertex { pos: scr(p), uv: pos2(i as f32 / GRID as f32, j as f32 / GRID as f32), color: tint });
        }
    }
    for j in 0..GRID {
        for i in 0..GRID {
            let a = (j * (GRID + 1) + i) as u32;
            mesh.add_triangle(a, a + 1, a + GRID as u32 + 2);
            mesh.add_triangle(a, a + GRID as u32 + 2, a + GRID as u32 + 1);
        }
    }
    painter.add(mesh);
    let accent = crate::theme::Tokens::get(painter.ctx()).accent;
    let warp = s.mode == PerspMode::Warp;
    for p in &s.planes {
        // Plane grid at thirds (like Photoshop's), then the outline.
        let thin = Stroke::new(0.75, Color32::from_rgba_unmultiplied(255, 255, 255, 150));
        for k in 1..3 {
            let t = k as f64 / 3.0;
            painter.line_segment([scr(plane_point(p, t, 0.0, warp)), scr(plane_point(p, t, 1.0, warp))], thin);
            painter.line_segment([scr(plane_point(p, 0.0, t, warp)), scr(plane_point(p, 1.0, t, warp))], thin);
        }
        let q = if warp { &p.dst } else { &p.src };
        let pts: Vec<Pos2> = q.iter().map(|c| scr(*c)).collect();
        painter.add(egui::Shape::closed_line(pts.clone(), Stroke::new(1.5, accent)));
        for c in pts {
            painter.circle_filled(c, 4.5, Color32::WHITE);
            painter.circle_stroke(c, 4.5, Stroke::new(1.25, accent));
        }
    }
    if let Some(Drag::NewPlane { start, cur }) = &s.drag {
        let r = egui::Rect::from_two_pos(scr(*start), scr(*cur));
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    }
}

pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(s) = app.distort.perspective.as_mut() else { return };
    if crate::widgets::pill_tab(ui, "Layout", s.mode == PerspMode::Layout).clicked() {
        set_mode(s, PerspMode::Layout);
    }
    if crate::widgets::pill_tab(ui, "Warp", s.mode == PerspMode::Warp).clicked() && !s.planes.is_empty() {
        set_mode(s, PerspMode::Warp);
    }
    crate::widgets::vline(ui, 22.0);
    if s.mode == PerspMode::Warp {
        for (label, tip, m) in [
            (tl!("Straighten"), tl!("Automatically straighten near-vertical line segments"), Straighten::Vertical),
            (tl!("Level"), tl!("Automatically level near-horizontal line segments"), Straighten::Horizontal),
            (tl!("Both"), tl!("Automatically straighten and level"), Straighten::Auto),
        ] {
            if ui.button(tl!(&label)).on_hover_text(tip).clicked() {
                straighten(&mut s.planes, m);
                s.update_grid();
            }
        }
    } else {
        ui.label(crate::i18n::trn(crate::i18n::current(), s.planes.len() as u64, "{n} plane", "{n} planes"));
        if ui.button(tl!("Remove Planes")).clicked() {
            s.planes.clear();
            s.update_grid();
        }
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if crate::widgets::primary_button(ui, "✓", 32.0)
            .on_hover_text(crate::i18n::fmt(tl!("Commit Perspective Warp ({key})"), &[("key", &crate::shortcuts::pretty("Enter"))]))
            .clicked()
        {
            let _ = commit(app);
        } else if crate::widgets::secondary_button(ui, "⊘", 32.0).on_hover_text(tl!("Cancel (Esc)")).clicked() {
            app.distort.perspective = None;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_snaps_planes_then_warp_and_commit() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 200, "height": 120, "depth": 8})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.session
            .edit("paint", |doc, active| {
                doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(10, 10, 190, 110), &[0.2, 0.6, 0.9, 1.0]);
                Ok(())
            })
            .unwrap();
        app.sync_views();
        crate::distort_ui::menu(&mut app, &ctx, "edit.perspectiveWarp", &json!({})).unwrap().unwrap();
        let ev = |app: &mut PhotocraftApp, e| crate::distort_ui::pointer(app, e, egui::Modifiers::NONE);
        // Two planes sharing an edge (the second starts at the first one's corner and snaps).
        ev(&mut app, ToolEvent::Down { x: 20.0, y: 20.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Move { x: 100.0, y: 100.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 100.0, y: 100.0 });
        ev(&mut app, ToolEvent::Down { x: 120.0, y: 30.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 180.0, y: 101.0 });
        // Drop its left corners next to the first plane's right corners: they snap.
        ev(&mut app, ToolEvent::Down { x: 120.0, y: 30.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 102.0, y: 21.0 });
        ev(&mut app, ToolEvent::Down { x: 120.0, y: 101.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 101.0, y: 99.0 });
        let s = app.distort.perspective.as_ref().unwrap();
        assert_eq!(s.planes.len(), 2);
        assert_eq!(s.planes[1].src[0], [100.0, 20.0], "snapped to the first plane's corner");
        assert_eq!(s.planes[1].src[3], [100.0, 100.0]);
        control(&mut app, &json!({"mode": "warp"})).unwrap();
        // Dragging the shared top corner moves both planes.
        ev(&mut app, ToolEvent::Down { x: 100.0, y: 20.0, pressure: 1.0 });
        ev(&mut app, ToolEvent::Up { x: 100.0, y: 5.0 });
        let s = app.distort.perspective.as_ref().unwrap();
        assert_eq!(s.planes[0].dst[1], [100.0, 5.0]);
        assert_eq!(s.planes[1].dst[0], [100.0, 5.0]);
        assert_eq!(s.planes[0].src[1], [100.0, 20.0], "warp mode keeps the layout");
        let before = app.session.active().unwrap().history.past_len();
        assert_eq!(commit(&mut app).unwrap()["committed"], json!(true));
        assert_eq!(app.session.active().unwrap().history.past_len(), before + 1);
    }

    #[test]
    fn replacing_the_planes_mid_corner_drag_ends_the_drag() {
        // #952: the drag kept (plane, corner) indices into the old list, so the next move or
        // release indexed the new, shorter one and panicked.
        for release in [false, true] {
            let ctx = egui::Context::default();
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.session.execute("file.new", json!({"width": 100, "height": 80, "depth": 8})).unwrap();
            app.session.execute("layer.new.layer", json!({})).unwrap();
            app.session
                .edit("paint", |doc, active| {
                    doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(20, 30, 80, 50), &[1.0, 0.0, 0.0, 1.0]);
                    Ok(())
                })
                .unwrap();
            app.sync_views();
            crate::distort_ui::menu(&mut app, &ctx, "edit.perspectiveWarp", &json!({})).unwrap().unwrap();
            let ev = |app: &mut PhotocraftApp, e| crate::distort_ui::pointer(app, e, egui::Modifiers::NONE);
            ev(&mut app, ToolEvent::Down { x: 20.0, y: 20.0, pressure: 1.0 });
            ev(&mut app, ToolEvent::Move { x: 90.0, y: 70.0, pressure: 1.0 });
            ev(&mut app, ToolEvent::Up { x: 90.0, y: 70.0 });
            // Press a corner, then replace the planes while it's held.
            ev(&mut app, ToolEvent::Down { x: 20.0, y: 20.0, pressure: 1.0 });
            assert!(matches!(app.distort.perspective.as_ref().unwrap().drag, Some(Drag::Corner { .. })));
            control(&mut app, &json!({"planes": []})).unwrap();
            if release {
                ev(&mut app, ToolEvent::Up { x: 30.0, y: 30.0 });
            } else {
                ev(&mut app, ToolEvent::Move { x: 30.0, y: 30.0, pressure: 1.0 });
            }
            let s = app.distort.perspective.as_ref().unwrap();
            assert!(s.planes.is_empty() && s.drag.is_none(), "release {release}");
        }
    }
}

#[cfg(test)]
mod agent_control_tests {
    use super::*;

    fn open_session() -> PhotocraftApp {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 64, "height": 64, "depth": 8})).unwrap();
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.session
            .edit("paint", |doc, active| {
                doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(4, 4, 60, 60), &[1.0, 0.0, 0.0, 1.0]);
                Ok(())
            })
            .unwrap();
        app.sync_views();
        begin(&mut app, &ctx, "edit.perspectiveWarp").unwrap();
        app
    }

    fn valid_plane() -> Plane {
        Plane { src: [[10.0, 10.0], [50.0, 10.0], [50.0, 50.0], [10.0, 50.0]], dst: [[12.0, 10.0], [48.0, 8.0], [50.0, 48.0], [12.0, 52.0]] }
    }

    #[test]
    fn invalid_agent_input_is_atomic() {
        let mut app = open_session();
        let before = app.distort.perspective.as_ref().unwrap().describe();
        let bad = Plane { src: [[1.0, 1.0]; 4], dst: [[2.0, 2.0]; 4] };
        assert!(control(&mut app, &json!({"mode": "warp", "planes": [bad]})).unwrap_err().contains("degenerate"));
        assert_eq!(app.distort.perspective.as_ref().unwrap().describe(), before);
        assert!(control(&mut app, &json!({"mode": "unknown"})).unwrap_err().contains("mode"));
        assert_eq!(app.distort.perspective.as_ref().unwrap().describe(), before);
        assert!(control(&mut app, &json!({"mode": 42})).unwrap_err().contains("mode"));
        assert_eq!(app.distort.perspective.as_ref().unwrap().describe(), before);
    }

    #[test]
    fn agent_commit_does_not_claim_success_on_noop_or_invalid_geometry() {
        let mut app = open_session();
        assert_eq!(control(&mut app, &json!({"commit": true})).unwrap()["committed"], json!(false));
        assert!(app.distort.perspective.is_none());

        let mut app = open_session();
        control(&mut app, &json!({"mode": "warp", "planes": [valid_plane()]})).unwrap();
        app.distort.perspective.as_mut().unwrap().planes[0].dst = [[4.0, 4.0]; 4];
        let before_history = app.session.active().unwrap().history.past_len();
        assert!(control(&mut app, &json!({"commit": true})).unwrap_err().contains("degenerate"));
        assert!(app.distort.perspective.is_some(), "an invalid commit must not lose the preview");
        assert_eq!(app.session.active().unwrap().history.past_len(), before_history);
    }

    #[test]
    fn successful_agent_commit_is_one_history_step() {
        let mut app = open_session();
        control(&mut app, &json!({"mode": "warp", "planes": [valid_plane()]})).unwrap();
        let before_history = app.session.active().unwrap().history.past_len();
        let reply = control(&mut app, &json!({"commit": true})).unwrap();
        assert_eq!(reply["committed"], json!(true));
        assert!(app.distort.perspective.is_none());
        assert_eq!(app.session.active().unwrap().history.past_len(), before_history + 1);
    }

    #[test]
    fn engine_failure_is_reported_and_preserves_preview() {
        let mut app = open_session();
        control(&mut app, &json!({"mode": "warp", "planes": [valid_plane()]})).unwrap();
        // An unknown engine command simulates an error after geometry validation.
        app.distort.perspective.as_mut().unwrap().command = "unrecognized.perspective.command".into();
        let before_history = app.session.active().unwrap().history.past_len();
        let error = control(&mut app, &json!({"commit": true})).unwrap_err();
        assert!(!error.is_empty(), "engine error must reach the agent");
        assert!(app.distort.perspective.is_some(), "a failed engine command must retain the preview");
        assert_eq!(app.session.active().unwrap().history.past_len(), before_history);
    }

    #[test]
    fn agent_actions_stay_bound_to_original_document() {
        let mut app = open_session();
        control(&mut app, &json!({"mode": "warp", "planes": [valid_plane()]})).unwrap();
        let preview = app.distort.perspective.as_ref().unwrap().describe();
        let original = app.session.active().unwrap().doc.id;
        app.session.execute("file.new", json!({"width": 72, "height": 72, "depth": 8})).unwrap();
        app.sync_views();
        assert_ne!(app.session.active().unwrap().doc.id, original);
        let other_history = app.session.active().unwrap().history.past_len();
        assert!(
            crate::distort_ui::display_doc(&app, app.session.active_index().unwrap()).is_none(),
            "a preview must never be drawn over an unrelated document"
        );
        assert!(control(&mut app, &json!({"mode": "layout"})).is_err());
        assert_eq!(app.distort.perspective.as_ref().unwrap().describe(), preview);
        assert!(control(&mut app, &json!({"commit": true})).is_err());
        assert_eq!(app.session.active().unwrap().history.past_len(), other_history);
        assert!(app.session.set_active(0));
        assert_eq!(control(&mut app, &json!({"commit": true})).unwrap()["committed"], json!(true));
    }

    #[test]
    fn invalid_identity_plane_does_not_discard_preview() {
        let mut app = open_session();
        let collapsed = Plane { src: [[10.0, 10.0]; 4], dst: [[10.0, 10.0]; 4] };
        app.distort.perspective.as_mut().unwrap().planes.push(collapsed);
        let before_history = app.session.active().unwrap().history.past_len();
        assert!(control(&mut app, &json!({"commit": true})).unwrap_err().contains("degenerate"));
        assert!(app.distort.perspective.is_some(), "a degenerate identity is an error, not an accepted no-op");
        assert_eq!(app.session.active().unwrap().history.past_len(), before_history);
    }

    #[test]
    fn nonfinite_agent_plane_is_rejected() {
        let mut bad = valid_plane();
        bad.dst[0][0] = f64::NAN;
        assert!(validate_planes(&[bad]).unwrap_err().contains("finite"));
    }
}
