//! Drawing assistants: snapping guides stored with a document (resolving #2293).
//!
//! An assistant constrains brush strokes to a geometric guide: a straight line at an angle
//! ([`StraightLineAssistant`]) or the rays of vanishing points around a horizon line
//! ([`PerspectiveAssistant`]). Every assistant implements [`DrawingAssistant`], so later kinds
//! (Bézier curves, curvilinear and fisheye grids, #4408) plug into the same stroke pipeline and
//! overlay renderer without touching them: add a variant to [`DrawingAssistantKind`], implement
//! the trait, and extend the engine commands.
//!
//! All coordinates are document pixels. Everything here is pure data plus pure `f64` geometry:
//! no pixels, no UI toolkit, safe on wasm. Invalid inputs (non-finite numbers, out-of-range
//! indices, degenerate lines) snap to nothing and draw nothing rather than failing.

use serde::{Deserialize, Serialize};

use crate::next_id;

/// Identity of one drawing assistant (see [`DrawingAssistantItem`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssistantId(pub u64);

impl AssistantId {
    pub fn fresh() -> Self {
        Self(next_id())
    }
}

/// Largest absolute coordinate an assistant stores (mirrors the brush pipeline's `MAX_COORD`:
/// beyond it dab generation would explode).
pub const MAX_COORDINATE: f64 = 1_000_000.0;
/// How many assistants one document holds (bounds guide-drag hit tests and overlay work).
pub const MAX_ASSISTANTS: usize = 64;
/// How many vanishing points one perspective assistant holds (1- and 2-point perspective today,
/// room for 3-point later).
pub const MAX_VANISHING_POINTS: usize = 4;
/// How many explicit rays one perspective assistant holds.
pub const MAX_RAYS: usize = 360;
/// Default snapping radius in document pixels (Photoshop-like magnetism at 100 % zoom).
pub const DEFAULT_SNAP_RADIUS: f64 = 8.0;
/// Largest snapping radius accepted (a whole-canvas magnet is meaningless and hides mistakes).
pub const MAX_SNAP_RADIUS: f64 = 500.0;

/// Whether a stored snapping radius is finite and within the command and bundle limits.
pub fn is_valid_snap_radius(radius: f64) -> bool {
    radius.is_finite() && (0.0..=MAX_SNAP_RADIUS).contains(&radius)
}

fn finite2(p: [f64; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

fn in_range(p: [f64; 2]) -> bool {
    finite2(p) && p[0].abs() <= MAX_COORDINATE && p[1].abs() <= MAX_COORDINATE
}

/// Unit direction of `angle_deg` (counter-clockwise, document Y pointing down like the rest of
/// the view geometry); `None` for a non-finite angle.
fn dir_from_deg(angle_deg: f64) -> Option<[f64; 2]> {
    if !angle_deg.is_finite() {
        return None;
    }
    let a = angle_deg.to_radians();
    Some([a.cos(), a.sin()])
}

/// `p` projected onto the line through `a` along unit `d`, plus its distance to the line.
fn project(p: [f64; 2], a: [f64; 2], d: [f64; 2]) -> ([f64; 2], f64) {
    let t = (p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1];
    let q = [a[0] + d[0] * t, a[1] + d[1] * t];
    (q, (p[0] - q[0]).hypot(p[1] - q[1]))
}

/// Clip the infinite line through `a` along unit `d` to `viewport` (`[x0, y0, x1, y1]`).
/// `None` when either input is degenerate or the line misses the viewport.
fn clip_line(a: [f64; 2], d: [f64; 2], viewport: [f64; 4]) -> Option<[[f64; 2]; 2]> {
    if !in_range(a) || !finite2(d) || d[0].hypot(d[1]) <= 1e-12 {
        return None;
    }
    let [x0, y0, x1, y1] = viewport;
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite() && x0 < x1 && y0 < y1) {
        return None;
    }
    // Slab method over the parameter t.
    let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
    for (origin, dir, lo_edge, hi_edge) in [(a[0], d[0], x0, x1), (a[1], d[1], y0, y1)] {
        if dir.abs() < 1e-12 {
            if origin < lo_edge || origin > hi_edge {
                return None;
            }
        } else {
            let (t0, t1) = ((lo_edge - origin) / dir, (hi_edge - origin) / dir);
            lo = lo.max(t0.min(t1));
            hi = hi.min(t0.max(t1));
        }
    }
    if !(lo.is_finite() && hi.is_finite() && hi >= lo) {
        return None;
    }
    Some([[a[0] + d[0] * lo, a[1] + d[1] * lo], [a[0] + d[0] * hi, a[1] + d[1] * hi]])
}

/// Clip the ray from `origin` along unit `d` (`t >= 0`) to `viewport`. `None` as in
/// [`clip_line`].
fn clip_ray(origin: [f64; 2], d: [f64; 2], viewport: [f64; 4]) -> Option<[[f64; 2]; 2]> {
    let seg = clip_line(origin, d, viewport)?;
    // Parameter of each endpoint along the ray.
    let param = |p: [f64; 2]| (p[0] - origin[0]) * d[0] + (p[1] - origin[1]) * d[1];
    let (t0, t1) = (param(seg[0]).max(0.0), param(seg[1]).max(0.0));
    let (lo, hi) = (t0.min(t1), t0.max(t1));
    if hi <= 1e-9 {
        return None;
    }
    Some([[origin[0] + d[0] * lo, origin[1] + d[1] * lo], [origin[0] + d[0] * hi, origin[1] + d[1] * hi]])
}

/// Whether a snapping radius is usable: finite and non-negative (0 snaps exact hits only).
fn usable_radius(radius: f64) -> bool {
    radius.is_finite() && radius >= 0.0
}

/// A straight-line (parallel/isometric angle) ruler: strokes snap onto the line through
/// `anchor` at `angle_deg` degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StraightLineAssistant {
    pub anchor: [f64; 2],
    pub angle_deg: f64,
}

impl Default for StraightLineAssistant {
    fn default() -> Self {
        Self { anchor: [0.0, 0.0], angle_deg: 0.0 }
    }
}

impl StraightLineAssistant {
    /// `None` for a non-finite angle or an out-of-range anchor.
    pub fn new(anchor: [f64; 2], angle_deg: f64) -> Option<Self> {
        if !in_range(anchor) || dir_from_deg(angle_deg).is_none() {
            return None;
        }
        Some(Self { anchor, angle_deg })
    }

    fn direction(self) -> Option<[f64; 2]> {
        if !in_range(self.anchor) { None } else { dir_from_deg(self.angle_deg) }
    }
}

/// One explicit guide ray of a perspective assistant: the line through vanishing point `vp`
/// at `angle_deg` degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerspectiveRay {
    pub vp: usize,
    pub angle_deg: f64,
}

/// A 1- or 2-point perspective guide: strokes snap onto the nearest explicit ray through a
/// vanishing point, or onto the horizon line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerspectiveAssistant {
    pub vps: Vec<[f64; 2]>,
    pub horizon_y: Option<f64>,
    pub rays: Vec<PerspectiveRay>,
}

impl PerspectiveAssistant {
    /// `None` when there are no (or too many) vanishing points, a point is out of range, the
    /// horizon is non-finite, a ray is invalid, or a ray names no vanishing point.
    pub fn new(vps: Vec<[f64; 2]>, horizon_y: Option<f64>, rays: Vec<PerspectiveRay>) -> Option<Self> {
        if vps.is_empty() || vps.len() > MAX_VANISHING_POINTS || !vps.iter().all(|p| in_range(*p)) {
            return None;
        }
        if horizon_y.is_some_and(|h| !h.is_finite() || h.abs() > MAX_COORDINATE) {
            return None;
        }
        if rays.len() > MAX_RAYS || rays.iter().any(|r| r.vp >= vps.len() || dir_from_deg(r.angle_deg).is_none()) {
            return None;
        }
        Some(Self { vps, horizon_y, rays })
    }

    /// Vanishing points with a fan of rays every `step_deg` degrees plus the horizon through
    /// the first point (1-point) or through both points' mean (2-point). `step_deg` must be a
    /// finite positive angle; `None` on any invalid input.
    pub fn with_default_rays(vps: Vec<[f64; 2]>, horizon_y: Option<f64>, step_deg: f64) -> Option<Self> {
        if !step_deg.is_finite() || step_deg <= 0.0 || step_deg > 180.0 {
            return None;
        }
        if vps.is_empty() || vps.len() > MAX_VANISHING_POINTS || !vps.iter().all(|p| in_range(*p)) {
            return None;
        }
        let horizon = match horizon_y {
            Some(h) if h.is_finite() && h.abs() <= MAX_COORDINATE => Some(h),
            Some(_) => return None,
            None => {
                let mean = vps.iter().map(|p| p[1]).sum::<f64>() / vps.len() as f64;
                mean.is_finite().then_some(mean)
            }
        };
        let mut rays = Vec::new();
        let mut a = 0.0;
        while a < 180.0 {
            for vp in 0..vps.len() {
                rays.push(PerspectiveRay { vp, angle_deg: a });
            }
            a += step_deg;
        }
        Self::new(vps, horizon, rays)
    }
}

/// The guide geometry of one assistant, extensible without touching the stroke pipeline or the
/// overlay renderer: Bézier and fisheye kinds (#4408) add variants here plus their trait impls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DrawingAssistantKind {
    StraightLine(StraightLineAssistant),
    Perspective(PerspectiveAssistant),
}

/// A visual primitive for the canvas overlay renderer (toolkit-agnostic data, like the tool
/// overlay seam: the UI draws these, it never computes guide geometry itself).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GuidePrimitive {
    /// A guide segment in document pixels.
    Segment { a: [f64; 2], b: [f64; 2] },
    /// An interactive drag handle in document pixels.
    Handle { pos: [f64; 2], kind: HandleKind },
}

/// What a [`GuidePrimitive::Handle`] drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// The straight-line anchor (moves the line).
    Anchor,
    /// The straight-line angle handle (rotates the line about its anchor).
    Angle,
    /// Vanishing point `usize` (moves the point and its rays).
    VanishingPoint(usize),
}

/// The extensible assistant interface: snapping plus overlay, in document pixels.
///
/// Implementors must be total: any input — non-finite points, degenerate geometry loaded from
/// an older or hand-edited file — snaps to nothing and draws nothing, never panics.
pub trait DrawingAssistant: Send + Sync {
    /// The snapped coordinate when `point` is within `snap_radius` of the guide, else `None`.
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]>;
    /// Visual primitives clipped to `viewport` (`[x0, y0, x1, y1]`, document pixels).
    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive>;
}

impl DrawingAssistant for StraightLineAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        let d = self.direction()?;
        let (q, dist) = project(point, self.anchor, d);
        (dist <= snap_radius && finite2(q)).then_some(q)
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        let Some(d) = self.direction() else { return out };
        if let Some([a, b]) = clip_line(self.anchor, d, viewport) {
            out.push(GuidePrimitive::Segment { a, b });
        }
        out.push(GuidePrimitive::Handle { pos: self.anchor, kind: HandleKind::Anchor });
        // Angle handle at a fixed document distance along the line (viewport-independent, so it
        // stays grabbable at any zoom).
        const ANGLE_HANDLE_DIST: f64 = 60.0;
        let angle_pos = [self.anchor[0] + d[0] * ANGLE_HANDLE_DIST, self.anchor[1] + d[1] * ANGLE_HANDLE_DIST];
        if finite2(angle_pos) {
            out.push(GuidePrimitive::Handle { pos: angle_pos, kind: HandleKind::Angle });
        }
        out
    }
}

impl DrawingAssistant for PerspectiveAssistant {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        if !finite2(point) || !usable_radius(snap_radius) {
            return None;
        }
        let mut best: Option<([f64; 2], f64)> = None;
        let mut consider = |q: [f64; 2], dist: f64| {
            if dist <= snap_radius && finite2(q) && best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((q, dist));
            }
        };
        for ray in &self.rays {
            let (Some(vp), Some(d)) = (self.vps.get(ray.vp).copied(), dir_from_deg(ray.angle_deg)) else { continue };
            if !in_range(vp) {
                continue;
            }
            let (q, dist) = project(point, vp, d);
            consider(q, dist);
        }
        if let Some(h) = self.horizon_y.filter(|h| h.is_finite() && h.abs() <= MAX_COORDINATE) {
            consider([point[0], h], (point[1] - h).abs());
        }
        best.map(|(q, _)| q)
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        let mut out = Vec::new();
        for ray in &self.rays {
            let (Some(vp), Some(d)) = (self.vps.get(ray.vp).copied(), dir_from_deg(ray.angle_deg)) else { continue };
            if !in_range(vp) {
                continue;
            }
            if let Some([a, b]) = clip_ray(vp, d, viewport) {
                out.push(GuidePrimitive::Segment { a, b });
            }
        }
        let [x0, _, x1, _] = viewport;
        if let Some(h) = self.horizon_y.filter(|h| h.is_finite() && h.abs() <= MAX_COORDINATE) {
            let [_, y0, _, y1] = viewport;
            if h >= y0 && h <= y1 && x0 < x1 {
                out.push(GuidePrimitive::Segment { a: [x0, h], b: [x1, h] });
            }
        }
        for (i, vp) in self.vps.iter().enumerate() {
            if in_range(*vp) {
                out.push(GuidePrimitive::Handle { pos: *vp, kind: HandleKind::VanishingPoint(i) });
            }
        }
        out
    }
}

impl DrawingAssistant for DrawingAssistantKind {
    fn snap_point(&self, point: [f64; 2], snap_radius: f64) -> Option<[f64; 2]> {
        match self {
            Self::StraightLine(a) => a.snap_point(point, snap_radius),
            Self::Perspective(a) => a.snap_point(point, snap_radius),
        }
    }

    fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        match self {
            Self::StraightLine(a) => a.overlay_primitives(viewport),
            Self::Perspective(a) => a.overlay_primitives(viewport),
        }
    }
}

/// One assistant on the canvas: an id for the commands, a display name, visibility, and its
/// geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingAssistantItem {
    pub id: AssistantId,
    pub name: String,
    pub visible: bool,
    pub kind: DrawingAssistantKind,
}

impl DrawingAssistantItem {
    pub fn new(name: String, kind: DrawingAssistantKind) -> Self {
        Self { id: AssistantId::fresh(), name, visible: true, kind }
    }
}

impl Default for DrawingAssistantItem {
    fn default() -> Self {
        Self { id: AssistantId::fresh(), name: String::new(), visible: true, kind: DrawingAssistantKind::StraightLine(StraightLineAssistant::default()) }
    }
}

/// Where a snapped point came from (for UI feedback and tests).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssistSnap {
    pub point: [f64; 2],
    pub assistant: AssistantId,
}

/// The document's assistant set: a master switch, a shared magnetism radius, and the guides.
/// Saved with the document (`.pcraft`); PSD has no equivalent, so assistants stay document-local
/// on PSD round trips, like the file encoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingAssistants {
    pub enabled: bool,
    pub snap_radius: f64,
    pub items: Vec<DrawingAssistantItem>,
}

impl Default for DrawingAssistants {
    fn default() -> Self {
        Self { enabled: true, snap_radius: DEFAULT_SNAP_RADIUS, items: Vec::new() }
    }
}

impl DrawingAssistants {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: AssistantId) -> Option<&DrawingAssistantItem> {
        self.items.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: AssistantId) -> Option<&mut DrawingAssistantItem> {
        self.items.iter_mut().find(|a| a.id == id)
    }

    /// The nearest snap across every visible assistant, or `None` when snapping is off, the
    /// radius is unusable, or nothing is close enough. First in document order wins ties.
    pub fn snap_point(&self, point: [f64; 2]) -> Option<AssistSnap> {
        self.snap_point_with_radius(point, self.snap_radius)
    }

    /// [`Self::snap_point`] with an explicit radius (the UI passes `8 px / zoom`).
    pub fn snap_point_with_radius(&self, point: [f64; 2], radius: f64) -> Option<AssistSnap> {
        if !self.enabled || !finite2(point) || !usable_radius(radius) {
            return None;
        }
        let mut best: Option<(AssistSnap, f64)> = None;
        for item in self.items.iter().filter(|a| a.visible) {
            if let Some(q) = item.kind.snap_point(point, radius) {
                let dist = (point[0] - q[0]).hypot(point[1] - q[1]);
                if best.is_none_or(|(_, bd)| dist < bd) {
                    best = Some((AssistSnap { point: q, assistant: item.id }, dist));
                }
            }
        }
        best.map(|(hit, _)| hit)
    }

    /// Overlay primitives of every visible assistant, in document order.
    pub fn overlay_primitives(&self, viewport: [f64; 4]) -> Vec<GuidePrimitive> {
        if !self.enabled {
            return Vec::new();
        }
        self.items.iter().filter(|a| a.visible).flat_map(|a| a.kind.overlay_primitives(viewport)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_snap_radius_limits() {
        for radius in [0.0, DEFAULT_SNAP_RADIUS, MAX_SNAP_RADIUS] {
            assert!(is_valid_snap_radius(radius), "{radius}");
        }
        for radius in [-1.0, MAX_SNAP_RADIUS + 1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!is_valid_snap_radius(radius), "{radius}");
        }
    }

    #[test]
    fn perspective_horizon_overlay_matches_coordinate_limits() {
        let viewport = [0.0, -MAX_COORDINATE - 1.0, 100.0, MAX_COORDINATE + 1.0];
        for horizon in [-MAX_COORDINATE, 0.0, MAX_COORDINATE] {
            let p = PerspectiveAssistant { horizon_y: Some(horizon), ..Default::default() };
            assert_eq!(p.overlay_primitives(viewport), vec![GuidePrimitive::Segment { a: [0.0, horizon], b: [100.0, horizon] }]);
            assert_eq!(p.snap_point([50.0, horizon], 0.0), Some([50.0, horizon]));
        }
        for horizon in [-MAX_COORDINATE - 1.0, MAX_COORDINATE + 1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let p = PerspectiveAssistant { horizon_y: Some(horizon), ..Default::default() };
            assert!(p.overlay_primitives(viewport).is_empty(), "{horizon}");
            assert_eq!(p.snap_point([50.0, horizon], 0.0), None);
        }
    }

    fn straight() -> StraightLineAssistant {
        StraightLineAssistant::new([100.0, 100.0], 0.0).expect("valid horizontal line")
    }

    #[test]
    fn test_straight_line_snapping_within_threshold() {
        let a = straight();
        // Directly above the line: projects straight down.
        assert_eq!(a.snap_point([150.0, 104.0], 8.0), Some([150.0, 100.0]));
        // Exactly on the line snaps to itself.
        assert_eq!(a.snap_point([40.0, 100.0], 8.0), Some([40.0, 100.0]));
        // On the radius boundary still snaps.
        assert_eq!(a.snap_point([150.0, 108.0], 8.0), Some([150.0, 100.0]));
        // A 45° line projects diagonally.
        let d = StraightLineAssistant::new([0.0, 0.0], 45.0).expect("valid diagonal");
        let q = d.snap_point([10.0, 0.0], 8.0).expect("snaps");
        assert!((q[0] - 5.0).abs() < 1e-9 && (q[1] - 5.0).abs() < 1e-9, "{q:?}");
    }

    #[test]
    fn test_straight_line_ignores_points_outside_radius() {
        let a = straight();
        assert_eq!(a.snap_point([150.0, 108.1], 8.0), None);
        assert_eq!(a.snap_point([150.0, 300.0], 8.0), None);
        // Zero radius only takes exact hits.
        assert_eq!(a.snap_point([150.0, 100.0], 0.0), Some([150.0, 100.0]));
        assert_eq!(a.snap_point([150.0, 100.1], 0.0), None);
    }

    #[test]
    fn test_perspective_snapping_to_nearest_vanishing_ray() {
        // Two vanishing points on the horizon y = 200, with a horizontal ray each.
        let p = PerspectiveAssistant::new(
            vec![[100.0, 200.0], [700.0, 200.0]],
            Some(200.0),
            vec![PerspectiveRay { vp: 0, angle_deg: 0.0 }, PerspectiveRay { vp: 1, angle_deg: 0.0 }],
        )
        .expect("valid perspective");
        // Near the left VP's ray: snaps onto y = 200.
        assert_eq!(p.snap_point([150.0, 203.0], 8.0), Some([150.0, 200.0]));
        // Between the VPs but closer to the right ray's line: still y = 200 either way, and the
        // projection keeps x.
        assert_eq!(p.snap_point([650.0, 196.0], 8.0), Some([650.0, 200.0]));
        // A diagonal ray wins over the horizon when nearer: ray from VP0 at 45° through (100,200).
        let p = PerspectiveAssistant::new(
            vec![[100.0, 200.0]],
            Some(200.0),
            vec![PerspectiveRay { vp: 0, angle_deg: 45.0 }, PerspectiveRay { vp: 0, angle_deg: 0.0 }],
        )
        .expect("valid perspective");
        // (130, 225) is ~3.5 px from the 45° ray, 25 px from the horizon.
        let q = p.snap_point([130.0, 225.0], 8.0).expect("snaps to the diagonal");
        assert!((q[0] - 127.5).abs() < 1e-9 && (q[1] - 227.5).abs() < 1e-6, "{q:?}");
        // (400, 206) is 6 px from the horizon, far from any ray: horizon wins.
        assert_eq!(p.snap_point([400.0, 206.0], 8.0), Some([400.0, 200.0]));
    }

    #[test]
    fn test_zero_length_line_and_collinear_edge_cases() {
        // Non-finite angles never validate, and loaded garbage draws/snaps nothing.
        assert!(StraightLineAssistant::new([0.0, 0.0], f64::NAN).is_none());
        assert!(StraightLineAssistant::new([0.0, 0.0], f64::INFINITY).is_none());
        assert!(StraightLineAssistant::new([f64::NAN, 0.0], 0.0).is_none());
        assert!(StraightLineAssistant::new([MAX_COORDINATE + 1.0, 0.0], 0.0).is_none());
        let bad = StraightLineAssistant { anchor: [f64::NAN, 0.0], angle_deg: 0.0 };
        assert_eq!(bad.snap_point([10.0, 0.0], 8.0), None);
        assert!(bad.overlay_primitives([0.0, 0.0, 100.0, 100.0]).is_empty());
        let bad_angle = StraightLineAssistant { anchor: [0.0, 0.0], angle_deg: f64::NAN };
        assert_eq!(bad_angle.snap_point([10.0, 0.0], 8.0), None);
        // Non-finite points and radii never snap.
        let a = straight();
        assert_eq!(a.snap_point([f64::NAN, 100.0], 8.0), None);
        assert_eq!(a.snap_point([150.0, 104.0], f64::NAN), None);
        assert_eq!(a.snap_point([150.0, 104.0], -1.0), None);
        assert_eq!(a.snap_point([150.0, 104.0], f64::INFINITY), None);
        // A point collinear with the anchor but far along the line still projects onto it.
        assert_eq!(a.snap_point([10_000.0, 100.0], 8.0), Some([10_000.0, 100.0]));
        // Perspective edge cases: no VPs, too many VPs, dangling ray indices.
        assert!(PerspectiveAssistant::new(vec![], None, vec![]).is_none());
        assert!(PerspectiveAssistant::new(vec![[0.0, 0.0]; MAX_VANISHING_POINTS + 1], None, vec![]).is_none());
        assert!(PerspectiveAssistant::new(vec![[0.0, 0.0]], None, vec![PerspectiveRay { vp: 7, angle_deg: 0.0 }]).is_none());
        let dangling = PerspectiveAssistant { vps: vec![[50.0, 50.0]], horizon_y: None, rays: vec![PerspectiveRay { vp: 3, angle_deg: 0.0 }] };
        assert_eq!(dangling.snap_point([50.0, 53.0], 8.0), None);
        // The dangling ray draws no segment, but the valid vanishing point keeps its handle.
        assert!(dangling.overlay_primitives([0.0, 0.0, 100.0, 100.0]).iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        // Empty perspective (valid shape, no guides) snaps to nothing.
        let empty = PerspectiveAssistant { vps: vec![[50.0, 50.0]], horizon_y: None, rays: vec![] };
        assert_eq!(empty.snap_point([50.0, 53.0], 8.0), None);
    }

    #[test]
    fn default_rays_cover_the_compass_and_horizon() {
        let p = PerspectiveAssistant::with_default_rays(vec![[400.0, 300.0]], None, 90.0).expect("1-point guide");
        assert_eq!(p.horizon_y, Some(300.0), "horizon defaults through the vanishing point");
        assert_eq!(p.rays.len(), 2, "0° and 90° rays");
        assert!(PerspectiveAssistant::with_default_rays(vec![[0.0, 0.0], [800.0, 0.0]], None, 45.0).expect("2-point").rays.len() == 8);
        assert!(PerspectiveAssistant::with_default_rays(vec![[0.0, 0.0]], None, 0.0).is_none(), "zero step rejected");
        assert!(PerspectiveAssistant::with_default_rays(vec![[0.0, 0.0]], None, f64::NAN).is_none());
        assert!(PerspectiveAssistant::with_default_rays(vec![], None, 15.0).is_none(), "no VP rejected");
        assert!(PerspectiveAssistant::with_default_rays(vec![[f64::INFINITY, 0.0]], None, 15.0).is_none());
    }

    #[test]
    fn set_snaps_to_nearest_visible_assistant() {
        let mut set = DrawingAssistants::default();
        let h = DrawingAssistantItem::new("h".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([0.0, 100.0], 0.0).expect("h")));
        let v = DrawingAssistantItem::new("v".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([100.0, 0.0], 90.0).expect("v")));
        let (hid, vid) = (h.id, v.id);
        set.items.extend([h, v]);
        // (103, 103): 3 px from each line; document order wins the tie.
        let hit = set.snap_point([103.0, 103.0]).expect("snaps");
        assert_eq!(hit.assistant, hid);
        assert_eq!(hit.point, [103.0, 100.0]);
        // Hiding the horizontal leaves the vertical.
        set.get_mut(hid).expect("exists").visible = false;
        let hit = set.snap_point([103.0, 103.0]).expect("snaps");
        assert_eq!((hit.assistant, hit.point), (vid, [100.0, 103.0]));
        // Disabled set, or nothing close, snaps nothing.
        set.enabled = false;
        assert_eq!(set.snap_point([103.0, 103.0]), None);
        set.enabled = true;
        assert_eq!(set.snap_point([500.0, 500.0]), None);
        // Zoom-scaled radius: a 2 px document radius misses, 8 px hits.
        assert_eq!(set.snap_point_with_radius([103.0, 103.0], 2.0), None);
        assert_eq!(set.snap_point_with_radius([103.0, 103.0], 8.0).map(|h| h.point), Some([100.0, 103.0]));
    }

    #[test]
    fn overlay_clips_to_viewport() {
        let a = straight();
        let prims = a.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        let seg = prims
            .iter()
            .find_map(|p| match *p {
                GuidePrimitive::Segment { a, b } => Some((a, b)),
                _ => None,
            })
            .expect("a segment");
        assert_eq!(seg, ([0.0, 100.0], [400.0, 100.0]));
        // A line missing the viewport draws no segment, but its handles remain.
        let far = StraightLineAssistant::new([0.0, 5000.0], 0.0).expect("valid");
        let prims = far.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        assert!(prims.iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        // Degenerate viewports draw nothing.
        assert!(a.overlay_primitives([10.0, 10.0, 10.0, 50.0]).iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        assert!(a.overlay_primitives([f64::NAN, 0.0, 400.0, 300.0]).iter().all(|p| matches!(p, GuidePrimitive::Handle { .. })));
        // Perspective: rays leave the VP towards the viewport, VPs get handles.
        let p = PerspectiveAssistant::with_default_rays(vec![[200.0, 150.0]], None, 90.0).expect("guide");
        let prims = p.overlay_primitives([0.0, 0.0, 400.0, 300.0]);
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Segment { .. })));
        assert!(prims.iter().any(|p| matches!(p, GuidePrimitive::Handle { kind: HandleKind::VanishingPoint(0), .. })));
        // A disabled set draws nothing at all.
        let mut set = DrawingAssistants::default();
        set.items.push(DrawingAssistantItem::new("h".into(), DrawingAssistantKind::StraightLine(a)));
        assert!(!set.overlay_primitives([0.0, 0.0, 400.0, 300.0]).is_empty());
        set.enabled = false;
        assert!(set.overlay_primitives([0.0, 0.0, 400.0, 300.0]).is_empty());
    }

    #[test]
    fn assistants_serialize_stably() {
        let mut set = DrawingAssistants::default();
        set.items
            .push(DrawingAssistantItem::new("iso".into(), DrawingAssistantKind::StraightLine(StraightLineAssistant::new([10.0, 20.0], 30.0).expect("valid"))));
        set.items.push(DrawingAssistantItem::new(
            "2pt".into(),
            DrawingAssistantKind::Perspective(PerspectiveAssistant::with_default_rays(vec![[0.0, 300.0], [800.0, 300.0]], None, 45.0).expect("valid")),
        ));
        let json = serde_json::to_value(&set).expect("serializes");
        let back: DrawingAssistants = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, set);
        // Older files without the field load the default.
        let old = serde_json::json!({"enabled": true, "snap_radius": 8.0, "items": []});
        assert_eq!(serde_json::from_value::<DrawingAssistants>(old).expect("loads"), DrawingAssistants::default());
        // Assistant ids are unique.
        assert_ne!(set.items[0].id, set.items[1].id);
    }
}
