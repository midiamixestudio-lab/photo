//! File › New: searchable formats and an editable canvas preview. Values live in the dialog fields (`width`/`height` in pixels,
//! `resolution` in ppi, `mode`, `depth`, `background`, `name`), so `ui.dialog.set` drives it and
//! Create runs `file.new` with them.

use egui::{Align2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Map, Value, json};

use photocraft_engine::prefs::UnitsAndRulers;

use crate::theme::Tokens;
use crate::{icons, widgets};
use photocraft_engine::document_preset_cmds::DocumentSettings;

pub use crate::new_doc_catalog::{CATEGORIES, Preset};

const DEPTH_OPTIONS: &[(u64, &str, &str)] = &[(8, "8 bit", "Integer"), (16, "16 bit", "Integer"), (32, "32 bit (float)", "Floating point")];

/// Width/Height units: (key, label, units per inch; 0 = pixels).
pub const UNITS: &[(&str, &str, f32)] =
    &[("px", "Pixels", 0.0), ("in", "Inches", 1.0), ("cm", "Centimeters", 2.54), ("mm", "Millimeters", 25.4), ("pt", "Points", 72.0), ("pica", "Picas", 6.0)];

/// Pixels to the display unit at `ppi`.
pub fn to_unit(px: f32, unit: &str, ppi: f32) -> f32 {
    match UNITS.iter().find(|u| u.0 == unit) {
        Some((_, _, per_in)) if *per_in > 0.0 => px / ppi.max(1.0) * per_in,
        _ => px,
    }
}

/// Display unit back to pixels at `ppi`.
pub fn from_unit(v: f32, unit: &str, ppi: f32) -> f32 {
    match UNITS.iter().find(|u| u.0 == unit) {
        Some((_, _, per_in)) if *per_in > 0.0 => (v / per_in * ppi.max(1.0)).round(),
        _ => v.round(),
    }
}

/// A typed size as the whole pixel count `file.new` takes (#254: a float like `512.0` isn't one, so
/// the command fell back to its 1920 x 1080 default). Clamped to the command's 1–300000 range.
pub fn px_value(px: f32) -> Value {
    json!(if px.is_finite() { px.round().clamp(1.0, 300_000.0) as u32 } else { 1 })
}

/// Name of the preset that takes the clipboard image's size.
pub const CLIPBOARD: &str = "Clipboard";

/// Offer the Clipboard preset (`w` × `h` px, at 72 ppi) first under the Recent presets, and
/// select it.
pub fn set_clipboard(f: &mut Map<String, Value>, w: u32, h: u32) {
    if w == 0 || h == 0 {
        return;
    }
    f.insert("__clipboard".into(), json!([w, h]));
    apply_preset(f, &(CLIPBOARD, w, h, 72.0), &UnitsAndRulers::default());
}

/// The clipboard image's size, when the dialog offers the Clipboard preset.
fn clipboard_preset(f: &Map<String, Value>) -> Option<Preset> {
    let size = f.get("__clipboard")?.as_array()?;
    let dim = |i: usize| size.get(i)?.as_u64().and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0);
    Some((CLIPBOARD, dim(0)?, dim(1)?, 72.0))
}

/// Set Width or Height (`key`) from a value typed in `unit`: whole pixels for `file.new`, plus the
/// typed value so the field keeps showing it (see [`shown_size`]).
pub fn set_size(f: &mut Map<String, Value>, key: &str, v: f32, unit: &str, ppi: f32) {
    f.insert(key.into(), px_value(from_unit(v, unit, ppi)));
    f.insert(typed_key(key), json!({"value": v, "unit": unit}));
    f.remove("__preset");
    f.remove("__savedPreset");
}

/// What Width or Height (`key`, `d` pixels when unset) shows in `unit`: the value the user typed
/// while it still gives the field's pixel count, else the pixels converted. Converting the rounded
/// pixels back on every keystroke turned a typed `5` mm into 14 px and the text into `4.9` (#1147).
/// A size over the 300000 px limit shows the limit, not the typed value the document won't have.
pub fn shown_size(f: &Map<String, Value>, key: &str, d: f32, unit: &str, ppi: f32) -> f32 {
    let px = get_f(f, key, d);
    let typed = f.get(&typed_key(key)).and_then(Value::as_object).filter(|t| unit != "px" && t.get("unit").and_then(Value::as_str) == Some(unit));
    match typed.and_then(|t| t.get("value")).and_then(Value::as_f64) {
        Some(v) if from_unit(v as f32, unit, ppi).max(1.0) == px => v as f32,
        _ => to_unit(px, unit, ppi),
    }
}

fn typed_key(key: &str) -> String {
    format!("__{key}Typed")
}

/// Swap Width and Height (the Orientation buttons), with the values typed for them, so a typed
/// 841 x 1189 mm turns into 1189 x 841 mm, not 1188.9 x 841.
pub fn swap_size(f: &mut Map<String, Value>) {
    f.remove("__savedPreset");
    let (w, h) = (get_f(f, "width", 1920.0), get_f(f, "height", 1080.0));
    f.insert("width".into(), px_value(h));
    f.insert("height".into(), px_value(w));
    let (typed_w, typed_h) = (f.remove(&typed_key("width")), f.remove(&typed_key("height")));
    if let Some(t) = typed_h {
        f.insert(typed_key("width"), t);
    }
    if let Some(t) = typed_w {
        f.insert(typed_key("height"), t);
    }
}

/// Print and photo presets (listed at 300 ppi) are specified in inches, screen presets in pixels.
fn is_print(p: &Preset) -> bool {
    p.3 >= 300.0
}

/// A built-in preset turned to the orientation picked with the Orientation buttons (#4668), so the
/// cards and what they apply follow it. Square presets and the Clipboard preset stay as they are.
fn oriented(p: &Preset, f: &Map<String, Value>) -> Preset {
    let portrait = match f.get("__orientation").and_then(Value::as_str) {
        Some("portrait") => true,
        Some("landscape") => false,
        _ => return *p,
    };
    if p.0 == CLIPBOARD || p.1 == p.2 || (p.2 > p.1) == portrait {
        return *p;
    }
    (p.0, p.2, p.1, p.3)
}

/// The unit a built-in preset shows and applies: a print preset in the physical unit picked for
/// Width/Height (#4668), else in inches; a screen preset in pixels.
fn preset_unit(p: &Preset, f: &Map<String, Value>) -> String {
    let unit = get_s(f, "__unit", "px");
    if !is_print(p) {
        "px".into()
    } else if unit != "px" && UNITS.iter().any(|u| u.0 == unit) {
        unit
    } else {
        "in".into()
    }
}

/// A built-in preset at Preferences › Units & Rulers › New Document Preset Resolutions, as in
/// Photoshop: a print preset keeps its size in inches at the Print Resolution (Letter at 200 ppi is
/// 1700 x 2200 px), a screen preset keeps its pixels at the Screen Resolution. The Clipboard preset
/// is the copied pixels and keeps its own resolution. A resolution that isn't a positive finite
/// number leaves the preset as listed.
pub fn at_pref_resolutions(p: &Preset, units: &UnitsAndRulers) -> Preset {
    let ppi = (if is_print(p) { units.print_resolution } else { units.screen_resolution }) as f32;
    if p.0 == CLIPBOARD || !(ppi.is_finite() && ppi > 0.0) {
        return *p;
    }
    if !is_print(p) {
        return (p.0, p.1, p.2, ppi);
    }
    let scale = ppi / p.3;
    let px = |v: u32| (v as f32 * scale).round().clamp(1.0, 300_000.0) as u32;
    (p.0, px(p.1), px(p.2), ppi)
}

/// Apply a built-in preset (at the preference resolutions, see [`at_pref_resolutions`]) to the
/// dialog fields.
pub fn apply_preset(f: &mut Map<String, Value>, p: &Preset, units: &UnitsAndRulers) {
    let (name, w, h, ppi) = at_pref_resolutions(p, units);
    let unit = preset_unit(p, f);
    f.remove("__savedPreset");
    f.insert("width".into(), json!(w));
    f.insert("height".into(), json!(h));
    f.insert("resolution".into(), json!(ppi));
    f.insert("__preset".into(), json!(name));
    f.insert("__unit".into(), json!(unit));
}

/// Fields `file.new` takes (drops the dialog's `__` UI keys).
pub fn command_params(f: &Map<String, Value>) -> Value {
    Value::Object(f.iter().filter(|(k, _)| !k.starts_with("__")).map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Set the resolution (pixels/inch) the way Photoshop's New Document does (#758): with Width/Height
/// in a physical unit the physical size is kept and the pixel count changes; in pixels the pixels
/// are kept.
pub fn set_resolution(f: &mut Map<String, Value>, new_ppi: f32) {
    let old_ppi = get_f(f, "resolution", 72.0);
    f.insert("resolution".into(), json!(new_ppi));
    if old_ppi != new_ppi {
        f.remove("__savedPreset");
    }
    if get_s(f, "__unit", "px") == "px" || !(old_ppi > 0.0 && new_ppi > 0.0) || old_ppi == new_ppi {
        return;
    }
    let scale = new_ppi / old_ppi;
    let (w, h) = (get_f(f, "width", 1920.0), get_f(f, "height", 1080.0));
    f.insert("width".into(), px_value(w * scale));
    f.insert("height".into(), px_value(h * scale));
    f.remove("__preset");
    f.remove("__savedPreset");
}

fn get_f(f: &Map<String, Value>, k: &str, d: f32) -> f32 {
    f.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}

fn get_s(f: &Map<String, Value>, k: &str, d: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}

fn small_label(ui: &mut egui::Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(s).size(11.5).color(t.text_dim));
}

/// Lay out a preset card's title centred in `width`: wrapped onto at most two lines, the rest
/// elided, so long translations stay inside the card.
fn card_title(painter: &egui::Painter, title: &str, width: f32, color: egui::Color32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(title.to_owned(), egui::FontId::proportional(12.0), color, width);
    job.wrap.max_rows = 2;
    job.halign = egui::Align::Center;
    painter.layout_job(job)
}

/// Paint a page thumbnail with the preset's aspect ratio.
fn page_icon(ui: &egui::Ui, r: Rect, w: u32, h: u32, t: &Tokens) {
    let s = ((r.width() - 8.0) / w.max(1) as f32).min((r.height() - 8.0) / h.max(1) as f32);
    let page = Rect::from_center_size(r.center(), vec2(w as f32 * s, h as f32 * s));
    ui.painter().rect_stroke(page, t.radius_sm, Stroke::new(1.2, t.text_dim), StrokeKind::Inside);
}

/// The Background Contents menu's entry for a colour of the user's own ([`body`]).
const CUSTOM: &str = "custom";
/// Side of the Background Contents colour swatch.
const SWATCH: f32 = 22.0;

/// The colour the new document's background starts with: white, black, the captured or current
/// toolbox background colour, or a custom `"#rrggbb"`; `None` for Transparent.
fn background_rgb(app: &crate::PhotocraftApp, f: &Map<String, Value>) -> Option<[f32; 3]> {
    let bg = get_s(f, "background", "white");
    match bg.as_str() {
        "transparent" => None,
        "black" => Some([0.0; 3]),
        "backgroundColor" => Some(
            f.get("backgroundColor")
                .and_then(Value::as_array)
                .and_then(|a| Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32]))
                .unwrap_or_else(|| {
                    let b = app.session.tools.background;
                    [b[0], b[1], b[2]]
                }),
        ),
        hex => Some(crate::color_picker_ui::parse_hex(hex).unwrap_or([1.0; 3])),
    }
}

/// Restore a snapshot without replacing the new document's name or its clipboard offer.
pub fn apply_saved_preset(f: &mut Map<String, Value>, preset: &photocraft_engine::document_preset_cmds::DocumentPreset) {
    apply_settings(f, &preset.settings);
    f.insert("__savedPreset".into(), json!(preset.name));
}

/// Restore every setting of a saved or recent document (all but its name).
pub fn apply_settings(f: &mut Map<String, Value>, settings: &DocumentSettings) {
    f.remove("backgroundColor");
    if let Some(params) = settings.command_params().as_object() {
        f.extend(params.clone());
    }
    f.insert("__unit".into(), json!(settings.unit));
    f.insert("__resUnit".into(), json!(settings.resolution_unit));
    f.remove("__savedPreset");
    f.remove("__preset");
    // Typed Width/Height belong to the previous values (`shown_size`).
    f.remove(&typed_key("width"));
    f.remove(&typed_key("height"));
}

/// The dialog's settings as `document.presets.save` / `document.recent.add` take them: the
/// `file.new` params plus the display units.
pub fn document_settings(f: &Map<String, Value>) -> Value {
    let mut settings = command_params(f);
    let unit = get_s(f, "__unit", "px");
    settings["unit"] = json!(if UNITS.iter().any(|u| u.0 == unit) { unit.as_str() } else { "px" });
    settings["resolutionUnit"] = json!(if get_s(f, "__resUnit", "in") == "cm" { "cm" } else { "in" });
    settings
}

/// Whether the dialog currently holds `settings` (so its Recent card shows selected).
fn holds(f: &Map<String, Value>, settings: &DocumentSettings) -> bool {
    // Unset fields hold the dialog's defaults, as the details pane shows them.
    let (w, h) = (get_f(f, "width", 1920.0), get_f(f, "height", 1080.0));
    (w, h, get_f(f, "resolution", 72.0)) == (settings.width as f32, settings.height as f32, settings.resolution)
        && f.get("depth").and_then(Value::as_u64).unwrap_or(8) == u64::from(settings.depth)
        && get_s(f, "mode", "rgb") == settings.mode
        && get_s(f, "background", "white") == settings.background
}

/// A card in the preset grid: a built-in preset or a recently created document.
enum Card {
    Builtin(Preset),
    Recent(DocumentSettings),
}

impl Card {
    /// Pixel size and resolution; a built-in preset at the Units & Rulers preset resolutions.
    fn size(&self, units: &UnitsAndRulers) -> (u32, u32, f32) {
        match self {
            Card::Builtin(listed) => {
                let p = at_pref_resolutions(listed, units);
                (p.1, p.2, p.3)
            }
            Card::Recent(s) => (s.width, s.height, s.resolution),
        }
    }

    /// The preset's name; a recent document takes the name of the built-in preset it matches.
    fn title(&self) -> &'static str {
        match self {
            Card::Builtin(p) => p.0,
            Card::Recent(s) => {
                CATEGORIES.iter().flat_map(|c| c.1.iter()).find(|p| (p.1, p.2, p.3) == (s.width, s.height, s.resolution)).map_or("Custom", |p| p.0)
            }
        }
    }

    /// The size line, in the unit the preset applies (a recent one: the unit it was made in).
    fn size_line(&self, units: &UnitsAndRulers, f: &Map<String, Value>) -> String {
        let (w, h, ppi) = self.size(units);
        let unit = match self {
            Card::Builtin(p) => preset_unit(p, f),
            Card::Recent(s) => s.unit.clone(),
        };
        if unit == "px" {
            format!("{w} x {h} px @ {} ppi", widgets::fmt_num(f64::from(ppi)))
        } else {
            let v = |n: u32| widgets::fmt_num(f64::from(to_unit(n as f32, &unit, ppi)));
            format!("{} x {} {unit} @ {} ppi", v(w), v(h), widgets::fmt_num(f64::from(ppi)))
        }
    }
}

/// The Recent category: the Clipboard preset, the documents recently created here (newest
/// first), then the built-in defaults they don't already cover.
fn recent_cards(f: &Map<String, Value>, recent: &[DocumentSettings], units: &UnitsAndRulers) -> Vec<Card> {
    let mut cards: Vec<Card> = clipboard_preset(f).map(Card::Builtin).into_iter().collect();
    cards.extend(recent.iter().cloned().map(Card::Recent));
    let defaults = CATEGORIES.iter().find(|c| c.0 == "Recent").into_iter().flat_map(|c| c.1.iter()).filter(|p| {
        let (_, w, h, ppi) = at_pref_resolutions(p, units);
        !recent.iter().any(|s| (s.width, s.height, s.resolution) == (w, h, ppi))
    });
    cards.extend(defaults.copied().map(Card::Builtin));
    cards
}

fn saved_presets(app: &mut crate::PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) -> bool {
    let query = get_s(f, "__search", "").to_lowercase();
    let presets: Vec<_> = app.session.presets.documents.iter().filter(|p| p.name.to_lowercase().contains(query.trim())).cloned().collect();
    if presets.is_empty() {
        ui.label(if app.session.presets.documents.is_empty() { tl!("No saved presets yet.") } else { tl!("No matching formats.") });
        return false;
    }
    let mut create = false;
    egui::ScrollArea::vertical().id_salt("saved-document-presets").max_height(320.0).show(ui, |ui| {
        for preset in &presets {
            ui.push_id(&preset.name, |ui| {
                ui.horizontal(|ui| {
                    let selected = get_s(f, "__savedPreset", "") == preset.name;
                    let label = egui::Button::new(&preset.name).selected(selected).truncate();
                    let response = ui.add_sized(vec2((ui.available_width() - 84.0).max(80.0), 32.0), label).on_hover_text(&preset.name);
                    if response.clicked() {
                        apply_saved_preset(f, preset);
                        create |= response.double_clicked() || response.triple_clicked();
                    }
                    if widgets::secondary_button(ui, tl!("Delete"), 76.0).clicked() {
                        match app.run("document.presets.delete", json!({"name":preset.name})) {
                            Ok(_) => {
                                f.remove("__presetError");
                                if selected {
                                    f.remove("__savedPreset");
                                }
                            }
                            Err(e) => {
                                f.insert("__presetError".into(), json!(e.to_string()));
                            }
                        }
                    }
                });
                let s = &preset.settings;
                small_label(ui, &format!("{} × {} px @ {} ppi · {}/{}", s.width, s.height, s.resolution, s.mode.to_uppercase(), s.depth));
                ui.add_space(8.0);
            });
        }
    });
    create
}

fn save_preset(app: &mut crate::PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    ui.add_space(12.0);
    if f.get("__savingPreset").and_then(Value::as_bool) != Some(true) {
        if widgets::secondary_button(ui, tl!("Save Preset…"), 240.0).clicked() {
            f.insert("__savingPreset".into(), json!(true));
            f.remove("__presetError");
        }
    } else {
        // Consume before TextEdit sees Enter, so naming can never confirm the outer dialog.
        let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let label = ui.label(tl!("Preset Name"));
        let mut name = get_s(f, "__presetName", "");
        ui.add(egui::TextEdit::singleline(&mut name).id_salt("document-preset-name").desired_width(240.0).char_limit(255)).labelled_by(label.id);
        f.insert("__presetName".into(), json!(name));
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, tl!("Save"), 110.0).clicked() || enter {
                match app.run("document.presets.save", json!({"name":name,"settings":document_settings(f)})) {
                    Ok(_) => {
                        if let Some(preset) = app.session.presets.documents.last() {
                            apply_saved_preset(f, preset);
                        }
                        f.insert("__category".into(), json!("Saved"));
                        f.insert("__search".into(), json!(""));
                        f.remove("__savingPreset");
                        f.remove("__presetName");
                        f.remove("__presetError");
                        ui.ctx().request_repaint();
                    }
                    Err(e) => {
                        f.insert("__presetError".into(), json!(e.to_string()));
                    }
                }
            }
            if widgets::secondary_button(ui, tl!("Cancel"), 110.0).clicked() {
                f.remove("__savingPreset");
                f.remove("__presetError");
            }
        });
    }
    if let Some(error) = f.get("__presetError").and_then(Value::as_str) {
        ui.add(egui::Label::new(error).wrap());
    }
}

/// Keep the modal within the viewport, including its frame and fixed footer.
pub fn dialog_width(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().width() - 48.0).clamp(280.0, 960.0)
}

/// Keep the listed preset as the application input, while searching its actual dimensions at
/// the preference resolutions. Applying an already-scaled print preset would scale it twice.
fn resolved_catalog(category: &str, query: &str, units: &UnitsAndRulers) -> Vec<Preset> {
    let category = if query.trim().is_empty() { category } else { "All" };
    let lang = crate::i18n::current();
    crate::new_doc_catalog::formats(category, "", lang)
        .into_iter()
        .filter(|p| crate::new_doc_catalog::matches(&at_pref_resolutions(p, units), query, lang))
        .collect()
}

fn resolved_cards(f: &Map<String, Value>, category: &str, query: &str, recent: &[DocumentSettings], units: &UnitsAndRulers) -> Vec<Card> {
    let lang = crate::i18n::current();
    if category == "Recent" {
        return recent_cards(f, recent, units)
            .into_iter()
            .map(|c| match c {
                Card::Builtin(p) => Card::Builtin(oriented(&p, f)),
                recent => recent,
            })
            .filter(|c| {
                let (w, h, ppi) = c.size(units);
                crate::new_doc_catalog::matches(&(c.title(), w, h, ppi), query, lang)
            })
            .collect();
    }
    let mut cards: Vec<Card> = resolved_catalog(if query.trim().is_empty() { category } else { "All" }, "", units)
        .into_iter()
        .map(|p| oriented(&p, f))
        .filter(|p| crate::new_doc_catalog::matches(&at_pref_resolutions(p, units), query, lang))
        .map(Card::Builtin)
        .collect();
    if let Some(p) = clipboard_preset(f)
        .filter(|p| (category == "Popular" || category == "All" || !query.trim().is_empty()) && crate::new_doc_catalog::matches(p, query, lang))
    {
        cards.insert(0, Card::Builtin(p));
    }
    cards
}

fn apply_card(f: &mut Map<String, Value>, card: &Card, units: &UnitsAndRulers) {
    match card {
        Card::Builtin(p) => apply_preset(f, p, units),
        Card::Recent(settings) => apply_settings(f, settings),
    }
}

pub fn body(app: &mut crate::PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) -> bool {
    let t = Tokens::get(ui.ctx());
    let label = ui.label(RichText::new(tl!("Search formats")).color(t.text_dim));
    let search_id = ui.make_persistent_id("new-document-search");
    // Enter in search selects a result. It must never also confirm the outer modal.
    let enter = ui.memory(|m| m.has_focus(search_id)) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    let mut query = get_s(f, "__search", "");
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 80.0).max(100.0);
        let response = ui
            .add(egui::TextEdit::singleline(&mut query).id(search_id).desired_width(width).char_limit(128).hint_text(tl!("Name, size or aspect ratio")))
            .labelled_by(label.id);
        crate::field_tab::register(ui.ctx(), response.id);
        if response.gained_focus() {
            crate::field_tab::select_all(ui.ctx(), response.id, &query);
        }
        if widgets::secondary_button(ui, tl!("Clear"), 64.0).clicked() {
            query.clear();
        }
    });
    f.insert("__search".into(), json!(query));
    ui.add_space(8.0);
    let mut category = get_s(f, "__category", "Popular");
    ui.horizontal_wrapped(|ui| {
        for name in ["Popular", "Recent", "Social Media", "All", "Photo", "Print", "Art & Illustration", "Web", "Mobile", "Film & Video", "Saved"] {
            if ui
                .selectable_label(category == name, RichText::new(tl!(name)).color(if category == name { t.accent_text } else { t.text }))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                category = name.to_owned();
                f.insert("__category".into(), json!(name));
                f.insert("__search".into(), json!(""));
                query.clear();
            }
        }
    });
    ui.add_space(10.0);
    let units = app.session.prefs().units_and_rulers.clone();
    let cards = resolved_cards(f, &category, &query, &app.session.presets.recent_documents, &units);
    if enter {
        if category == "Saved" {
            if let Some(p) = app.session.presets.documents.iter().find(|p| p.name.to_lowercase().contains(query.to_lowercase().trim())).cloned() {
                apply_saved_preset(f, &p);
            }
        } else if let Some(card) = cards.first() {
            apply_card(f, card, &units);
        }
        ui.memory_mut(|m| m.surrender_focus(search_id));
    }
    let mut create = false;
    let total = ui.available_width();
    if total >= 760.0 {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            ui.vertical(|ui| {
                ui.set_width(total - 308.0);
                create |= catalog(app, ui, f, &category, &cards);
            });
            ui.vertical(|ui| {
                ui.set_width(288.0);
                details(app, ui, f);
            });
        });
    } else {
        create |= catalog(app, ui, f, &category, &cards);
        ui.add_space(12.0);
        details(app, ui, f);
    }
    create
}

fn catalog(app: &mut crate::PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>, category: &str, cards: &[Card]) -> bool {
    if category == "Saved" {
        return saved_presets(app, ui, f);
    }
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(crate::i18n::fmt(tl!("BLANK DOCUMENT PRESETS ({n})"), &[("n", &cards.len().to_string())])).size(11.0).color(t.text_faint));
    ui.add_space(8.0);
    if cards.is_empty() {
        ui.label(tl!("No matching formats."));
    }
    let columns = ((ui.available_width() + 10.0) / 174.0).floor().clamp(1.0, 3.0) as usize;
    let card = vec2((ui.available_width() - 10.0 * (columns.saturating_sub(1)) as f32) / columns as f32, 146.0);
    let chosen = get_s(f, "__preset", "");
    let units = app.session.prefs().units_and_rulers.clone();
    let mut create = false;
    egui::ScrollArea::vertical().id_salt("new-document-formats").max_height(330.0).auto_shrink([false, true]).show(ui, |ui| {
        for row in cards.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for c in row {
                    let (w, h, ppi) = c.size(&units);
                    let (r, resp) = ui.allocate_exact_size(card, Sense::click());
                    let on = match c {
                        Card::Builtin(p) => chosen == p.0,
                        Card::Recent(settings) => holds(f, settings),
                    };
                    let t = if on && t.bevel { Tokens { text: t.accent_text, text_dim: t.accent_text, text_faint: t.accent_text, ..t } } else { t };
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, tl!(c.title())));
                    ui.painter().rect_filled(
                        r,
                        t.radius,
                        if on {
                            t.row_selected
                        } else if resp.hovered() {
                            t.hover
                        } else {
                            t.field
                        },
                    );
                    ui.painter().rect_stroke(
                        r,
                        t.radius,
                        Stroke::new(if on || resp.has_focus() { 1.5 } else { 1.0 }, if on || resp.has_focus() { t.accent } else { t.field_border }),
                        StrokeKind::Inside,
                    );
                    page_icon(ui, Rect::from_center_size(pos2(r.center().x, r.top() + 34.0), vec2(72.0, 52.0)), w, h, &t);
                    let title = card_title(ui.painter(), tl!(c.title()), card.x - 16.0, t.text);
                    ui.painter().galley(pos2(r.center().x, r.top() + 82.0 - title.size().y / 2.0), title, t.text);
                    ui.painter().text(
                        pos2(r.center().x, r.top() + 112.0),
                        Align2::CENTER_CENTER,
                        format!("{} × {} px", w, h),
                        crate::theme::mono(11.0),
                        t.text_dim,
                    );
                    let info = if matches!(c, Card::Builtin(p) if is_print(p)) {
                        c.size_line(&units, f)
                    } else {
                        format!("{} · {} ppi", crate::new_doc_catalog::aspect_ratio(w, h), ppi)
                    };
                    ui.painter().text(pos2(r.center().x, r.top() + 132.0), Align2::CENTER_CENTER, info, egui::FontId::proportional(11.0), t.text_faint);
                    let resp = match c {
                        Card::Builtin(_) => resp.on_hover_text(tl!(c.title())),
                        Card::Recent(settings) => {
                            let background = match settings.background.as_str() {
                                "white" => tl!("White"),
                                "black" => tl!("Black"),
                                "transparent" => tl!("Transparent"),
                                "backgroundColor" => tl!("Background Color"),
                                hex => hex,
                            };
                            resp.on_hover_text(format!(
                                "{} · {} · {}/{} · {background}",
                                tl!(c.title()),
                                c.size_line(&units, f),
                                settings.mode.to_uppercase(),
                                settings.depth
                            ))
                        }
                    };
                    if resp.clicked() {
                        apply_card(f, c, &units);
                        create |= resp.double_clicked() || resp.triple_clicked();
                    }
                }
            });
            ui.add_space(10.0);
        }
    });
    create
}

fn preview(ui: &mut egui::Ui, f: &Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let (w, h) = (get_f(f, "width", 1920.0).clamp(1.0, 300_000.0) as u32, get_f(f, "height", 1080.0).clamp(1.0, 300_000.0) as u32);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 118.0), Sense::hover());
    ui.painter().rect_filled(r, t.radius, t.field);
    page_icon(ui, Rect::from_center_size(r.center() - vec2(0.0, 9.0), vec2(132.0, 78.0)), w, h, &t);
    ui.painter().text(
        r.center_bottom() - vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        format!("{} × {} px · {}", w, h, crate::new_doc_catalog::aspect_ratio(w, h)),
        crate::theme::mono(11.0),
        t.text_dim,
    );
    ui.add_space(12.0);
}

fn details(app: &mut crate::PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(t.radius).inner_margin(14.0).show(ui, |ui| {
        ui.set_min_width(260.0);
        preview(ui, f);
        ui.label(RichText::new(tl!("PRESET DETAILS")).size(11.0).color(t.text_faint));
        ui.add_space(4.0);
        let name_label = ui.label(tl!("Name"));
        let raw = get_s(f, "name", "Untitled-1");
        let mut name = if raw == "Untitled-1" { tl!("Untitled-1").to_string() } else { raw };
        let response = ui.add(egui::TextEdit::singleline(&mut name).desired_width(250.0).font(egui::FontId::proportional(15.0))).labelled_by(name_label.id);
        if response.changed() {
            f.insert("name".into(), json!(name));
        }
        crate::field_tab::register(ui.ctx(), response.id);
        if !f.contains_key("__focused") {
            f.insert("__focused".into(), json!(true));
            crate::field_tab::focus(ui.ctx(), response.id);
        }
        if response.gained_focus() {
            crate::field_tab::select_all(ui.ctx(), response.id, &name);
        }
        ui.add_space(8.0);
        let ppi = get_f(f, "resolution", 72.0);
        let mut unit = get_s(f, "__unit", "px");
        let units = widgets::FieldUnits { unit: widgets::unit_of_key(&unit), dpi: f64::from(ppi), ppi: 72.0 };
        small_label(ui, tl!("Width"));
        ui.horizontal(|ui| {
            let mut w = shown_size(f, "width", 1920.0, &unit, ppi);
            if widgets::value_field_units(ui, &mut w, 0.01..=300_000.0, 110.0, widgets::unit_decimals(&unit), units).changed() {
                set_size(f, "width", w, &unit, ppi);
            }
            let opts: Vec<(String, &str)> = UNITS.iter().map(|u| (u.0.to_string(), u.1)).collect();
            if widgets::dropdown(ui, "nd-unit", &mut unit, &opts, 120.0) {
                f.insert("__unit".into(), json!(unit));
                f.remove("__savedPreset");
            }
        });
        small_label(ui, tl!("Height"));
        ui.horizontal(|ui| {
            let mut h = shown_size(f, "height", 1080.0, &unit, ppi);
            if widgets::value_field_units(ui, &mut h, 0.01..=300_000.0, 110.0, widgets::unit_decimals(&unit), units).changed() {
                set_size(f, "height", h, &unit, ppi);
            }
            ui.add_space(6.0);
            small_label(ui, crate::i18n::tr_ctx(crate::i18n::current(), "document", "Orientation"));
            let (w, h) = (get_f(f, "width", 1920.0), get_f(f, "height", 1080.0));
            for (icon, portrait) in [("rectangle-vertical", true), ("rectangle-horizontal", false)] {
                if icons::button(ui, icon, 24.0, (h > w) == portrait, if portrait { "Portrait" } else { "Landscape" }).clicked() {
                    f.insert("__orientation".into(), json!(if portrait { "portrait" } else { "landscape" }));
                    if (h > w) != portrait {
                        swap_size(f);
                    }
                }
            }
        });
        ui.add_space(4.0);
        small_label(ui, tl!("Background Contents"));
        // A custom colour is kept as its "#rrggbb", which `file.new` takes as is (#4789).
        let current = get_s(f, "background", "white");
        let mut bg = if crate::color_picker_ui::parse_hex(&current).is_some() { CUSTOM.to_string() } else { current.clone() };
        let opts = [
            ("white".to_string(), tl!("White")),
            ("black".to_string(), tl!("Black")),
            ("backgroundColor".to_string(), tl!("Background Color")),
            ("transparent".to_string(), tl!("Transparent")),
            (CUSTOM.to_string(), tl!("Custom…")),
        ];
        let shown = background_rgb(app, f);
        let mut pick = false;
        ui.horizontal(|ui| {
            if widgets::dropdown(ui, "nd-bg", &mut bg, &opts, 240.0 - SWATCH - 6.0) {
                if bg == CUSTOM {
                    // Custom… opens the Color Picker on the colour shown so far.
                    pick = true;
                } else {
                    f.insert("background".into(), json!(bg));
                }
                f.remove("__savedPreset");
                // A fresh choice of Background Color uses today's toolbox colour. Selecting a
                // saved preset instead restores the colour captured when it was saved.
                f.remove("backgroundColor");
            }
            // Photoshop's swatch beside the menu: the colour the canvas starts with; a click
            // opens the Color Picker, as Custom… does.
            let (r, resp) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::click());
            match shown {
                Some(c) => {
                    ui.painter().rect_filled(
                        r,
                        2.0,
                        egui::Color32::from_rgb((c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8),
                    );
                }
                None => widgets::checker(ui.painter(), r, 4.0),
            }
            ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, Tokens::get(ui.ctx()).field_border), StrokeKind::Inside);
            let resp = resp.on_hover_text(tl!("Custom background color")).on_hover_cursor(egui::CursorIcon::PointingHand);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Custom background color")));
            if resp.clicked() {
                pick = true;
                f.remove("__savedPreset");
            }
        });
        if pick {
            let start = shown.unwrap_or([1.0; 3]);
            f.insert("background".into(), json!(crate::color_picker_ui::hex(start)));
            f.remove("backgroundColor");
            crate::color_picker_ui::request_field(f, "background", &crate::color_picker_ui::title_for(tl!("Background Contents")));
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new(tl!("Advanced"))
            .id_salt("new-document-advanced")
            .default_open(f.get("__advanced").and_then(Value::as_bool).unwrap_or(false))
            .show(ui, |ui| {
                ui.add_space(4.0);
                small_label(ui, tl!("Resolution"));
                ui.horizontal(|ui| {
                    let per_cm = get_s(f, "__resUnit", "in") == "cm";
                    let mut r = if per_cm { ppi / 2.54 } else { ppi };
                    if widgets::value_field(ui, &mut r, 1.0..=30_000.0, "", 110.0).changed() {
                        set_resolution(f, if per_cm { r * 2.54 } else { r });
                    }
                    let mut ru = get_s(f, "__resUnit", "in");
                    if widgets::dropdown(
                        ui,
                        "nd-resunit",
                        &mut ru,
                        &[("in".to_string(), tl!("Pixels/Inch")), ("cm".to_string(), tl!("Pixels/Centimeter"))],
                        120.0,
                    ) {
                        f.insert("__resUnit".into(), json!(ru));
                        f.remove("__savedPreset");
                    }
                });
                ui.add_space(4.0);
                small_label(ui, tl!("Color Mode"));
                ui.horizontal(|ui| {
                    let mut mode = get_s(f, "mode", "rgb");
                    if widgets::dropdown(
                        ui,
                        "nd-mode",
                        &mut mode,
                        &[
                            ("gray".to_string(), tl!("Grayscale")),
                            ("rgb".to_string(), tl!("RGB Color")),
                            ("cmyk".to_string(), tl!("CMYK Color")),
                            ("lab".to_string(), tl!("Lab Color")),
                        ],
                        110.0,
                    ) {
                        f.insert("mode".into(), json!(mode));
                        f.remove("__savedPreset");
                    }
                    let mut depth = f.get("depth").and_then(Value::as_u64).unwrap_or(8);
                    let depth_options: Vec<(u64, &str, &str)> = DEPTH_OPTIONS.iter().map(|(bits, label, tooltip)| (*bits, *label, *tooltip)).collect();
                    if widgets::dropdown_with_tooltips(ui, "nd-depth", &mut depth, &depth_options, 120.0) {
                        f.insert("depth".into(), json!(depth));
                        f.remove("__savedPreset");
                    }
                });
            });
        save_preset(app, ui, f);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_uses_the_oriented_card_dimensions_and_recent_snapshots_stay_intact() {
        let mut f = crate::state::UiState::new_document_fields();
        f.insert("__orientation".into(), json!("landscape"));
        let units = UnitsAndRulers::default();
        let cards = resolved_cards(&f, "Popular", "1440x1080", &[], &units);
        let instagram = cards.iter().find(|c| c.title() == "Instagram post, 3:4").expect("oriented search result");
        apply_card(&mut f, instagram, &units);
        assert_eq!((f["width"].as_u64(), f["height"].as_u64()), (Some(1440), Some(1080)));
        let recent = DocumentSettings {
            width: 1080,
            height: 1440,
            resolution: 72.0,
            mode: "rgb".into(),
            depth: 8,
            background: "white".into(),
            background_color: None,
            unit: "px".into(),
            resolution_unit: "in".into(),
        };
        let cards = resolved_cards(&f, "Recent", "1080x1440", &[recent], &units);
        assert!(cards.iter().any(|c| matches!(c, Card::Recent(_))));
    }

    #[test]
    fn units_round_trip_through_pixels() {
        assert_eq!(to_unit(2100.0, "in", 300.0), 7.0);
        assert_eq!(from_unit(7.0, "in", 300.0), 2100.0);
        assert_eq!(from_unit(2.54, "cm", 300.0), 300.0);
        assert_eq!(to_unit(640.0, "px", 72.0), 640.0);
    }

    /// #1147: 5 mm at 72 ppi is 14 px, which is 4.94 mm. The field must keep showing the typed 5.
    #[test]
    fn typed_size_in_a_physical_unit_shows_as_typed() {
        let mut f = crate::state::UiState::new_document_fields();
        set_size(&mut f, "height", 5.0, "mm", 72.0);
        assert_eq!(command_params(&f)["height"], json!(14));
        assert_eq!(shown_size(&f, "height", 1080.0, "mm", 72.0), 5.0);
        // Another unit, or pixels changed elsewhere (a preset, the orientation swap): converted.
        assert_eq!(shown_size(&f, "height", 1080.0, "cm", 72.0), to_unit(14.0, "cm", 72.0));
        f.insert("height".into(), json!(1080));
        assert_eq!(shown_size(&f, "height", 1080.0, "mm", 72.0), to_unit(1080.0, "mm", 72.0));
        // Pixels show whole pixels.
        set_size(&mut f, "width", 512.4, "px", 72.0);
        assert_eq!(shown_size(&f, "width", 1920.0, "px", 72.0), 512.0);
        // Over the 300000 px limit the field shows the limit.
        set_size(&mut f, "width", 300_000.0, "mm", 72.0);
        assert_eq!(command_params(&f)["width"], json!(300_000));
        assert_eq!(shown_size(&f, "width", 1920.0, "mm", 72.0), to_unit(300_000.0, "mm", 72.0));
    }

    /// #1147: the Orientation buttons swap the typed values too: 841 x 1189 mm becomes 1189 x 841.
    #[test]
    fn orientation_swap_keeps_the_typed_values() {
        let mut f = crate::state::UiState::new_document_fields();
        set_size(&mut f, "width", 841.0, "mm", 72.0);
        set_size(&mut f, "height", 1189.0, "mm", 72.0);
        swap_size(&mut f);
        let p = command_params(&f);
        assert_eq!((p["width"].clone(), p["height"].clone()), (json!(3370), json!(2384)));
        assert_eq!((shown_size(&f, "width", 1920.0, "mm", 72.0), shown_size(&f, "height", 1080.0, "mm", 72.0)), (1189.0, 841.0));
    }

    #[test]
    fn resolution_keeps_physical_size_in_physical_units_and_pixels_in_px() {
        let a4 = CATEGORIES.iter().find(|c| c.0 == "Print").unwrap().1.iter().find(|p| p.0 == "A4").unwrap();
        let mut f = crate::state::UiState::new_document_fields();
        apply_preset(&mut f, a4, &Default::default());
        f.insert("__unit".into(), json!("in"));
        set_resolution(&mut f, 150.0);
        let p = command_params(&f);
        assert_eq!((p["width"].clone(), p["height"].clone(), p["resolution"].clone()), (json!(1240), json!(1754), json!(150.0)));
        assert!(!f.contains_key("__preset"));

        let mut f = crate::state::UiState::new_document_fields();
        apply_preset(&mut f, a4, &Default::default());
        f.insert("__unit".into(), json!("px"));
        set_resolution(&mut f, 150.0);
        let p = command_params(&f);
        assert_eq!((p["width"].clone(), p["height"].clone(), p["resolution"].clone()), (json!(2480), json!(3508), json!(150.0)));
    }

    /// Units & Rulers › New Document Preset Resolutions: print presets keep their inches at the
    /// Print Resolution, screen presets keep their pixels at the Screen Resolution.
    #[test]
    fn iso_b_paper_presets_use_correct_dimensions_and_selected_units() {
        let presets = CATEGORIES.iter().find(|c| c.0 == "Print").unwrap().1;
        let mut f = crate::state::UiState::new_document_fields();
        f.insert("__unit".into(), json!("mm"));
        for (name, w, h) in [("B3", 4169, 5906), ("B4", 2953, 4169)] {
            let preset = presets.iter().find(|p| p.0 == name).unwrap();
            assert_eq!((preset.1, preset.2, preset.3), (w, h, 300.0));
            assert_eq!(preset_unit(preset, &f), "mm");
            apply_preset(&mut f, preset, &Default::default());
            let params = command_params(&f);
            assert_eq!((params["width"].as_u64(), params["height"].as_u64()), (Some(u64::from(w)), Some(u64::from(h))));
            assert_eq!(params["resolution"], 300.0);
        }
    }

    #[test]
    fn presets_follow_the_preset_resolution_preferences() {
        let find = |cat: &str, name: &str| *CATEGORIES.iter().find(|c| c.0 == cat).unwrap().1.iter().find(|p| p.0 == name).unwrap();
        let (letter, web) = (find("Print", "Letter"), find("Web", "Web Minimum"));
        let stock = UnitsAndRulers::default();
        assert_eq!(at_pref_resolutions(&letter, &stock), letter, "the defaults are the listed sizes");
        assert_eq!(at_pref_resolutions(&web, &stock), web);

        let units = UnitsAndRulers { print_resolution: 200.0, screen_resolution: 96.0, ..stock.clone() };
        assert_eq!(at_pref_resolutions(&letter, &units), ("Letter", 1700, 2200, 200.0), "8.5 x 11 in at 200 ppi");
        assert_eq!(at_pref_resolutions(&find("Print", "A4"), &units), ("A4", 1653, 2339, 200.0));
        assert_eq!(at_pref_resolutions(&web, &units), ("Web Minimum", 1024, 768, 96.0), "pixels kept");
        let clipboard = (CLIPBOARD, 123, 45, 72.0);
        assert_eq!(at_pref_resolutions(&clipboard, &units), clipboard, "the copied pixels keep their resolution");

        let mut f = crate::state::UiState::new_document_fields();
        apply_preset(&mut f, &letter, &units);
        assert_eq!((f["width"].as_u64(), f["height"].as_u64(), f["resolution"].as_f64()), (Some(1700), Some(2200), Some(200.0)));
        assert_eq!((f["__preset"].as_str(), f["__unit"].as_str()), (Some("Letter"), Some("in")), "still a print preset below 300 ppi");
        apply_preset(&mut f, &web, &units);
        assert_eq!((f["width"].as_u64(), f["resolution"].as_f64(), f["__unit"].as_str()), (Some(1024), Some(96.0), Some("px")));

        // Out-of-range values (a hand-edited preferences file) never make an unusable size.
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let units = UnitsAndRulers { print_resolution: bad, screen_resolution: bad, ..stock.clone() };
            assert_eq!(at_pref_resolutions(&letter, &units), letter, "{bad}");
            assert_eq!(at_pref_resolutions(&web, &units), web, "{bad}");
        }
        let huge = UnitsAndRulers { print_resolution: 1e9, ..stock };
        let (_, w, h, _) = at_pref_resolutions(&find("Art & Illustration", "Poster"), &huge);
        assert_eq!((w, h), (300_000, 300_000), "clamped to the file.new limit");
    }

    #[test]
    fn preset_card_titles_fit_the_card_in_every_language() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            for lang in crate::i18n::Lang::all() {
                for p in CATEGORIES.iter().flat_map(|c| c.1.iter()) {
                    let title = crate::i18n::tr(lang, p.0);
                    let g = card_title(ui.painter(), title, 152.0, egui::Color32::WHITE);
                    assert!(g.size().x <= 152.0 && g.rows.len() <= 2 && !g.elided, "{}: {title}", lang.code());
                }
            }
        });
        out.textures_delta.clear();
    }

    #[test]
    fn new_document_depth_labels_and_tooltips_are_translated() {
        // Bit-depth labels are unit/number strings that some languages legitimately write exactly
        // like the English source (Turkish and Italian "8 bit"); they only need a catalog entry there.
        const SAME_AS_SOURCE: &[(&str, &str)] = &[("tr", "8 bit"), ("tr", "16 bit"), ("it", "8 bit"), ("it", "16 bit")];
        for lang in crate::i18n::Lang::all().filter(|lang| lang.code() != "en") {
            for (_, label, tooltip) in DEPTH_OPTIONS {
                if SAME_AS_SOURCE.contains(&(lang.code(), *label)) {
                    assert!(crate::i18n::has(lang, label), "{}: {label}", lang.code());
                } else {
                    assert_ne!(crate::i18n::tr(lang, label), *label, "{}: {label}", lang.code());
                }
                assert_ne!(crate::i18n::tr(lang, tooltip), *tooltip, "{}: {tooltip}", lang.code());
            }
        }
    }

    #[test]
    fn recent_category_lists_clipboard_then_created_documents_then_uncovered_defaults() {
        let settings = |w, h, ppi| DocumentSettings {
            width: w,
            height: h,
            resolution: ppi,
            mode: "rgb".into(),
            depth: 8,
            background: "white".into(),
            background_color: None,
            unit: "px".into(),
            resolution_unit: "in".into(),
        };
        let mut f = Map::new();
        set_clipboard(&mut f, 20, 30);
        let cards = recent_cards(&f, &[settings(800, 600, 72.0), settings(1920, 1080, 72.0)], &UnitsAndRulers::default());
        let titles: Vec<(&str, String)> = cards.iter().map(|c| (c.title(), c.size_line(&UnitsAndRulers::default(), &f))).collect();
        assert_eq!(
            titles,
            [
                (CLIPBOARD, "20 x 30 px @ 72 ppi".to_string()),
                ("Custom", "800 x 600 px @ 72 ppi".to_string()),
                ("HDTV 1080p", "1920 x 1080 px @ 72 ppi".to_string()),
                ("Default PhotoCraft Size", "7 x 5 in @ 300 ppi".to_string()),
            ]
        );
        assert!(holds(&f, &settings(20, 30, 72.0)) && !holds(&f, &settings(800, 600, 72.0)));
    }

    #[test]
    fn clipboard_preset_comes_first_and_is_selected() {
        // Nothing on the clipboard: the dialog opens as before.
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let f = app.new_document_fields();
        assert_eq!(f, crate::state::UiState::new_document_fields());
        assert!(clipboard_preset(&f).is_none());
        // Pixels copied in the app: the Clipboard preset takes their size, at 72 ppi, selected.
        app.run("file.new", json!({"width": 200, "height": 100})).unwrap();
        app.run("select.rect", json!({"x": 10, "y": 20, "width": 123, "height": 45})).unwrap();
        app.run("edit.copy", json!({})).unwrap();
        let f = app.new_document_fields();
        assert_eq!(clipboard_preset(&f), Some((CLIPBOARD, 123, 45, 72.0)));
        assert_eq!((f["width"].as_u64(), f["height"].as_u64(), f["__preset"].as_str()), (Some(123), Some(45), Some(CLIPBOARD)));
        let p = command_params(&f);
        assert!(p.get("__clipboard").is_none(), "file.new never sees the dialog's keys");
        assert_eq!((p["width"].as_u64(), p["height"].as_u64(), p["resolution"].as_f64()), (Some(123), Some(45), Some(72.0)));
    }

    #[test]
    fn preset_sets_size_resolution_and_create_params_drop_ui_keys() {
        let mut f = crate::state::UiState::new_document_fields();
        let a4 = CATEGORIES.iter().find(|c| c.0 == "Print").unwrap().1.iter().find(|p| p.0 == "A4").unwrap();
        apply_preset(&mut f, a4, &Default::default());
        let p = command_params(&f);
        assert_eq!(p["width"], 2480);
        assert_eq!(p["height"], 3508);
        assert_eq!(p["resolution"], 300.0);
        assert!(p.get("__preset").is_none() && p.get("__unit").is_none());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", p).unwrap();
        let d = &s.active().unwrap().doc;
        assert_eq!((d.size.width, d.size.height, d.resolution_dpi), (2480, 3508, 300.0));
    }

    #[test]
    fn saved_selection_restores_all_settings_but_keeps_the_document_name() {
        let mut session = photocraft_engine::Session::new();
        session
            .execute(
                "document.presets.save",
                json!({"name":"Print proof","settings":{
                    "width":1600,"height":800,"resolution":254.0,"mode":"cmyk","depth":16,
                    "background":"backgroundColor","backgroundColor":[0.2,0.3,0.4],"unit":"mm","resolutionUnit":"cm"
                }}),
            )
            .unwrap();
        let preset = session.presets.documents[0].clone();
        let mut f = crate::state::UiState::new_document_fields();
        set_clipboard(&mut f, 20, 30);
        f.insert("name".into(), json!("Catalog cover"));
        apply_saved_preset(&mut f, &preset);
        assert_eq!(f["name"], "Catalog cover");
        assert_eq!(f["__unit"], "mm");
        assert_eq!(f["__resUnit"], "cm");
        assert_eq!(f["__savedPreset"], "Print proof");
        assert!(f.get("__preset").is_none());
        assert_eq!(clipboard_preset(&f), Some((CLIPBOARD, 20, 30, 72.0)));
        let mut expected = preset.settings.command_params();
        expected["name"] = json!("Catalog cover");
        assert_eq!(command_params(&f), expected);
        f.insert("width".into(), json!(42));
        assert_eq!(session.presets.documents[0], preset, "editing the form does not edit the snapshot");
        apply_preset(&mut f, &(CLIPBOARD, 20, 30, 72.0), &Default::default());
        assert!(f.get("__savedPreset").is_none());
        assert_eq!((f["width"].as_u64(), f["height"].as_u64()), (Some(20), Some(30)));
    }
    /// The real dialog (#254): a typed size must reach `file.new`, however it is confirmed.
    mod dialog {
        use super::super::{CATEGORIES, apply_preset};
        use crate::PhotocraftApp;
        use crate::state::{DialogKind, UiState};
        use egui::accesskit::Role;
        use egui_kittest::{Harness, kittest::Queryable};
        use std::sync::{Arc, Mutex};

        /// The real preference services, backed by an in-memory store shared across app restarts.
        fn harness_with_store(store: &Arc<Mutex<Option<String>>>) -> Harness<'static, PhotocraftApp> {
            let (read, write) = (store.clone(), store.clone());
            let services = crate::Services {
                load_prefs: Some(Box::new(move || read.lock().unwrap().clone())),
                save_prefs: Some(Box::new(move |text| {
                    *write.lock().unwrap() = Some(text.to_string());
                    Ok(())
                })),
                ..Default::default()
            };
            let app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
            let mut h = Harness::builder().with_max_steps(64).with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_ui_state(
                |ui, app| {
                    crate::prefs_ui::tick(app, ui.ctx());
                    crate::dialogs::show(app, ui.ctx());
                },
                app,
            );
            PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
            h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            h.run();
            h
        }

        /// #4789: the swatch beside Background Contents (and Custom…) opens the Color Picker; its
        /// colour becomes the new document's background (kept as its "#rrggbb").
        #[test]
        fn a_custom_background_color_is_picked_and_used() {
            let mut h = harness_with_store(&Arc::new(Mutex::new(None)));
            let nd = h.state().ui.dialogs[0].id;
            assert!(h.query_by_label("Custom background color").is_some());
            h.get_by_label("Custom background color").click();
            h.run_steps(3);
            let picker = h
                .state()
                .ui
                .dialogs
                .iter()
                .find(|d| d.fields.get("__dialog").and_then(serde_json::Value::as_u64) == Some(nd))
                .map(|d| d.id)
                .expect("the Color Picker opens over the dialog");
            h.state_mut().ui.dialog_mut(picker).unwrap().fields.insert("color".into(), serde_json::json!("#336699"));
            crate::dialogs::confirm(h.state_mut(), picker).unwrap();
            h.run_steps(3);
            let bg = h.state().ui.dialogs.iter().find(|d| d.id == nd).unwrap().fields["background"].clone();
            assert_eq!(bg, "#336699");
            crate::dialogs::confirm(h.state_mut(), nd).unwrap();
            let doc = &h.state().session.active().unwrap().doc;
            let px = photocraft_compose::flatten(doc).px[0];
            assert!((px[0] * 255.0 - 51.0).abs() < 1.0 && (px[1] * 255.0 - 102.0).abs() < 1.0 && (px[2] * 255.0 - 153.0).abs() < 1.0, "{px:?}");
        }

        /// #4789: the swatch shows what each Background Contents choice paints, Background Color included.
        #[test]
        fn the_swatch_shows_each_background_choice() {
            let mut h = harness_with_store(&Arc::new(Mutex::new(None)));
            h.state_mut().run("tools.setColors", serde_json::json!({"foreground": "#000000", "background": "#336699"})).unwrap();
            let rgb = |bg: &str| super::super::background_rgb(h.state(), serde_json::json!({ "background": bg }).as_object().unwrap());
            assert_eq!(rgb("white"), Some([1.0; 3]));
            assert_eq!(rgb("black"), Some([0.0; 3]));
            assert_eq!(rgb("transparent"), None);
            assert_eq!(rgb("#ff0000"), Some([1.0, 0.0, 0.0]));
            let c = rgb("backgroundColor").expect("Background Color shows the toolbox colour");
            assert!((c[0] * 255.0 - 51.0).abs() < 1.0 && (c[1] * 255.0 - 102.0).abs() < 1.0 && (c[2] * 255.0 - 153.0).abs() < 1.0, "{c:?}");
        }

        fn name_preset(h: &mut Harness<'static, PhotocraftApp>, name: &str) {
            h.get_by_label("Save Preset…").click();
            h.run();
            h.get_by_role_and_label(Role::TextInput, "Preset Name").click();
            h.run_steps(1);
            h.event(egui::Event::Text(name.into()));
            h.run_steps(1);
        }

        #[test]
        fn save_restart_select_create_and_delete_through_the_dialog() {
            let store = Arc::new(Mutex::new(None));
            let mut h = harness_with_store(&store);
            type_into(&mut h, 0, "1600");
            type_into(&mut h, 1, "1600");
            let mut f = fields(&h);
            f.insert("background".into(), serde_json::json!("transparent"));
            set_fields(&mut h, f);
            name_preset(&mut h, " Product square ");
            h.get_by_label("Save").click();
            h.run();
            assert!(h.state().session.documents().is_empty(), "saving does not create a document");
            assert_eq!(h.state().session.presets.documents[0].name, "Product square");
            type_into(&mut h, 0, "32");
            assert_eq!(h.state().session.presets.documents[0].settings.width, 1600);
            drop(h);

            let mut h = harness_with_store(&store);
            h.get_by_label("Saved").click();
            h.run();
            h.get_by_label("Product square").click();
            h.run();
            assert!(h.state().session.documents().is_empty(), "selection only populates the form");
            assert_eq!(fields(&h)["name"], "Untitled-1");
            assert_eq!(fields(&h)["background"], "transparent");
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (1600, 1600, 72.0));
            let pixel = h.state_mut().session.execute("document.pixel", serde_json::json!({"x":0,"y":0})).unwrap();
            assert_eq!(pixel[3], 0.0);

            h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            h.run();
            h.get_by_label("Saved").click();
            h.run();
            h.get_by_label("Delete").click();
            h.run();
            drop(h);
            let mut h = harness_with_store(&store);
            h.get_by_label("Saved").click();
            h.run();
            assert!(h.state().session.presets.documents.is_empty());
            assert!(h.query_by_label("No saved presets yet.").is_some());
        }

        #[test]
        fn enter_saves_the_preset_and_bad_names_leave_the_form_open() {
            let mut h = harness();
            name_preset(&mut h, "Enter test");
            enter(&mut h);
            assert_eq!(h.state().session.presets.documents.len(), 1);
            assert!(h.state().session.documents().is_empty(), "Enter must not also press Create");
            assert_eq!(h.state().ui.dialogs.len(), 1);
            name_preset(&mut h, " ENTER TEST ");
            h.get_by_label("Save").click();
            h.run();
            assert!(fields(&h).get("__presetError").is_some());
            assert_eq!(h.state().session.presets.documents.len(), 1);
            assert!(h.state().session.documents().is_empty());
            h.get_by_label("Cancel").click();
            h.run();
            name_preset(&mut h, "");
            // Cancel keeps the entered name; clear it as an automation edit, then use the button.
            h.state_mut().ui.dialogs[0].fields.insert("__presetName".into(), serde_json::json!("   "));
            h.run();
            h.get_by_label("Save").click();
            h.run();
            assert!(fields(&h).get("__presetError").is_some());
            assert_eq!(h.state().session.presets.documents.len(), 1);
        }

        fn harness() -> Harness<'static, PhotocraftApp> {
            let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            let mut h = Harness::builder()
                .with_max_steps(64)
                .with_size(egui::vec2(1400.0, 900.0))
                .with_step_dt(1.0 / 60.0)
                .build_ui_state(|ui, app| crate::dialogs::show(app, ui.ctx()), app);
            PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
            h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            h.run();
            h
        }

        fn click_at(h: &mut Harness<'static, PhotocraftApp>, at: egui::Pos2) {
            h.hover_at(at);
            h.run_steps(1);
            h.drag_at(at);
            h.run_steps(1);
            h.drop_at(at);
            h.run();
        }

        /// The Width (0), Height (1) and Resolution (2) fields.
        fn field(h: &Harness<'static, PhotocraftApp>, i: usize) -> egui::Rect {
            h.query_all_by_role(Role::SpinButton).nth(i).map(|n| n.rect()).expect("a size field")
        }

        /// Click into field `i` (which selects its text) and type `text`, as a user does.
        fn type_into(h: &mut Harness<'static, PhotocraftApp>, i: usize, text: &str) {
            if i == 2 && h.query_all_by_role(Role::SpinButton).count() == 2 {
                h.get_by_label("Advanced").click();
                h.run();
            }
            let r = field(h, i);
            click_at(h, r.center());
            for c in text.chars() {
                h.event(egui::Event::Text(c.to_string()));
                h.run_steps(1);
            }
        }

        fn fields(h: &Harness<'static, PhotocraftApp>) -> serde_json::Map<String, serde_json::Value> {
            h.state().ui.dialogs.first().map(|d| d.fields.clone()).expect("the dialog is open")
        }

        fn set_fields(h: &mut Harness<'static, PhotocraftApp>, f: serde_json::Map<String, serde_json::Value>) {
            h.state_mut().ui.dialogs[0].fields = f;
            h.run();
        }

        fn enter(h: &mut Harness<'static, PhotocraftApp>) {
            h.key_press(egui::Key::Enter);
            h.run();
        }

        fn created(h: &Harness<'static, PhotocraftApp>) -> (u32, u32, f32) {
            assert!(h.state().ui.dialogs.is_empty(), "the dialog closed");
            let d = &h.state().session.active().expect("a new document").doc;
            (d.size.width, d.size.height, d.resolution_dpi)
        }

        fn search(h: &mut Harness<'static, PhotocraftApp>, query: &str) {
            h.get_by_role_and_label(Role::TextInput, "Search formats").click();
            h.run_steps(1);
            h.event(egui::Event::Text(query.into()));
            h.run();
        }

        #[test]
        fn search_enter_selects_social_size_without_creating_until_create() {
            let mut h = harness();
            let mut f = fields(&h);
            f.insert("name".into(), serde_json::json!("Campaign"));
            f.insert("depth".into(), serde_json::json!(16));
            f.insert("background".into(), serde_json::json!("transparent"));
            set_fields(&mut h, f);
            h.get_by_label("Print").click();
            h.run();
            search(&mut h, "tiktok");
            assert!(h.query_by_role_and_label(Role::Button, "Stories / Reels / Shorts").is_some());
            enter(&mut h);
            assert!(h.state().session.documents().is_empty());
            assert_eq!(fields(&h)["__preset"], "Stories / Reels / Shorts");
            assert_eq!(fields(&h)["name"], "Campaign");
            assert_eq!(fields(&h)["depth"], 16);
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (1080, 1920, 72.0));
            let pixel = h.state_mut().session.execute("document.pixel", serde_json::json!({"x":0,"y":0})).unwrap();
            assert_eq!(pixel[3], 0.0);
        }

        #[test]
        fn search_matches_the_preference_scaled_print_size_and_enter_applies_it_once() {
            let mut h = harness();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.printResolution","value":200.0})).unwrap();
            h.run();
            search(&mut h, "2550x3300");
            assert!(h.query_by_role_and_label(Role::Button, "Letter").is_none(), "the listed 300 ppi size is no longer offered");
            h.get_by_label("Clear").click();
            h.run();
            search(&mut h, "1700x2200");
            assert!(h.query_by_role_and_label(Role::Button, "Letter").is_some());
            // Card text and the created pixels must agree, even though the underlying preset
            // stays at its listed resolution to avoid applying the scale a second time.
            let _ = preset_title_at(&h, "1700 × 2200 px");
            enter(&mut h);
            assert!(h.state().session.documents().is_empty());
            assert_eq!(fields(&h)["__unit"], "in");
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (1700, 2200, 200.0));
        }

        #[test]
        fn social_search_and_card_selection_keep_pixels_at_the_screen_resolution() {
            for (query, name, height) in [("1080x1440", "Instagram post, 3:4", 1440), ("1080x1350", "Instagram post, 4:5", 1350)] {
                let mut h = harness();
                h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.screenResolution","value":600.0})).unwrap();
                h.run();
                search(&mut h, query);
                let at = h.get_by_role_and_label(Role::Button, name).rect().center();
                click_at(&mut h, at);
                assert_eq!(fields(&h)["__unit"], "px", "a screen resolution above 300 does not make a print preset");
                assert_eq!(fields(&h)["resolution"], 600.0);
                let _ = preset_title_at(&h, &format!("1080 × {height} px"));
                h.get_by_label("Create").click();
                h.run();
                assert_eq!(created(&h), (1080, height, 600.0));
            }
        }

        #[test]
        fn recent_search_restores_captured_settings_without_preference_rescaling() {
            let mut h = harness();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.printResolution","value":200.0})).unwrap();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.screenResolution","value":600.0})).unwrap();
            h.state_mut()
                .run(
                    "document.recent.add",
                    serde_json::json!({"settings":{
                        "width":800,"height":600,"resolution":120.0,"mode":"cmyk","depth":16,
                        "background":"black","unit":"mm","resolutionUnit":"cm"
                    }}),
                )
                .unwrap();
            h.run();
            h.get_by_label("Recent").click();
            h.run();
            search(&mut h, "800x600");
            enter(&mut h);
            assert!(h.state().session.documents().is_empty());
            let f = fields(&h);
            assert_eq!((f["mode"].as_str(), f["depth"].as_u64(), f["background"].as_str()), (Some("cmyk"), Some(16), Some("black")));
            assert_eq!((f["__unit"].as_str(), f["__resUnit"].as_str()), (Some("mm"), Some("cm")));
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (800, 600, 120.0));
            let recent = h.state().session.presets.recent_documents.first().unwrap();
            assert_eq!((recent.mode.as_str(), recent.depth, recent.unit.as_str()), ("cmyk", 16, "mm"));
        }

        #[test]
        fn typed_physical_units_create_the_expected_pixels_while_displaying_pixels() {
            let mut h = harness();
            let mut f = fields(&h);
            f.insert("resolution".into(), serde_json::json!(300.0));
            f.insert("__unit".into(), serde_json::json!("px"));
            set_fields(&mut h, f);
            type_into(&mut h, 0, "210mm");
            type_into(&mut h, 1, "297mm");
            assert_eq!(fields(&h)["__unit"], "px");
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (2480, 3508, 300.0));
        }

        #[test]
        fn empty_search_enter_keeps_the_form_and_clear_restores_the_category() {
            let mut h = harness();
            search(&mut h, "no-such-format");
            assert!(h.query_by_label("No matching formats.").is_some());
            enter(&mut h);
            assert!(h.state().session.documents().is_empty());
            assert_eq!(fields(&h)["width"], 1920);
            h.get_by_label("Clear").click();
            h.run();
            assert!(h.query_by_role_and_label(Role::Button, "Stories / Reels / Shorts").is_some());
        }

        fn assert_centered(h: &Harness<'static, PhotocraftApp>) {
            let id = h.state().ui.dialogs[0].id;
            let rect = h.ctx.memory(|m| m.area_rect(egui::Id::new(("dialog", id)))).expect("modal rect");
            assert!((rect.center() - h.ctx.content_rect().center()).length() <= 2.0, "modal {rect:?}, viewport {:?}", h.ctx.content_rect());
        }

        #[test]
        fn opens_and_reopens_centered_in_every_theme_even_with_a_remembered_position() {
            for theme in crate::theme::ThemeKind::ALL {
                let mut h = harness();
                PhotocraftApp::setup_context(&h.ctx, theme);
                h.ctx.data_mut(|m| m.insert_persisted(egui::Id::new(("dialog-place", "NewDocument")), egui::vec2(5.0, 7.0)));
                h.run();
                assert_centered(&h);
                h.get_by_label("Advanced").click();
                h.run();
                assert_centered(&h);
                assert!(h.ctx.content_rect().contains_rect(h.get_by_label("Create").rect()));
                h.get_by_label("Close").click();
                h.run();
                h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
                h.run();
                assert_centered(&h);
                h.set_size(egui::vec2(640.0, 740.0));
                h.run();
                assert_centered(&h);
                assert!(h.ctx.content_rect().contains_rect(h.get_by_label("Create").rect()));
            }
        }

        #[test]
        fn saving_from_a_search_clears_the_filter_and_saved_search_selects_by_name() {
            let mut h = harness();
            search(&mut h, "4:5");
            enter(&mut h);
            name_preset(&mut h, "Campaign vertical");
            h.get_by_label("Save").click();
            h.run();
            assert_eq!(fields(&h)["__search"], "");
            assert!(h.query_by_label("Campaign vertical").is_some());
            search(&mut h, "campaign");
            enter(&mut h);
            assert_eq!(fields(&h)["__savedPreset"], "Campaign vertical");
            assert!(h.state().session.documents().is_empty());
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (1080, 1350, 72.0));
        }

        #[test]
        fn narrow_viewport_keeps_search_and_create_inside_the_window() {
            let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            let mut h =
                Harness::builder().with_max_steps(64).with_size(egui::vec2(640.0, 740.0)).build_ui_state(|ui, app| crate::dialogs::show(app, ui.ctx()), app);
            PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
            h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            h.run();
            let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 740.0));
            assert!(viewport.contains_rect(h.get_by_label("Create").rect()), "create {:?}, viewport {:?}", h.get_by_label("Create").rect(), viewport);
            assert!(viewport.contains_rect(h.get_by_role_and_label(Role::TextInput, "Search formats").rect()));
            search(&mut h, "1080x1350");
            h.get_by_role_and_label(Role::Button, "Instagram post, 4:5").click();
            h.run();
            h.get_by_label("Create").click();
            h.run();
            assert_eq!(created(&h), (1080, 1350, 72.0));
        }

        /// Find the painted title inside a preset card.
        fn preset_title_at(h: &Harness<'static, PhotocraftApp>, title: &str) -> egui::Pos2 {
            fn find(shape: &egui::Shape, title: &str) -> Option<egui::Pos2> {
                match shape {
                    egui::Shape::Text(text) if text.galley.job.text == title => Some(text.visual_bounding_rect().center()),
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, title)),
                    _ => None,
                }
            }
            h.output().shapes.iter().find_map(|shape| find(&shape.shape, title)).unwrap_or_else(|| panic!("visible preset title: {title:?}"))
        }

        fn second_click_creates_once(h: &mut Harness<'static, PhotocraftApp>, at: egui::Pos2) {
            assert!(h.state().session.documents().is_empty(), "the first click only selects the preset");
            assert_eq!(h.state().ui.dialogs.len(), 1);
            // Two real pointer press/release pairs within egui's double-click interval.
            click_at(h, at);
            h.run_steps(3);
            assert_eq!(h.state().session.documents().len(), 1, "the second click creates exactly one document");
            assert!(h.state().ui.dialogs.is_empty(), "double-click closes the New Document dialog");
            assert!(h.state().ui.views[0].fit_pending, "the normal Create path fits the new document");
        }

        fn assert_pixel(h: &mut Harness<'static, PhotocraftApp>, expected: [f64; 4]) {
            let pixel = h.state_mut().session.execute("document.pixel", serde_json::json!({"x":0,"y":0})).unwrap();
            for (actual, expected) in pixel.as_array().unwrap().iter().zip(expected) {
                assert!((actual.as_f64().unwrap() - expected).abs() < 0.001, "background pixel: {pixel}");
            }
        }

        #[test]
        fn double_click_builtin_preset_creates_its_size_and_current_details() {
            let mut h = harness();
            let mut f = fields(&h);
            f.extend(serde_json::json!({"name":"Portrait proof","mode":"cmyk","depth":16,"background":"transparent"}).as_object().unwrap().clone());
            set_fields(&mut h, f);
            h.get_by_label("Photo").click();
            h.run_steps(2);
            let at = preset_title_at(&h, "Portrait, 4 x 6");
            click_at(&mut h, at);
            assert_eq!(fields(&h)["__preset"], "Portrait, 4 x 6");
            assert_eq!(
                super::super::command_params(&fields(&h)),
                serde_json::json!({"name":"Portrait proof","width":1200,"height":1800,"resolution":300.0,
                    "mode":"cmyk","depth":16,"background":"transparent"})
            );
            second_click_creates_once(&mut h, at);
            assert_eq!(created(&h), (1200, 1800, 300.0));
            let doc = &h.state().session.active().unwrap().doc;
            assert_eq!(doc.name, "Portrait proof");
            assert_eq!((doc.mode, doc.depth), (photocraft_doc::ColorMode::Cmyk, photocraft_doc::SampleType::U16));
            assert_pixel(&mut h, [0.0; 4]);
        }

        #[test]
        fn double_click_clipboard_preset_creates_its_size_and_current_details() {
            let mut h = harness();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.printResolution","value":200.0})).unwrap();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.screenResolution","value":96.0})).unwrap();
            let mut f = fields(&h);
            super::super::set_clipboard(&mut f, 37, 23);
            apply_preset(&mut f, &("Other size", 100, 200, 300.0), &Default::default());
            f.extend(serde_json::json!({"mode":"gray","depth":8,"background":"white"}).as_object().unwrap().clone());
            set_fields(&mut h, f);
            let at = preset_title_at(&h, super::super::CLIPBOARD);
            click_at(&mut h, at);
            assert_eq!(fields(&h)["__preset"], super::super::CLIPBOARD);
            assert_eq!(
                super::super::command_params(&fields(&h)),
                serde_json::json!({"name":"Untitled-1","width":37,"height":23,"resolution":72.0,
                    "mode":"gray","depth":8,"background":"white"})
            );
            second_click_creates_once(&mut h, at);
            assert_eq!(created(&h), (37, 23, 72.0));
            let doc = &h.state().session.active().unwrap().doc;
            assert_eq!((doc.mode, doc.depth), (photocraft_doc::ColorMode::Grayscale, photocraft_doc::SampleType::U8));
            assert_pixel(&mut h, [1.0; 4]);
        }

        #[test]
        fn double_click_saved_preset_restores_all_settings_and_captured_background() {
            let mut h = harness();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.printResolution","value":200.0})).unwrap();
            h.state_mut().run("prefs.set", serde_json::json!({"path":"unitsAndRulers.screenResolution","value":96.0})).unwrap();
            h.state_mut()
                .run(
                    "document.presets.save",
                    serde_json::json!({"name":"Saved proof","settings":{
                        "width":48,"height":32,"resolution":254.0,"mode":"rgb","depth":32,
                        "background":"backgroundColor","backgroundColor":[0.125,0.375,0.625],"unit":"mm","resolutionUnit":"cm"
                    }}),
                )
                .unwrap();
            h.state_mut().session.tools.background = [1.0, 0.0, 1.0, 1.0];
            let mut f = fields(&h);
            f.insert("name".into(), serde_json::json!("Current document name"));
            set_fields(&mut h, f);
            h.get_by_label("Saved").click();
            h.run_steps(2);
            let at = h.get_by_label("Saved proof").rect().center();
            click_at(&mut h, at);
            assert_eq!(fields(&h)["__savedPreset"], "Saved proof");
            assert_eq!(fields(&h)["__unit"], "mm");
            assert_eq!(fields(&h)["__resUnit"], "cm");
            second_click_creates_once(&mut h, at);
            assert_eq!(created(&h), (48, 32, 254.0));
            let doc = &h.state().session.active().unwrap().doc;
            assert_eq!(doc.name, "Current document name");
            assert_eq!((doc.mode, doc.depth), (photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::F32));
            assert_pixel(&mut h, [0.125, 0.375, 0.625, 1.0]);
        }

        #[test]
        fn preset_cards_use_the_preset_resolution_preferences() {
            let mut h = harness();
            h.state_mut().run("prefs.set", serde_json::json!({"path": "unitsAndRulers.printResolution", "value": 150.0})).unwrap();
            h.state_mut().run("prefs.set", serde_json::json!({"path": "unitsAndRulers.screenResolution", "value": 96.0})).unwrap();
            // Wait for the responsive modal to finish re-centering before inspecting its paint.
            h.get_by_label("Photo").click();
            h.run();
            let at = preset_title_at(&h, "Landscape, 6 x 4");
            click_at(&mut h, at);
            assert_eq!(fields(&h)["__unit"], "in");
            second_click_creates_once(&mut h, at);
            assert_eq!(created(&h), (900, 600, 150.0), "6 x 4 in at 150 ppi");

            h.state_mut().ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
            h.run();
            h.get_by_label("Web").click();
            h.run();
            let at = preset_title_at(&h, "Web Minimum");
            click_at(&mut h, at);
            enter(&mut h);
            assert_eq!(created(&h), (1024, 768, 96.0));
        }

        #[test]
        fn arithmetic_dimensions_create_the_evaluated_size() {
            let mut h = harness();
            type_into(&mut h, 0, "1920/2");
            type_into(&mut h, 1, "(100+50)*2");
            enter(&mut h);
            assert_eq!(created(&h), (960, 300, 72.0));
        }

        #[test]
        fn typed_size_then_enter_creates_that_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "512");
            let f = fields(&h);
            assert_eq!((f["width"].as_u64(), f["height"].as_u64()), (Some(512), Some(512)), "whole pixels: {f:?}");
            enter(&mut h);
            assert_eq!(created(&h), (512, 512, 72.0));
        }

        #[test]
        fn typed_size_then_create_without_leaving_the_field_creates_that_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "300");
            // Still editing Height: click Create straight away.
            let create = h.get_by_label("Create").rect();
            click_at(&mut h, create.center());
            assert_eq!(created(&h), (512, 300, 72.0));
        }

        #[test]
        fn typing_over_a_preset_wins() {
            let mut h = harness();
            let mut f = fields(&h);
            let web = CATEGORIES.iter().find(|c| c.0 == "Web").unwrap().1.iter().find(|p| p.0 == "Web Minimum").unwrap();
            apply_preset(&mut f, web, &Default::default());
            set_fields(&mut h, f);
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "512");
            assert!(fields(&h).get("__preset").is_none(), "typing deselects the preset");
            enter(&mut h);
            assert_eq!(created(&h), (512, 512, 72.0));
        }

        #[test]
        fn the_clipboard_card_is_first_and_creates_the_clipboard_size() {
            let mut h = harness();
            let mut f = fields(&h);
            super::super::set_clipboard(&mut f, 640, 360);
            set_fields(&mut h, f);
            assert!(h.query_by_label_contains("BLANK DOCUMENT PRESETS (7)").is_some());
            let clipboard = h.get_by_role_and_label(Role::Button, super::super::CLIPBOARD).rect();
            let social = h.get_by_role_and_label(Role::Button, "Stories / Reels / Shorts").rect();
            assert!(clipboard.left() < social.left(), "clipboard is the first card");
            click_at(&mut h, social.center());
            assert_eq!(fields(&h).get("__preset").and_then(|v| v.as_str()), Some("Stories / Reels / Shorts"));
            click_at(&mut h, clipboard.center());
            assert_eq!(fields(&h).get("__preset").and_then(|v| v.as_str()), Some(super::super::CLIPBOARD));
            enter(&mut h);
            assert_eq!(created(&h), (640, 360, 72.0));
        }

        #[test]
        fn clicking_a_preset_card_after_typing_sets_its_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            h.get_by_label("Photo").click();
            h.run();
            h.get_by_role_and_label(Role::Button, "Landscape, 6 x 4").click();
            h.run();
            assert_eq!(fields(&h).get("__preset").and_then(|v| v.as_str()), Some("Landscape, 6 x 4"));
            enter(&mut h);
            assert_eq!(created(&h), (1800, 1200, 300.0));
        }

        #[test]
        fn typed_size_in_inches_converts_at_the_resolution() {
            let mut h = harness();
            type_into(&mut h, 2, "300");
            let mut f = fields(&h);
            f.insert("__unit".into(), serde_json::json!("in"));
            set_fields(&mut h, f);
            type_into(&mut h, 0, "2");
            type_into(&mut h, 1, "1.5");
            enter(&mut h);
            assert_eq!(created(&h), (600, 450, 300.0));
        }

        /// #1147: typed digit by digit, millimetres used to be rewritten after each keystroke
        /// (`8` became `8.1`), so 841 never arrived.
        #[test]
        fn typed_size_in_millimetres_keeps_every_digit() {
            let mut h = harness();
            let mut f = fields(&h);
            f.insert("__unit".into(), serde_json::json!("mm"));
            set_fields(&mut h, f);
            type_into(&mut h, 0, "841");
            type_into(&mut h, 1, "1189");
            enter(&mut h);
            assert_eq!(created(&h), (2384, 3370, 72.0));
        }

        #[test]
        fn changing_units_keeps_the_typed_pixel_size() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "512");
            for unit in ["in", "cm", "mm", "pt", "pica", "px"] {
                let mut f = fields(&h);
                f.insert("__unit".into(), serde_json::json!(unit));
                set_fields(&mut h, f);
            }
            enter(&mut h);
            assert_eq!(created(&h), (512, 512, 72.0));
        }

        #[test]
        fn orientation_swap_keeps_whole_pixels() {
            let mut h = harness();
            type_into(&mut h, 0, "512");
            type_into(&mut h, 1, "256");
            // The Portrait icon button follows the "Orientation" label (icons have tooltips only).
            let label = h.get_by_label("Orientation").rect();
            let gap = h.ctx.global_style().spacing.item_spacing.x;
            click_at(&mut h, egui::pos2(label.right() + gap + 12.0, label.center().y));
            enter(&mut h);
            assert_eq!(created(&h), (256, 512, 72.0));
        }

        /// #4668: once an orientation is picked, the presets turn to it, on the cards and when applied.
        #[test]
        fn picked_orientation_turns_the_presets() {
            let mut h = harness();
            h.get_by_label("Print").click();
            h.run();
            // Use the named button; the responsive layout can change icon positions.
            h.get_by_role_and_label(Role::Button, "Landscape").click();
            h.run();
            assert_eq!(fields(&h)["__orientation"], "landscape");
            assert_eq!((fields(&h)["width"].as_u64(), fields(&h)["height"].as_u64()), (Some(1920), Some(1080)));
            let _ = preset_title_at(&h, "11.7 x 8.3 in @ 300 ppi");
            let at = preset_title_at(&h, "A4");
            click_at(&mut h, at);
            enter(&mut h);
            assert_eq!(created(&h), (3508, 2480, 300.0));
        }

        /// #4668: a physical unit picked for Width/Height is the one print presets show and keep.
        #[test]
        fn picked_physical_unit_is_the_print_presets_unit() {
            let mut h = harness();
            let mut f = fields(&h);
            f.insert("__unit".into(), serde_json::json!("mm"));
            set_fields(&mut h, f);
            h.get_by_label("Print").click();
            h.run_steps(2);
            let _ = preset_title_at(&h, "210 x 297 mm @ 300 ppi");
            let at = preset_title_at(&h, "A4");
            click_at(&mut h, at);
            assert_eq!(fields(&h)["__unit"], "mm");
            // Screen presets stay in pixels.
            h.get_by_label("Web").click();
            h.run_steps(2);
            let _ = preset_title_at(&h, "1024 × 768 px");
            enter(&mut h);
            assert_eq!(created(&h), (2480, 3508, 300.0));
        }

        #[test]
        fn hovering_preset_tabs_shows_pointing_hand_not_text_cursor() {
            let mut h = harness();
            for tab in ["Recent", "Saved", "Photo", "Print", "Art & Illustration", "Web", "Mobile", "Film & Video"] {
                let rect = h.get_by_label(tab).rect();
                h.hover_at(rect.center());
                h.run_steps(2);
                assert_eq!(
                    h.output().platform_output.cursor_icon,
                    egui::CursorIcon::PointingHand,
                    "hovering preset tab {tab:?} must show PointingHand instead of text caret"
                );
            }
        }
    }
}
