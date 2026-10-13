//! A document's linked smart-object path is read with ambient filesystem access only in a
//! trusted session. An untrusted automation session never follows it: not through a `filter.*`
//! command, a smart-filter edit, or a render, and not when the file lies inside its read root.

use photocraft_automation::{AuthorizedWorkspace, Headless};
use photocraft_engine::doc::{LayerContent, SmartSource};
use serde_json::json;
use std::path::{Path, PathBuf};

/// A fresh scratch directory for one test.
fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("photocraft-linked-sources-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a solid red 8×4 PNG at `path`.
fn write_red_png(path: &Path) {
    let img = photocraft_codecs::Image::from_u8(8, 4, photocraft_codecs::ChannelLayout::Rgb, [255u8, 0, 0].repeat(32)).unwrap();
    std::fs::write(path, photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

/// Writes `dir/parent.pcraft`: one smart object linked to `linked`, carrying stale white cached
/// pixels. Anything that re-renders it from the linked file turns it red.
fn write_parent(dir: &Path, linked: &Path) {
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 4, "background": "white"})).unwrap();
    session.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let state = session.active_mut().unwrap();
    let doc = std::sync::Arc::make_mut(&mut state.doc);
    let LayerContent::Smart(smart) = &mut doc.layers[0].content else { panic!("smart object") };
    smart.source = SmartSource::Linked { path: linked.to_string_lossy().into_owned() };
    std::fs::write(dir.join("parent.pcraft"), photocraft_engine::smart_cmds::encode_source(doc).unwrap()).unwrap();
}

/// The top-left RGBA pixel of the session's active document.
fn top_left(h: &Headless) -> [u8; 4] {
    let png = h.render_png(None, 64).unwrap();
    let rgba = photocraft_codecs::decode(&png).unwrap().to_rgba8();
    [rgba[0], rgba[1], rgba[2], rgba[3]]
}

/// Opens `parent.pcraft`, adds a smart filter, then hides it: the pixel after each step, and
/// whether hiding (which re-renders from the source) succeeded.
fn drive(h: &mut Headless, path: &str) -> (Vec<[u8; 4]>, bool) {
    h.open(Path::new(path)).unwrap();
    let layer = h.inspect(None).unwrap()["layers"][0]["id"].clone();
    let mut seen = vec![top_left(h)];
    h.command_run("filter.blur.gaussianBlur", json!({"layer": layer, "radius": 1.0})).unwrap();
    seen.push(top_left(h));
    let hidden = h.command_run("layer.smartFilter.setVisible", json!({"layer": layer, "index": 0, "visible": false}));
    seen.push(top_left(h));
    (seen, hidden.is_ok())
}

#[test]
fn trusted_sessions_read_the_linked_file() {
    let dir = tmp("trusted");
    write_red_png(&dir.join("secret.png"));
    write_parent(&dir, &dir.join("secret.png"));
    let mut h = Headless::trusted_local();
    assert_eq!(drive(&mut h, dir.join("parent.pcraft").to_str().unwrap()), (vec![WHITE, RED, RED], true));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn untrusted_sessions_never_read_a_linked_file_outside_the_read_root() {
    let dir = tmp("outside");
    let (root, outside) = (dir.join("root"), dir.join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    write_red_png(&outside.join("secret.png"));
    write_parent(&root, &outside.join("secret.png"));
    let mut h = Headless::with_workspace(AuthorizedWorkspace::new(Some(&root), Some(&root)).unwrap());
    // The linked file counts as missing: the re-render fails and the stale cache stays.
    assert_eq!(drive(&mut h, "parent.pcraft"), (vec![WHITE, WHITE, WHITE], false));
    // Release directory capabilities before cleanup on Windows.
    drop(h);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn untrusted_sessions_never_read_a_linked_file_inside_the_read_root_either() {
    // Engine commands take no directory capability, so a linked path is ambient wherever it
    // points; the same rule the Smart Object contents commands already apply.
    let dir = tmp("inside");
    write_red_png(&dir.join("secret.png"));
    write_parent(&dir, &dir.join("secret.png"));
    let mut h = Headless::with_workspace(AuthorizedWorkspace::new(Some(&dir), Some(&dir)).unwrap());
    assert_eq!(drive(&mut h, "parent.pcraft"), (vec![WHITE, WHITE, WHITE], false));
    drop(h);
    std::fs::remove_dir_all(dir).unwrap();
}
