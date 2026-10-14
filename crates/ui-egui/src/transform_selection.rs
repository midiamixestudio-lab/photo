//! Draw the selected-pixel outline through Free Transform without resampling its mask per frame.

use egui::{Pos2, Rect};
use photocraft_algo::transform::Homography;
use photocraft_doc::DocId;

use crate::{PhotocraftApp, canvas::ViewXform, state::TransformSession};

pub(crate) fn session(app: &PhotocraftApp, document: DocId) -> Option<&TransformSession> {
    let t = app.ui.transform.as_ref()?;
    let preview = app.transform_preview.as_ref()?;
    (preview.doc.id == document
        && preview.doc.selection.is_some()
        && !t.selection
        && t.path.is_none()
        && t.content_aware.is_none()
        && !(t.warp.is_some() && crate::transform_tool::warps_type(app)))
    .then_some(t)
}

/// Trace the complete source, including parts currently outside the viewport that a drag can
/// bring into view. Bound the sampling grid; the canvas caches it independently of the quad.
pub(crate) fn trace_step(bounds: photocraft_geom::Rect, mut step: u32) -> u32 {
    step = step.max(1);
    let samples = |s: u32| (u64::from(bounds.width()).div_ceil(u64::from(s)) + 2).saturating_mul(u64::from(bounds.height()).div_ceil(u64::from(s)) + 2);
    while samples(step) > 1_048_576 {
        step *= 2;
    }
    step
}

pub(crate) fn draw(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, document: DocId) {
    let Some(t) = session(app, document) else { return };
    if !app.ui.view.shows(app.ui.view.show.selection_edges) || crate::canvas::polygon_replaces_selection(app) {
        return;
    }
    let Some((id, _, segs)) = &app.outline_cache else { return };
    if *id != document {
        return;
    }
    let Some(h) = Homography::rect_to_quad(t.rect, t.quad) else { return };
    let front = h.front_side(t.rect);
    // Use the same adaptive grid as the pixel preview, including every custom Warp grid line.
    let samples = t.warp.as_ref().map(|w| crate::warp_preview::samples(&w.to_mesh(1, 1), xf.zoom as f64));
    let map = |p: [f64; 2]| -> Option<Pos2> {
        let (x, y) = if let Some(w) = &t.warp {
            w.map(p[0], p[1])
        } else {
            if front * h.w(p[0], p[1]) <= 1e-9 {
                return None;
            }
            h.apply(p[0], p[1])
        };
        let p = xf.to_screen(x as f32, y as f32);
        p.is_finite().then_some(p)
    };
    let lines = segs.iter().flat_map(|&(a, b)| {
        let (a, b) = (a.map(f64::from), b.map(f64::from));
        let axis = usize::from(a[0] == b[0]);
        let cuts = samples.as_ref().map(|(us, vs)| if axis == 0 { us } else { vs });
        let start = t.rect[axis];
        let size = t.rect[axis + 2] - start;
        let cuts = cuts.map_or(&[][..], |cuts| {
            let lo = cuts.partition_point(|v| start + v * size <= a[axis]);
            let hi = cuts.partition_point(|v| start + v * size < b[axis]);
            &cuts[lo..hi.max(lo)]
        });
        cuts.iter()
            .map(move |&v| {
                let mut p = a;
                p[axis] = start + v * size;
                p
            })
            .chain(std::iter::once(b))
            .scan(a, move |previous, next| {
                let line = map(*previous).zip(map(next)).and_then(|(a, b)| clip([a, b], painter.clip_rect()));
                *previous = next;
                Some(line)
            })
            .flatten()
    });
    crate::canvas::marching_ants_screen(painter, lines, painter.ctx().input(|i| i.time));
    painter.ctx().request_repaint_after(std::time::Duration::from_millis(100));
}

/// Clip before emitting dashes: a near-horizon projective edge can be billions of pixels long.
fn clip([a, b]: [Pos2; 2], rect: Rect) -> Option<[Pos2; 2]> {
    let a = [f64::from(a.x), f64::from(a.y)];
    let d = [f64::from(b.x) - a[0], f64::from(b.y) - a[1]];
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for axis in 0..2 {
        let (min, max) = (f64::from(rect.min[axis]), f64::from(rect.max[axis]));
        if d[axis] == 0.0 {
            if a[axis] < min || a[axis] > max {
                return None;
            }
        } else {
            let (x, y) = ((min - a[axis]) / d[axis], (max - a[axis]) / d[axis]);
            lo = lo.max(x.min(y));
            hi = hi.min(x.max(y));
            if lo > hi {
                return None;
            }
        }
    }
    Some([lo, hi].map(|t| egui::pos2((a[0] + t * d[0]) as f32, (a[1] + t * d[1]) as f32)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_horizon_lines_are_clipped_before_dashes_are_generated() {
        let r = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));
        let line = clip([egui::pos2(-1.0e9, 100.0), egui::pos2(1.0e9, 100.0)], r).unwrap();
        assert!(line[0].distance(egui::pos2(0.0, 100.0)) < 0.01);
        assert!(line[1].distance(egui::pos2(800.0, 100.0)) < 0.01);
        assert!(clip([egui::pos2(-100.0, -10.0), egui::pos2(900.0, -10.0)], r).is_none());
    }

    #[test]
    fn large_selection_traces_have_a_bounded_sampling_grid() {
        for bounds in [photocraft_geom::Rect::new(0, 0, 100_000, 100_000), photocraft_geom::Rect::new(-1_000_000, 0, 1_000_000, 1)] {
            let step = trace_step(bounds, 1);
            assert!(step.is_power_of_two());
            let cells = (u64::from(bounds.width()).div_ceil(u64::from(step)) + 2) * (u64::from(bounds.height()).div_ceil(u64::from(step)) + 2);
            assert!(cells <= 1_048_576);
        }
        assert_eq!(trace_step(photocraft_geom::Rect::new(0, 0, 200, 150), 1), 1);
    }
}
