//! Drawing assistants: document-stored snapping guides (resolving #2293).
//!
//! Assistants live in [`photocraft_doc::DrawingAssistants`] on the document, so adding, moving or
//! deleting one is a history step (undo/redo free through [`Session::edit`]) and saves to
//! `.pcraft`. The stroke pipeline ([`snap_points`]) runs every brush point through the set
//! before dab generation, keeping pressure, tilt, rotation, wheel and time untouched. The canvas
//! overlay reads the same set through [`photocraft_doc::DrawingAssistant::overlay_primitives`].

use photocraft_doc::assistants::is_valid_snap_radius;
use photocraft_doc::{AssistantId, DrawingAssistantItem, DrawingAssistantKind, MAX_ASSISTANTS, MAX_SNAP_RADIUS, PerspectiveAssistant, StraightLineAssistant};
use photocraft_paint::StrokePoint;
use serde_json::{Value, json};

use crate::brush_cmds::MAX_COORD;
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

/// Snap `pts` onto an assistant set, preserving every stylus channel but the position.
/// Points that snap outside the brush pipeline's coordinate range (a projection can overshoot
/// its input along a far anchor's line) keep their raw position instead: the pipeline never
/// sees coordinates it did not validate.
pub fn snap_stroke_points(set: &photocraft_doc::DrawingAssistants, pts: &[StrokePoint]) -> Vec<StrokePoint> {
    if !set.enabled || set.items.iter().all(|a| !a.visible) {
        return pts.to_vec();
    }
    pts.iter()
        .map(|p| match set.snap_point([p.x, p.y]) {
            Some(hit) if hit.point[0].abs() <= MAX_COORD && hit.point[1].abs() <= MAX_COORD => StrokePoint { x: hit.point[0], y: hit.point[1], ..*p },
            _ => *p,
        })
        .collect()
}

/// [`snap_stroke_points`] against the active document's assistants (a no-op without one).
pub fn snap_points(s: &Session, pts: &[StrokePoint]) -> Vec<StrokePoint> {
    let Some(st) = s.active() else { return pts.to_vec() };
    snap_stroke_points(&st.doc.assistants, pts)
}

fn id_param(p: &Value, cmd: &str) -> Result<AssistantId> {
    p.get("id").and_then(Value::as_u64).map(AssistantId).ok_or_else(|| bad(cmd, "missing `id`"))
}

fn item_json(a: &DrawingAssistantItem) -> Value {
    let (kind, detail) = match &a.kind {
        DrawingAssistantKind::StraightLine(l) => ("straightLine", json!({"anchor": l.anchor, "angleDeg": l.angle_deg})),
        DrawingAssistantKind::Perspective(p) => (
            "perspective",
            json!({"points": p.vps, "horizonY": p.horizon_y, "rays": p.rays.iter().map(|r| json!({"vp": r.vp, "angleDeg": r.angle_deg})).collect::<Vec<_>>()}),
        ),
    };
    json!({"id": a.id.0, "name": a.name, "visible": a.visible, "kind": kind, "detail": detail})
}

fn list_json(s: &Session) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    Ok(json!({
        "enabled": st.doc.assistants.enabled,
        "snapRadius": st.doc.assistants.snap_radius,
        "items": st.doc.assistants.items.iter().map(item_json).collect::<Vec<_>>(),
    }))
}

fn check_capacity(s: &Session, cmd: &str) -> Result<()> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.assistants.items.len() >= MAX_ASSISTANTS {
        return Err(bad(cmd, format!("the document already holds {MAX_ASSISTANTS} assistants")));
    }
    Ok(())
}

fn next_name(s: &Session, prefix: &str) -> String {
    // Number within the document's assistants of any kind ("Perspective 3").
    let n = s.active().map(|st| st.doc.assistants.items.len() + 1).unwrap_or(1);
    format!("{prefix} {n}")
}

fn name_param(s: &Session, p: &Value, prefix: &str) -> String {
    p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty() && n.len() <= 120).map(str::to_string).unwrap_or_else(|| next_name(s, prefix))
}

fn anchor_param(p: &Value, cmd: &str, key: &str, fallback: [f64; 2]) -> Result<[f64; 2]> {
    match p.get(key) {
        None => Ok(fallback),
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))?;
            let x = a.first().and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))?;
            let y = a.get(1).and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))?;
            Ok([x, y])
        }
    }
}

fn angle_param(p: &Value, cmd: &str, key: &str, fallback: f64) -> Result<f64> {
    match p.get(key) {
        None => Ok(fallback),
        Some(v) => v.as_f64().ok_or_else(|| bad(cmd, format!("`{key}` must be a number (degrees)"))),
    }
}

fn add_straight(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addStraightLine";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let anchor = anchor_param(p, CMD, "anchor", center)?;
    let angle = angle_param(p, CMD, "angleDeg", 0.0)?;
    let line = StraightLineAssistant::new(anchor, angle)
        .ok_or_else(|| bad(CMD, "`anchor` must be finite coordinates within ±1000000 and `angleDeg` a finite angle"))?;
    let name = name_param(s, p, "Straight Line");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::StraightLine(line));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn points_param(p: &Value, cmd: &str, fallback: Vec<[f64; 2]>) -> Result<Vec<[f64; 2]>> {
    match p.get("points") {
        None => Ok(fallback),
        Some(v) => {
            let a = v.as_array().ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
            if a.is_empty() {
                return Err(bad(cmd, "`points` must name at least one vanishing point"));
            }
            a.iter()
                .map(|q| {
                    let pair = q.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
                    let x = pair.first().and_then(Value::as_f64).ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
                    let y = pair.get(1).and_then(Value::as_f64).ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
                    Ok([x, y])
                })
                .collect()
        }
    }
}

fn add_perspective(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addPerspective";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let points = points_param(p, CMD, vec![center])?;
    let horizon = match p.get("horizonY") {
        None => None,
        Some(v) => Some(v.as_f64().ok_or_else(|| bad(CMD, "`horizonY` must be a number"))?),
    };
    let step = match p.get("rayStep") {
        None => 15.0,
        Some(v) => v.as_f64().ok_or_else(|| bad(CMD, "`rayStep` must be a number (degrees)"))?,
    };
    let guide = PerspectiveAssistant::with_default_rays(points, horizon, step)
        .ok_or_else(|| bad(CMD, "need 1–4 finite vanishing points within ±1000000, a finite `horizonY`, and a `rayStep` between 0° and 180°"))?;
    let name = name_param(s, p, "Perspective");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Perspective(guide));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn get_mut(s: &mut Session, cmd: &str, p: &Value) -> Result<AssistantId> {
    let id = id_param(p, cmd)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.assistants.get(id).is_none() {
        return Err(bad(cmd, format!("no assistant with id {}", id.0)));
    }
    Ok(id)
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.remove";
    let id = get_mut(s, CMD, p)?;
    s.edit("Delete Assistant", |doc, _| {
        doc.assistants.items.retain(|a| a.id != id);
        Ok(())
    })?;
    Ok(json!({"removed": id.0}))
}

fn set_visible(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setVisible";
    let id = get_mut(s, CMD, p)?;
    let visible = p.get("visible").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "`visible` must be a boolean"))?;
    s.edit(if visible { "Show Assistant" } else { "Hide Assistant" }, |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            a.visible = visible;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "visible": visible}))
}

fn set_snap(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setSnap";
    let enabled = match p.get("enabled") {
        None => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad(CMD, "`enabled` must be a boolean"))?),
    };
    let radius = match p.get("radius") {
        None => None,
        Some(v) => {
            let r = v.as_f64().ok_or_else(|| bad(CMD, "`radius` must be a number (document pixels)"))?;
            if !is_valid_snap_radius(r) {
                return Err(bad(CMD, format!("`radius` must be between 0 and {MAX_SNAP_RADIUS}")));
            }
            Some(r)
        }
    };
    if enabled.is_none() && radius.is_none() {
        return Err(bad(CMD, "give `enabled` and/or `radius`"));
    }
    let label = match (enabled, radius) {
        (Some(false), _) => "Disable Assistant Snapping",
        _ => "Assistant Snapping",
    };
    s.edit(label, |doc, _| {
        if let Some(e) = enabled {
            doc.assistants.enabled = e;
        }
        if let Some(r) = radius {
            doc.assistants.snap_radius = r;
        }
        Ok(())
    })?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    Ok(json!({"enabled": st.doc.assistants.enabled, "radius": st.doc.assistants.snap_radius}))
}

fn clear(s: &mut Session, _p: &Value) -> Result<Value> {
    s.active().ok_or(EngineError::NoDocument)?;
    s.edit("Delete Assistants", |doc, _| {
        doc.assistants.items.clear();
        Ok(())
    })?;
    Ok(json!({"cleared": true}))
}

/// Move a straight-line assistant's anchor (canvas handle drag; pass `"coalesce": "assist-drag"`
/// so a drag is one undo step).
fn move_anchor(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.moveAnchor";
    let id = get_mut(s, CMD, p)?;
    let anchor = anchor_param(p, CMD, "anchor", [f64::NAN, f64::NAN])?;
    if p.get("anchor").is_none() {
        return Err(bad(CMD, "missing `anchor`"));
    }
    s.edit("Move Assistant", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let DrawingAssistantKind::StraightLine(line) = &mut a.kind else {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no anchor".into() });
        };
        *line = StraightLineAssistant::new(anchor, line.angle_deg)
            .ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: "`anchor` must be finite coordinates within ±1000000".into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "anchor": anchor}))
}

/// Rotate a straight-line assistant about its anchor (canvas angle-handle drag).
fn set_angle(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setAngle";
    let id = get_mut(s, CMD, p)?;
    let angle = angle_param(p, CMD, "angleDeg", f64::NAN)?;
    if p.get("angleDeg").is_none() {
        return Err(bad(CMD, "missing `angleDeg`"));
    }
    s.edit("Rotate Assistant", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let DrawingAssistantKind::StraightLine(line) = &mut a.kind else {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no angle".into() });
        };
        *line = StraightLineAssistant::new(line.anchor, angle)
            .ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: "`angleDeg` must be a finite angle".into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "angleDeg": angle}))
}

/// Move one vanishing point (canvas handle drag); its rays follow.
fn move_vanishing_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.moveVanishingPoint";
    let id = get_mut(s, CMD, p)?;
    let index = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "`index` must be a whole number"))?;
    let point = anchor_param(p, CMD, "point", [f64::NAN, f64::NAN])?;
    if p.get("point").is_none() {
        return Err(bad(CMD, "missing `point`"));
    }
    s.edit("Move Vanishing Point", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let DrawingAssistantKind::Perspective(guide) = &mut a.kind else {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no vanishing points".into() });
        };
        let slot = guide
            .vps
            .get_mut(index as usize)
            .ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: format!("vanishing point {index} does not exist") })?;
        if !(point[0].is_finite()
            && point[1].is_finite()
            && point[0].abs() <= photocraft_doc::MAX_ASSISTANT_COORDINATE
            && point[1].abs() <= photocraft_doc::MAX_ASSISTANT_COORDINATE)
        {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "`point` must be finite coordinates within ±1000000".into() });
        }
        *slot = point;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "point": point}))
}

/// Assistant command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "assist.addStraightLine",
            label: "Add Straight Line Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"anchor":[x,y]?=canvas centre,"angleDeg":degrees=0,"name":string?} — a snapping ruler through `anchor` at `angleDeg`"##,
            enabled: has_doc,
            run: add_straight,
            journal: true,
        },
        CommandSpec {
            id: "assist.addPerspective",
            label: "Add Perspective Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"points":[[x,y],…]?=canvas centre,"horizonY":px?=through the points,"rayStep":degrees=15,"name":string?} — 1- or 2-point perspective snapping"##,
            enabled: has_doc,
            run: add_perspective,
            journal: true,
        },
        CommandSpec {
            id: "assist.list",
            label: "List Drawing Assistants",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: has_doc,
            run: |s, _| list_json(s),
            journal: false,
        },
        CommandSpec {
            id: "assist.remove",
            label: "Delete Drawing Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id}"##,
            enabled: has_doc,
            run: remove,
            journal: true,
        },
        CommandSpec {
            id: "assist.setVisible",
            label: "Show/Hide Drawing Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"visible":bool}"##,
            enabled: has_doc,
            run: set_visible,
            journal: true,
        },
        CommandSpec {
            id: "assist.setSnap",
            label: "Drawing Assistant Snapping",
            menu: &[],
            shortcut: None,
            params: r##"{"enabled":bool?,"radius":document px 0..500?} — master switch and magnetism"##,
            enabled: has_doc,
            run: set_snap,
            journal: true,
        },
        CommandSpec {
            id: "assist.clear",
            label: "Delete All Drawing Assistants",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: has_doc,
            run: clear,
            journal: true,
        },
        CommandSpec {
            id: "assist.moveAnchor",
            label: "Move Assistant Anchor",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"anchor":[x,y]}"##,
            enabled: has_doc,
            run: move_anchor,
            journal: true,
        },
        CommandSpec {
            id: "assist.setAngle",
            label: "Rotate Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"angleDeg":degrees}"##,
            enabled: has_doc,
            run: set_angle,
            journal: true,
        },
        CommandSpec {
            id: "assist.moveVanishingPoint",
            label: "Move Vanishing Point",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":vanishing point,"point":[x,y]}"##,
            enabled: has_doc,
            run: move_vanishing_point,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 100, "background": "white"})).unwrap();
        s
    }

    fn px(s: &Session, x: i32, y: i32) -> Vec<f32> {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
    }

    fn add_horizontal(s: &mut Session, y: f64) -> u64 {
        s.execute("assist.addStraightLine", json!({"anchor": [50.0, y], "angleDeg": 0.0})).unwrap()["id"].as_u64().unwrap()
    }

    /// Five raw pointer samples crossing just below the guide at y = 50.
    fn crossing_points() -> Value {
        json!([[10.0, 53.0], [30.0, 52.0], [50.0, 54.0], [70.0, 51.0], [90.0, 53.0]])
    }

    #[test]
    fn stroke_snaps_to_guide_preserving_pressure_and_tilt() {
        let mut s = session();
        add_horizontal(&mut s, 50.0);
        // Stylus metadata survives snapping untouched.
        let pts = vec![
            StrokePoint { x: 10.0, y: 53.0, pressure: 0.4, tilt_x: 12.0, tilt_y: -7.0, rotation: 30.0, wheel: 0.5, time: 11.0 },
            StrokePoint { x: 90.0, y: 53.0, pressure: 0.9, ..Default::default() },
        ];
        let snapped = snap_points(&s, &pts);
        assert_eq!((snapped[0].x, snapped[0].y), (10.0, 50.0));
        assert_eq!((snapped[1].x, snapped[1].y), (90.0, 50.0));
        for (a, b) in pts.iter().zip(&snapped) {
            assert_eq!((b.pressure, b.tilt_x, b.tilt_y, b.rotation, b.wheel, b.time), (a.pressure, a.tilt_x, a.tilt_y, a.rotation, a.wheel, a.time));
        }
        // The committed stroke paints the guide, not the raw samples (size 3, hard: the raw
        // y ≈ 52–54 would leave y = 50 white).
        s.execute("paint.stroke", json!({"points": crossing_points(), "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0})).unwrap();
        assert!(px(&s, 50, 50)[0] < 0.2, "snapped stroke paints the guide: {:?}", px(&s, 50, 50));
        assert_eq!(px(&s, 50, 80), vec![1.0; 4], "far from the guide stays white");
    }

    #[test]
    fn snapping_off_leaves_points_unadjusted() {
        let mut s = session();
        add_horizontal(&mut s, 50.0);
        s.execute("assist.setSnap", json!({"enabled": false})).unwrap();
        let pts = vec![StrokePoint::new(10.0, 53.0, 0.5)];
        assert_eq!(snap_points(&s, &pts), pts);
        s.execute("paint.stroke", json!({"points": crossing_points(), "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0})).unwrap();
        assert_eq!(px(&s, 50, 50), vec![1.0; 4], "guide row untouched with snapping off");
        assert!(px(&s, 50, 53)[0] < 0.5, "raw samples paint instead: {:?}", px(&s, 50, 53));
        // Hidden assistants are skipped too, while the set stays enabled.
        s.execute("assist.setSnap", json!({"enabled": true})).unwrap();
        let id = s.active().unwrap().doc.assistants.items[0].id.0;
        s.execute("assist.setVisible", json!({"id": id, "visible": false})).unwrap();
        assert_eq!(snap_points(&s, &pts), pts);
    }

    #[test]
    fn perspective_assistant_snaps_strokes_to_rays() {
        let mut s = session();
        // One vanishing point at the canvas centre with the default fan: horizontal/vertical rays.
        s.execute("assist.addPerspective", json!({"points": [[50.0, 50.0]], "rayStep": 90.0})).unwrap();
        // Just below the horizontal ray: snaps up onto y = 50.
        let snapped = snap_points(&s, &[StrokePoint::new(20.0, 53.0, 1.0)]);
        assert_eq!((snapped[0].x, snapped[0].y), (20.0, 50.0));
        // Far from every ray and the horizon: untouched.
        let far = snap_points(&s, &[StrokePoint::new(10.0, 10.0, 1.0)]);
        assert_eq!((far[0].x, far[0].y), (10.0, 10.0));
    }

    #[test]
    fn assistant_config_undoes_redoes_and_lists() {
        let mut s = session();
        let before = s.active().unwrap().history.past_len();
        let id = add_horizontal(&mut s, 50.0);
        assert_eq!(s.execute("assist.list", json!({})).unwrap()["items"].as_array().unwrap().len(), 1);
        assert!(s.undo());
        assert_eq!(s.execute("assist.list", json!({})).unwrap()["items"].as_array().unwrap().len(), 0);
        assert!(s.redo());
        let items = s.execute("assist.list", json!({})).unwrap()["items"].as_array().unwrap().clone();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"].as_u64().unwrap(), id);
        assert_eq!(items[0]["kind"].as_str().unwrap(), "straightLine");
        assert_eq!(s.active().unwrap().history.past_len(), before + 1, "one undo step for the add");
        // Moving the anchor is undoable too.
        s.execute("assist.moveAnchor", json!({"id": id, "anchor": [10.0, 60.0]})).unwrap();
        assert!(s.undo());
        let anchor = s.execute("assist.list", json!({})).unwrap()["items"][0]["detail"]["anchor"].clone();
        assert_eq!(anchor, json!([50.0, 50.0]));
    }

    #[test]
    fn handle_moves_rotate_and_follow_rays() {
        let mut s = session();
        let id = add_horizontal(&mut s, 50.0);
        // Rotate to vertical through the same anchor.
        s.execute("assist.setAngle", json!({"id": id, "angleDeg": 90.0})).unwrap();
        let snapped = snap_points(&s, &[StrokePoint::new(53.0, 20.0, 1.0)]);
        assert_eq!((snapped[0].x, snapped[0].y), (50.0, 20.0));
        // A coalesced drag is one undo step.
        let steps = s.active().unwrap().history.past_len();
        for x in [51.0, 52.0, 53.0] {
            s.execute("assist.moveAnchor", json!({"id": id, "anchor": [x, 50.0], "coalesce": "assist-drag"})).unwrap();
        }
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        assert!(s.undo());
        // Vanishing points move with their rays: shift the VP down 10 px, the horizontal ray
        // follows, and strokes snap to the new ray.
        s.execute("assist.addPerspective", json!({"points": [[50.0, 50.0]], "rayStep": 90.0})).unwrap();
        let pid = s.execute("assist.list", json!({})).unwrap()["items"][1]["id"].as_u64().unwrap();
        s.execute("assist.moveVanishingPoint", json!({"id": pid, "index": 0, "point": [50.0, 60.0], "coalesce": "vp-drag"})).unwrap();
        let snapped = snap_points(&s, &[StrokePoint::new(20.0, 63.0, 1.0)]);
        assert_eq!((snapped[0].x, snapped[0].y), (20.0, 60.0));
    }

    #[test]
    fn snap_radius_boundaries_are_accepted_and_undoable() {
        let mut s = session();
        for radius in [0.0, MAX_SNAP_RADIUS] {
            let before = s.active().unwrap().doc.assistants.snap_radius;
            s.execute("assist.setSnap", json!({"radius": radius})).unwrap();
            assert_eq!(s.active().unwrap().doc.assistants.snap_radius, radius);
            assert!(s.undo());
            assert_eq!(s.active().unwrap().doc.assistants.snap_radius, before);
        }
    }

    #[test]
    fn bad_params_fail_without_history_steps() {
        let mut s = session();
        let steps = s.active().unwrap().history.past_len();
        for (id, params) in [
            ("assist.addStraightLine", json!({"angleDeg": "flat"})),
            ("assist.addStraightLine", json!({"anchor": [0.0]})),
            ("assist.addPerspective", json!({"points": []})),
            ("assist.addPerspective", json!({"rayStep": 0.0})),
            ("assist.addPerspective", json!({"points": [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0], [4.0, 4.0]]})),
            ("assist.remove", json!({})),
            ("assist.remove", json!({"id": 999})),
            ("assist.setVisible", json!({"id": 1})),
            ("assist.setSnap", json!({})),
            ("assist.setSnap", json!({"radius": -1.0})),
            ("assist.setSnap", json!({"radius": MAX_SNAP_RADIUS + 1.0})),
            ("assist.setSnap", json!({"radius": 600.0})),
            ("assist.setSnap", json!({"radius": f64::NAN})),
            ("assist.moveAnchor", json!({"id": 1})),
            ("assist.setAngle", json!({"id": 1})),
            ("assist.moveVanishingPoint", json!({"id": 1, "index": 0})),
        ] {
            assert!(s.execute(id, params).is_err(), "{id}");
        }
        // Wrong-kind handle commands fail gracefully too.
        let straight = add_horizontal(&mut s, 50.0);
        assert!(s.execute("assist.moveVanishingPoint", json!({"id": straight, "index": 0, "point": [0.0, 0.0]})).is_err());
        s.execute("assist.addPerspective", json!({"points": [[10.0, 10.0]]})).unwrap();
        let pid = s.execute("assist.list", json!({})).unwrap()["items"][1]["id"].as_u64().unwrap();
        assert!(s.execute("assist.moveAnchor", json!({"id": pid, "anchor": [0.0, 0.0]})).is_err());
        assert!(s.execute("assist.setAngle", json!({"id": pid, "angleDeg": 10.0})).is_err());
        assert!(s.execute("assist.moveVanishingPoint", json!({"id": pid, "index": 9, "point": [0.0, 0.0]})).is_err());
        assert_eq!(s.active().unwrap().history.past_len(), steps + 2, "only the two valid adds stepped");
    }

    #[test]
    fn live_stroke_preview_matches_the_snapped_commit() {
        use crate::brush_cmds::LiveStroke;
        let mut s = session();
        add_horizontal(&mut s, 50.0);
        let params = json!({"points": [[10.0, 53.0]], "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0});
        let mut live = LiveStroke::begin(&s, &params).unwrap();
        live.push(&[StrokePoint::new(50.0, 54.0, 1.0), StrokePoint::new(90.0, 53.0, 1.0)]).unwrap();
        let seed = live.seed;
        let layer = s.active().unwrap().active_layer.unwrap();
        let expected = live.doc.layer(layer).unwrap().surface().unwrap().rgba(50, 50);
        assert!(expected[0] < 0.2, "preview snaps: {expected:?}");
        s.offer_live_stroke(live).unwrap();
        let mut commit = json!({"points": [[10.0, 53.0], [50.0, 54.0], [90.0, 53.0]], "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0});
        commit["seed"] = json!(seed);
        let r = s.execute("paint.stroke", commit).unwrap();
        assert_eq!(r["fromLive"], json!(true));
        let actual = s.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().rgba(50, 50);
        assert!((0..4).all(|i| (expected[i] - actual[i]).abs() < 0.02), "commit matches preview: {actual:?} vs {expected:?}");
    }
}

#[cfg(test)]
mod retouch_tests {
    use super::super::retouch_cmds::{LiveDab, LiveRetouch};
    use super::*;
    use photocraft_geom::Rect;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 100, "background": "white"})).unwrap();
        s
    }

    fn px(s: &Session, x: i32, y: i32) -> Vec<f32> {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
    }

    fn paint_red_left(s: &mut Session) {
        s.edit("setup", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 50, 100), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
    }

    fn paint_gray(s: &mut Session) {
        s.edit("setup", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 100, 100), &[0.5, 0.5, 0.5, 1.0]);
            Ok(())
        })
        .unwrap();
    }

    fn add_horizontal(s: &mut Session, y: f64) {
        s.execute("assist.addStraightLine", json!({"anchor": [50.0, y], "angleDeg": 0.0})).unwrap();
    }

    #[test]
    fn clone_stamp_snaps_to_guide_and_samples_snapped_offset() {
        let mut s = session();
        paint_red_left(&mut s);
        add_horizontal(&mut s, 50.0);
        let r = s.execute("paint.cloneStamp", json!({"points": [[60, 53], [80, 53]], "source": [10, 50], "size": 8, "hardness": 100})).unwrap();
        assert_eq!(r["offset"], json!([-50, 0]), "the source mapping follows the snapped first point");
        let p = px(&s, 70, 50);
        assert!(p[0] > 0.99 && p[1] < 0.01 && p[3] > 0.99, "cloned red onto the guide: {p:?}");
        assert_eq!(px(&s, 70, 80), vec![1.0; 4], "far from the guide untouched");
    }

    #[test]
    fn dodge_snaps_to_guide() {
        let mut s = session();
        paint_gray(&mut s);
        add_horizontal(&mut s, 50.0);
        s.execute("paint.dodge", json!({"points": [[10, 53], [90, 53]], "size": 4, "hardness": 100})).unwrap();
        assert!(px(&s, 50, 50)[0] > 0.53, "the guide row dodges: {:?}", px(&s, 50, 50));
        assert!((px(&s, 50, 80)[0] - 0.5).abs() < 0.01, "far from the guide untouched");
    }

    #[test]
    fn live_retouch_preview_snaps() {
        let mut s = session();
        paint_red_left(&mut s);
        add_horizontal(&mut s, 50.0);
        let mut live = LiveRetouch::begin(&s, "paint.cloneStamp", &json!({"points": [[60, 53]], "source": [10, 50], "size": 8, "hardness": 100})).unwrap();
        live.push(&[StrokePoint::new(80.0, 53.0, 1.0)]).unwrap();
        assert_eq!(live.points().iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(), vec![[60.0, 50.0], [80.0, 50.0]]);
        let st = s.active().unwrap();
        let p = live.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().rgba(70, 50);
        assert!(p[0] > 0.99 && p[1] < 0.01, "preview clones onto the guide: {p:?}");
    }

    #[test]
    fn live_dab_preview_snaps() {
        let mut s = session();
        paint_gray(&mut s);
        add_horizontal(&mut s, 50.0);
        let mut live = LiveDab::begin(&s, "paint.dodge", &json!({"points": [[50, 53]], "size": 4, "hardness": 100})).unwrap();
        live.push(&[StrokePoint::new(70.0, 53.0, 1.0)]).unwrap();
        let st = s.active().unwrap();
        let surf = live.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        assert!(surf.rgba(50, 50)[0] > 0.53, "preview dodges the guide");
        assert!((surf.rgba(50, 80)[0] - 0.5).abs() < 0.01, "far from the guide untouched");
    }

    #[test]
    fn retouch_snapping_off_leaves_points_unadjusted() {
        let mut s = session();
        paint_red_left(&mut s);
        add_horizontal(&mut s, 50.0);
        s.execute("assist.setSnap", json!({"enabled": false})).unwrap();
        // A small brush (radius 2): the raw y = 53 samples leave the y = 50 row untouched.
        let r = s.execute("paint.cloneStamp", json!({"points": [[60, 53], [80, 53]], "source": [10, 50], "size": 4, "hardness": 100})).unwrap();
        assert_eq!(r["offset"], json!([-50, -3]), "the raw first point maps the source");
        let p = px(&s, 70, 53);
        assert!(p[0] > 0.99 && p[1] < 0.01, "the raw row paints instead: {p:?}");
    }
}
