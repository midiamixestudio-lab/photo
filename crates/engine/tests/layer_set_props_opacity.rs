//! `layer.setProps` opacity is on the 0..1 scale (the `layerStyle.*` commands use
//! 0..100). An out-of-range value used to be clamped silently, hiding scripting
//! mistakes behind a plausible-looking opaque layer; it is now an error that
//! names the range, and the layer is left unchanged.

use photocraft_engine::Session;
use serde_json::json;

fn session_with_layer() -> (Session, u64) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 16, "height": 12})).unwrap();
    let id = s.execute("layer.new.layer", json!({"name": "Top"})).unwrap()["layer"].as_u64().unwrap();
    (s, id)
}

fn opacity(s: &Session, id: u64) -> f32 {
    let st = s.active().unwrap();
    st.doc.layers.iter().find(|l| l.id.0 == id).unwrap().opacity
}

#[test]
fn opacity_above_one_is_rejected_and_names_the_range() {
    let (mut s, id) = session_with_layer();
    // 55 is the 0..100-scale value from the bug report.
    let err = s.execute("layer.setProps", json!({"layer": id, "opacity": 55})).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("opacity must be 0..1"), "error names the range: {msg}");
    assert!(msg.contains("55"), "error names the value: {msg}");
    // Nothing was applied: the layer keeps its previous opacity.
    assert_eq!(opacity(&s, id), 1.0);
}

#[test]
fn negative_opacity_is_rejected_and_in_range_values_still_apply() {
    let (mut s, id) = session_with_layer();
    let err = s.execute("layer.setProps", json!({"layer": id, "opacity": -0.5})).unwrap_err();
    assert!(err.to_string().contains("opacity must be 0..1"), "{err}");
    assert_eq!(opacity(&s, id), 1.0);
    for v in [0.0, 0.25, 1.0] {
        s.execute("layer.setProps", json!({"layer": id, "opacity": v})).unwrap();
        assert_eq!(opacity(&s, id), v as f32);
    }
}
