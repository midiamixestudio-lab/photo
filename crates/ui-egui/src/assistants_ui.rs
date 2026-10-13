//! Drawing assistants: the canvas overlay, handle drags and the options-bar popup.
//!
//! Guides draw through the document's own
//! [`photocraft_doc::DrawingAssistant::overlay_primitives`] (the UI never computes guide
//! geometry); handle drags commit the `assist.*` commands, coalesced so one drag is one undo
//! step. Strokes themselves snap in the engine pipeline, so the overlay and the paint agree.

use crate::{
    PhotocraftApp,
    canvas::{ToolEvent, ViewXform},
    theme::Tokens,
    widgets,
};
use egui::{Pos2, Stroke};
use photocraft_doc::{AssistantId, DrawingAssistant, DrawingAssistantKind, GuidePrimitive, HandleKind};
use serde_json::json;

/// An assistant handle drag in progress (cleared when the assistant goes away).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssistantDrag {
    pub id: u64,
    pub handle: HandleKind,
}

/// Screen points of tolerance around a handle (`8 / zoom` document pixels).
fn tolerance(xf: &ViewXform) -> f64 {
    8.0 / f64::from(xf.zoom.max(0.001))
}

/// Viewport (document pixels) covering the canvas rect, through rotation and flip.
fn viewport(xf: &ViewXform) -> [f64; 4] {
    let corners = [
        xf.to_doc(xf.rect.min),
        xf.to_doc(Pos2::new(xf.rect.max.x, xf.rect.min.y)),
        xf.to_doc(xf.rect.max),
        xf.to_doc(Pos2::new(xf.rect.min.x, xf.rect.max.y)),
    ];
    [
        corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
        corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
        corners.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
        corners.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
    ]
}

fn shown(app: &PhotocraftApp) -> bool {
    app.ui.view.shows(app.ui.view.show.assistants)
}

/// Every visible assistant's handles as (id, handle, document position).
fn handles(app: &PhotocraftApp, vp: [f64; 4]) -> Vec<(u64, HandleKind, [f64; 2])> {
    let Some(st) = app.session.active() else { return Vec::new() };
    if !st.doc.assistants.enabled {
        return Vec::new();
    }
    st.doc
        .assistants
        .items
        .iter()
        .filter(|a| a.visible)
        .flat_map(|a| {
            a.kind.overlay_primitives(vp).into_iter().filter_map(|p| match p {
                GuidePrimitive::Handle { pos, kind } => Some((a.id.0, kind, pos)),
                GuidePrimitive::Segment { .. } => None,
            })
        })
        .collect()
}

fn drag_of(app: &mut PhotocraftApp) -> Option<AssistantDrag> {
    let d = app.ui.assistant_drag?;
    // The assistant may have been deleted or hidden mid-drag.
    let ok = app.session.active().is_some_and(|st| {
        st.doc.assistants.get(AssistantId(d.id)).is_some_and(|a| {
            a.visible
                && match (d.handle, &a.kind) {
                    (HandleKind::Anchor | HandleKind::Angle, DrawingAssistantKind::StraightLine(_)) => true,
                    (HandleKind::VanishingPoint(i), DrawingAssistantKind::Perspective(p)) => i < p.vps.len(),
                    _ => false,
                }
        })
    });
    if ok {
        Some(d)
    } else {
        app.ui.assistant_drag = None;
        None
    }
}

/// Move the dragged handle to document point `p` (one coalesced history step per drag).
fn drag_to(app: &mut PhotocraftApp, d: AssistantDrag, p: [f64; 2], snap_angle: bool) {
    let anchor = match d.handle {
        HandleKind::Angle => app.session.active().and_then(|st| st.doc.assistants.get(AssistantId(d.id))).and_then(|a| match &a.kind {
            DrawingAssistantKind::StraightLine(l) => Some(l.anchor),
            DrawingAssistantKind::Perspective(_) => None,
        }),
        _ => None,
    };
    let (id, params) = match (d.handle, anchor) {
        (HandleKind::Anchor, _) => ("assist.moveAnchor", json!({"id": d.id, "anchor": p})),
        (HandleKind::Angle, Some(a)) => {
            let mut deg = (p[1] - a[1]).atan2(p[0] - a[0]).to_degrees();
            if snap_angle {
                deg = (deg / 15.0).round() * 15.0;
            }
            ("assist.setAngle", json!({"id": d.id, "angleDeg": deg}))
        }
        (HandleKind::Angle, None) => return,
        (HandleKind::VanishingPoint(i), _) => ("assist.moveVanishingPoint", json!({"id": d.id, "index": i, "point": p})),
    };
    let mut params = params;
    params["coalesce"] = json!("assist-drag");
    let _ = app.run(id, params);
}

/// Canvas pointer handling: drag assistant handles. Returns true while a drag owns the pointer
/// (so a handle grab never starts a stroke).
pub fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) -> bool {
    if drag_of(app).is_none() {
        // Not dragging: only a press on a handle starts one.
        let ToolEvent::Down { x, y, .. } = ev else { return false };
        if !shown(app) || !app.active_tool().is_brushlike() {
            return false;
        }
        let Some(xf) = ViewXform::active(app) else { return false };
        let tol = tolerance(&xf);
        let hit = handles(app, viewport(&xf))
            .into_iter()
            .filter(|(_, _, h)| (h[0] - x).hypot(h[1] - y) <= tol)
            .min_by(|(_, _, a), (_, _, b)| (a[0] - x).hypot(a[1] - y).partial_cmp(&(b[0] - x).hypot(b[1] - y)).unwrap_or(std::cmp::Ordering::Equal));
        match hit {
            Some((id, handle, _)) => {
                app.ui.assistant_drag = Some(AssistantDrag { id, handle });
                true
            }
            None => false,
        }
    } else {
        match ev {
            ToolEvent::Down { x, y, .. } => {
                // Re-grab: switch to whatever handle is under the press.
                app.ui.assistant_drag = None;
                pointer(app, ToolEvent::Down { x, y, pressure: 1.0 }, mods)
            }
            ToolEvent::Move { x, y, .. } => {
                if let Some(d) = drag_of(app) {
                    drag_to(app, d, [x, y], mods.shift);
                }
                true
            }
            ToolEvent::Up { .. } => {
                app.ui.assistant_drag = None;
                true
            }
        }
    }
}

/// Canvas overlay: guide segments and drag handles of every visible assistant.
pub fn draw(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    if !shown(app) {
        return;
    }
    let Some(st) = app.session.active() else { return };
    if !st.doc.assistants.enabled || st.doc.assistants.is_empty() {
        return;
    }
    let vp = viewport(xf);
    let prims = st.doc.assistants.overlay_primitives(vp);
    if prims.is_empty() {
        return;
    }
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(xf.rect));
    let t = Tokens::get(painter.ctx());
    let screen = |p: [f64; 2]| xf.to_screen(p[0] as f32, p[1] as f32);
    let dragging = app.ui.assistant_drag;
    for p in &prims {
        if let GuidePrimitive::Segment { a, b } = *p {
            painter.line_segment([screen(a), screen(b)], Stroke::new(1.0, t.accent));
        }
    }
    for p in &prims {
        if let GuidePrimitive::Handle { pos, kind } = *p {
            let selected = dragging.is_some_and(|d| d.handle == kind && st.doc.assistants.items.iter().any(|a| a.id.0 == d.id && a.visible));
            crate::vector_ui::draw_anchor(&painter, screen(pos), selected, t.accent);
        }
    }
}

/// Options-bar popup: the "Snap to Assistants" switch, magnetism, and assistant management.
pub fn menu(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let count = app.session.active().map(|st| st.doc.assistants.items.len()).unwrap_or(0);
    let on = app.session.active().is_some_and(|st| st.doc.assistants.enabled);
    let resp = crate::icons::button_with_icon_size(ui, "ruler", 24.0, 14.0, on && count > 0, tl!("Drawing assistants: snap strokes to guides"));
    let resp = crate::brush_picker::named(resp, "Drawing assistants: snap strokes to guides");
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(240.0);
        widgets::menu_scroll(ui, |ui| {
            let mut run: Vec<(&str, serde_json::Value)> = Vec::new();
            let (mut enabled, radius) = app
                .session
                .active()
                .map(|st| (st.doc.assistants.enabled, st.doc.assistants.snap_radius))
                .unwrap_or((true, photocraft_doc::DEFAULT_SNAP_RADIUS));
            if ui.checkbox(&mut enabled, tl!("Snap to Assistants")).changed() {
                run.push(("assist.setSnap", json!({"enabled": enabled})));
            }
            ui.horizontal(|ui| {
                ui.label(tl!("Magnetism"));
                let mut px = radius;
                let resp = ui.add(egui::DragValue::new(&mut px).range(0.0..=photocraft_doc::MAX_SNAP_RADIUS).speed(0.5).suffix(" px"));
                if resp.changed() {
                    let mut p = json!({"radius": px});
                    p["coalesce"] = json!("assist-radius");
                    run.push(("assist.setSnap", p));
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(tl!("Straight Line")).clicked() {
                    run.push(("assist.addStraightLine", json!({})));
                    ui.close();
                }
                if ui.button(tl!("Perspective")).clicked() {
                    run.push(("assist.addPerspective", json!({})));
                    ui.close();
                }
            });
            let items: Vec<(u64, String, bool, String)> = app
                .session
                .active()
                .map(|st| {
                    st.doc
                        .assistants
                        .items
                        .iter()
                        .map(|a| {
                            let kind = match &a.kind {
                                DrawingAssistantKind::StraightLine(_) => tl!("Line").to_string(),
                                DrawingAssistantKind::Perspective(_) => tl!("Perspective").to_string(),
                            };
                            (a.id.0, a.name.clone(), a.visible, kind)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !items.is_empty() {
                ui.separator();
            }
            for (id, name, visible, kind) in items {
                ui.horizontal(|ui| {
                    let icon = if visible { "eye" } else { "eye-off" };
                    if crate::icons::button(ui, icon, 22.0, visible, tl!("Show/hide this assistant")).clicked() {
                        run.push(("assist.setVisible", json!({"id": id, "visible": !visible})));
                    }
                    ui.label(format!("{name} ({kind})"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::icons::button(ui, "trash", 22.0, false, tl!("Delete this assistant")).clicked() {
                            run.push(("assist.remove", json!({"id": id})));
                        }
                    });
                });
            }
            for (id, params) in run {
                let _ = app.run(id, params);
            }
        });
    });
}

#[cfg(test)]
mod tests;
