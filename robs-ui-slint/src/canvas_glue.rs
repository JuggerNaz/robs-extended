//! Phase 3 glue: the canvas editing layer behind the `CanvasApi` global —
//! scene-item drag/resize, annotation draw/select/text tools, on-canvas
//! text-overlay dragging, and the toolbar callbacks. Ports the egui
//! interaction rules from `robs-ui/src/app/annotations.rs` and
//! `panels/preview.rs`.
//!
//! Coordinates: the markup forwards canvas-relative pointer pixels; the
//! scene-space math divides by the canvas scale stored in
//! [`push::PushedState`] each tick (the same fit math the pushes use).
//! The per-tick snapshot of annotations/overlays/selection/editor state is
//! pushed by `push.rs::push_canvas`; this module installs the callbacks and
//! owns the pointer state machine plus the shared geometry helpers.

use std::cell::RefCell;
use std::rc::Rc;

use robs_controller::state::EventLogKind;
use robs_controller::RobsController;
use robs_core::{
    Annotation, AnnotationId, AnnotationShape, AnnotationTool, ObjectId, Position, Scale,
    SceneItemId,
};
use slint::{Color, ComponentHandle, ModelRc, VecModel};

use crate::{CanvasApi, MainWindow, push};

/// Text-annotation font size in scene units (mirror of
/// `robs-ui/src/annotation_raster.rs`).
pub(crate) const TEXT_FONT_SIZE: f32 = 32.0;

/// What a press started — the pointer state machine.
#[derive(Clone, Copy, Default, PartialEq)]
enum DragMode {
    #[default]
    Idle,
    /// Drag-drawing the in-progress shape / pen stroke.
    Draw,
    /// Translating a scene item.
    MoveItem(SceneItemId),
    /// Corner-resizing a scene item (egui's corner-agnostic scale drag).
    ResizeItem(SceneItemId),
    /// Translating a committed annotation.
    MoveAnn(AnnotationId),
    /// Translating a text overlay label.
    MoveOverlay(ObjectId),
}

/// UI-side pointer state for the canvas.
pub struct CanvasUi {
    mode: DragMode,
    /// Canvas-px reference point: the press point for Move/Resize modes,
    /// the previous event position for MoveAnn/MoveOverlay (incremental
    /// translate), unused for Draw.
    last_px: (f32, f32),
    /// Scene-space item position at press (MoveItem).
    item_pos0: Position,
    /// Item scale at press (ResizeItem).
    item_scale0: (f32, f32),
    /// Item rendered size in canvas px at press (ResizeItem).
    item_dims0: (f32, f32),
}

impl CanvasUi {
    pub fn new() -> Self {
        Self {
            mode: DragMode::Idle,
            last_px: (0.0, 0.0),
            item_pos0: Position::zero(),
            item_scale0: (1.0, 1.0),
            item_dims0: (0.0, 0.0),
        }
    }
}

// ---------------------------------------------------------------------------
// Install: commands
// ---------------------------------------------------------------------------

/// Wire every `CanvasApi` callback. The models themselves are pushed each
/// tick by `push.rs`.
pub fn install(
    component: &slint::Weak<MainWindow>,
    controller: &Rc<RefCell<RobsController>>,
    state: &Rc<RefCell<CanvasUi>>,
    pushed: &Rc<RefCell<push::PushedState>>,
) {
    // ---- Initial paint: empty models + seeded slider ----
    if let Some(root) = component.upgrade() {
        let api = root.global::<CanvasApi>();
        api.set_annotations(ModelRc::new(VecModel::default()));
        api.set_text_overlays(ModelRc::new(VecModel::default()));
        api.set_selected_item_index(-1);
        api.set_stroke_width(controller.borrow().annotation.annotation_style.stroke_width);
    }

    // Handlers re-acquire globals from the window inside the callback (a
    // `'static` requirement — global references can't be captured).
    let Some(root) = component.upgrade() else {
        return;
    };
    let api = root.global::<CanvasApi>();

    // ---- Pointer state machine ----
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        let pushed = Rc::clone(pushed);
        api.on_canvas_press(move |px: f32, py: f32| {
            let Some(root) = weak.upgrade() else { return };
            let scale = pushed.borrow().canvas.scale.max(0.0001);
            let mut c = controller.borrow_mut();

            // Clicking anywhere while the inline editor is open commits it
            // (empty text deletes — egui lost-focus parity).
            if c.annotation.editing_text_id.is_some() {
                let text = root.global::<CanvasApi>().get_text_input().to_string();
                commit_edit(&mut c, text);
            }

            let scene = pos_to_scene(px, py, scale);
            match c.annotation.annotation_tool {
                // ---- Pen: drag to accumulate freehand points ----
                AnnotationTool::Pen => {
                    let mut ann = Annotation::new(AnnotationShape::Pen, scene, scene);
                    ann.set_style(c.annotation.annotation_style);
                    ann.push_point(scene);
                    c.annotation.annotation_drawing = Some(ann);
                    let ui = &mut *state.borrow_mut();
                    ui.mode = DragMode::Draw;
                    ui.last_px = (px, py);
                }
                // ---- Text: click to place, then type inline ----
                AnnotationTool::Text => {
                    let mut ann = Annotation::new(AnnotationShape::Text, scene, scene);
                    ann.set_style(c.annotation.annotation_style);
                    c.annotation.annotations.push(ann);
                    let new_id = c.annotation.annotations.last().map(|a| a.id());
                    c.annotation.editing_text_id = new_id;
                    c.annotation.text_input.clear();
                    c.log_event("Annotation added: Text", EventLogKind::Annotation);
                }
                // ---- Select: overlay/annotation hit-test, handles, items ----
                AnnotationTool::Select => {
                    // 1) Text overlays, topmost first (registered last by the
                    // egui renderer, so they won over annotations).
                    if let Some(id) = hit_overlay(&c, px, py, scale) {
                        let ui = &mut *state.borrow_mut();
                        ui.mode = DragMode::MoveOverlay(id);
                        ui.last_px = (px, py);
                        return;
                    }
                    // 2) Annotations, topmost first (6px slack, egui parity).
                    if let Some(id) = hit_annotation(&c, px, py, scale) {
                        c.annotation.selected_annotation = Some(id);
                        let ui = &mut *state.borrow_mut();
                        ui.mode = DragMode::MoveAnn(id);
                        ui.last_px = (px, py);
                        return;
                    }
                    // 3) Corner handles of the selected scene item.
                    let selected = root.global::<CanvasApi>().get_selected_item_index();
                    if let Some((id, scale0, dims0)) = handle_hit(&c, selected, px, py, scale) {
                        let ui = &mut *state.borrow_mut();
                        ui.mode = DragMode::ResizeItem(id);
                        ui.item_scale0 = scale0;
                        ui.item_dims0 = dims0;
                        ui.last_px = (px, py);
                        return;
                    }
                    // 4) Item bodies, topmost first (click = select).
                    if let Some((id, index, pos)) = item_hit(&c, px, py, scale) {
                        root.global::<CanvasApi>()
                            .set_selected_item_index(index as i32);
                        let ui = &mut *state.borrow_mut();
                        ui.mode = DragMode::MoveItem(id);
                        ui.item_pos0 = pos;
                        ui.last_px = (px, py);
                        return;
                    }
                    // 5) Empty canvas: deselect everything.
                    c.annotation.selected_annotation = None;
                    root.global::<CanvasApi>().set_selected_item_index(-1);
                }
                // ---- Regular shapes (Arrow/Line/Rectangle/Ellipse) ----
                tool => {
                    let shape = tool.shape().unwrap_or(AnnotationShape::Line);
                    let mut ann = Annotation::new(shape, scene, scene);
                    ann.set_style(c.annotation.annotation_style);
                    c.annotation.annotation_drawing = Some(ann);
                    let ui = &mut *state.borrow_mut();
                    ui.mode = DragMode::Draw;
                    ui.last_px = (px, py);
                }
            }
        });
    }
    {
        let weak = component.clone();
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        let pushed = Rc::clone(pushed);
        api.on_canvas_move(move |px: f32, py: f32| {
            let Some(_root) = weak.upgrade() else { return };
            let scale = pushed.borrow().canvas.scale.max(0.0001);
            let mut c = controller.borrow_mut();
            let mut ui = state.borrow_mut();
            match ui.mode {
                DragMode::Idle => {}
                DragMode::Draw => {
                    let scene = pos_to_scene(px, py, scale);
                    if let Some(ann) = c.annotation.annotation_drawing.as_mut() {
                        if ann.shape() == AnnotationShape::Pen {
                            ann.push_point(scene);
                        } else {
                            ann.set_end(scene);
                        }
                    }
                }
                DragMode::MoveItem(id) => {
                    let dx = (px - ui.last_px.0) / scale;
                    let dy = (py - ui.last_px.1) / scale;
                    let p = ui.item_pos0;
                    if let Some(item) =
                        c.scenes.current_scene_mut().and_then(|s| s.item_mut(id))
                    {
                        item.set_position(Position::new(p.x + dx, p.y + dy));
                    }
                }
                DragMode::ResizeItem(id) => {
                    let dx = px - ui.last_px.0;
                    let dy = py - ui.last_px.1;
                    let (sx0, sy0) = ui.item_scale0;
                    let (w0, h0) = ui.item_dims0;
                    // Corner-agnostic: drag distance always grows the item
                    // (the egui quirk, preserved verbatim).
                    let sx = (sx0 + dx * sx0 / w0.max(1.0)).max(0.01);
                    let sy = (sy0 + dy * sy0 / h0.max(1.0)).max(0.01);
                    if let Some(item) =
                        c.scenes.current_scene_mut().and_then(|s| s.item_mut(id))
                    {
                        item.set_scale(Scale::new(sx, sy));
                    }
                }
                DragMode::MoveAnn(id) => {
                    let dx = (px - ui.last_px.0) / scale;
                    let dy = (py - ui.last_px.1) / scale;
                    ui.last_px = (px, py);
                    if let Some(ann) =
                        c.annotation.annotations.iter_mut().find(|a| a.id() == id)
                    {
                        ann.translate(dx, dy);
                    }
                }
                DragMode::MoveOverlay(id) => {
                    let dx = (px - ui.last_px.0) / scale;
                    let dy = (py - ui.last_px.1) / scale;
                    ui.last_px = (px, py);
                    if let Some(ov) = c.text_overlays.iter_mut().find(|o| o.id() == id) {
                        let p = ov.position();
                        ov.set_position(Position::new(p.x + dx, p.y + dy));
                    }
                }
            }
        });
    }
    {
        let controller = Rc::clone(controller);
        let state = Rc::clone(state);
        api.on_canvas_release(move |_px: f32, _py: f32| {
            let mut c = controller.borrow_mut();
            let mut ui = state.borrow_mut();
            if ui.mode == DragMode::Draw {
                if let Some(ann) = c.annotation.annotation_drawing.take() {
                    // egui commit rules: pen strokes need >1 point, shapes
                    // need a >2-scene-unit extent (clicks are discarded).
                    let keep = match ann.shape() {
                        AnnotationShape::Pen => ann.points().len() > 1,
                        _ => {
                            let (w, h) = ann.size();
                            w > 2.0 || h > 2.0
                        }
                    };
                    if keep {
                        c.log_event(
                            format!("Annotation added: {:?}", ann.shape()),
                            EventLogKind::Annotation,
                        );
                        c.annotation.annotations.push(ann);
                    }
                }
            }
            ui.mode = DragMode::Idle;
        });
    }

    // ---- Inline text editor ----
    {
        let controller = Rc::clone(controller);
        api.on_commit_text_edit(move |text: slint::SharedString| {
            let mut c = controller.borrow_mut();
            commit_edit(&mut c, text.to_string());
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_cancel_text_edit(move || {
            let mut c = controller.borrow_mut();
            // Cancel always deletes the annotation being edited (egui
            // Escape parity).
            if let Some(id) = c.annotation.editing_text_id.take() {
                c.annotation.annotations.retain(|a| a.id() != id);
            }
        });
    }

    // ---- Selection / keyboard ----
    {
        let controller = Rc::clone(controller);
        api.on_delete_selected(move || {
            let mut c = controller.borrow_mut();
            if let Some(id) = c.annotation.selected_annotation.take() {
                c.annotation.annotations.retain(|a| a.id() != id);
            }
        });
    }

    // ---- Toolbar ----
    {
        let controller = Rc::clone(controller);
        api.on_set_tool(move |index: i32| {
            let tool = tool_from_index(index);
            let mut c = controller.borrow_mut();
            c.annotation.annotation_tool = tool;
            if tool == AnnotationTool::Select {
                c.annotation.selected_annotation = None;
            }
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_set_stroke_color(move |color: Color| {
            let mut c = controller.borrow_mut();
            c.annotation.annotation_style.color =
                [color.red(), color.green(), color.blue(), color.alpha()];
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_set_stroke_width(move |width: f32| {
            let mut c = controller.borrow_mut();
            c.annotation.annotation_style.stroke_width = width.clamp(1.0, 24.0);
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_set_filled(move |filled: bool| {
            controller.borrow_mut().annotation.annotation_style.filled = filled;
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_undo_annotation(move || {
            let mut c = controller.borrow_mut();
            c.annotation.annotations.pop();
            // Drop the selection if it pointed at the removed annotation.
            if let Some(sel) = c.annotation.selected_annotation {
                if !c.annotation.annotations.iter().any(|a| a.id() == sel) {
                    c.annotation.selected_annotation = None;
                }
            }
        });
    }
    {
        let controller = Rc::clone(controller);
        api.on_clear_annotations(move || {
            let mut c = controller.borrow_mut();
            c.annotation.annotations.clear();
            c.annotation.selected_annotation = None;
        });
    }
}

// ---------------------------------------------------------------------------
// Shared geometry helpers (also used by `push.rs`)
// ---------------------------------------------------------------------------

fn pos_to_scene(px: f32, py: f32, scale: f32) -> Position {
    Position::new(px / scale, py / scale)
}

pub(crate) fn tool_from_index(index: i32) -> AnnotationTool {
    match index {
        1 => AnnotationTool::Arrow,
        2 => AnnotationTool::Line,
        3 => AnnotationTool::Rectangle,
        4 => AnnotationTool::Ellipse,
        5 => AnnotationTool::Pen,
        6 => AnnotationTool::Text,
        _ => AnnotationTool::Select,
    }
}

pub(crate) fn tool_index(tool: AnnotationTool) -> i32 {
    match tool {
        AnnotationTool::Select => 0,
        AnnotationTool::Arrow => 1,
        AnnotationTool::Line => 2,
        AnnotationTool::Rectangle => 3,
        AnnotationTool::Ellipse => 4,
        AnnotationTool::Pen => 5,
        AnnotationTool::Text => 6,
    }
}

/// Canvas-px bounding box of an annotation (egui `screen_bbox` port).
/// `None` for an empty pen stroke.
pub(crate) fn ann_bbox(ann: &Annotation, scale: f32) -> Option<(f32, f32, f32, f32)> {
    match ann.shape() {
        AnnotationShape::Pen => {
            if ann.points().is_empty() {
                return None;
            }
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for p in ann.points() {
                min_x = min_x.min(p.x);
                min_y = min_y.min(p.y);
                max_x = max_x.max(p.x);
                max_y = max_y.max(p.y);
            }
            Some((
                min_x * scale,
                min_y * scale,
                (max_x - min_x) * scale,
                (max_y - min_y) * scale,
            ))
        }
        AnnotationShape::Text => {
            let font_px = (TEXT_FONT_SIZE * scale).max(8.0);
            // egui's estimate: 0.6 em per character, at least 20px wide.
            let w = (ann.text().len().max(1) as f32 * font_px * 0.6).max(20.0);
            let s = ann.start();
            Some((s.x * scale, s.y * scale, w, font_px * 1.3))
        }
        _ => {
            let (s, e) = (ann.start(), ann.end());
            Some((
                s.x.min(e.x) * scale,
                s.y.min(e.y) * scale,
                (e.x - s.x).abs() * scale,
                (e.y - s.y).abs() * scale,
            ))
        }
    }
}

/// SVG-like path commands for an annotation, in SCENE coordinates (the
/// markup's Path viewbox does the letterbox mapping). Slint's Path has no
/// arc command, so ellipses are 48-gon polylines — visually identical.
/// Arrow heads are sized in screen pixels by egui, so the head length is
/// converted back to scene units here.
pub(crate) fn path_commands(ann: &Annotation, scale: f32) -> String {
    let fmt = |p: Position| format!("{:.1} {:.1}", p.x, p.y);
    match ann.shape() {
        AnnotationShape::Text => String::new(),
        AnnotationShape::Pen => {
            let mut pts = ann.points().iter();
            let Some(first) = pts.next() else {
                return String::new();
            };
            let mut out = format!("M {}", fmt(*first));
            for p in pts {
                out.push_str(&format!(" L {}", fmt(*p)));
            }
            out
        }
        AnnotationShape::Line => format!("M {} L {}", fmt(ann.start()), fmt(ann.end())),
        AnnotationShape::Rectangle => {
            let (s, e) = (ann.start(), ann.end());
            let (x0, y0) = (s.x.min(e.x), s.y.min(e.y));
            let (x1, y1) = (s.x.max(e.x), s.y.max(e.y));
            format!("M {x0:.1} {y0:.1} L {x1:.1} {y0:.1} L {x1:.1} {y1:.1} L {x0:.1} {y1:.1} Z")
        }
        AnnotationShape::Ellipse => {
            let (s, e) = (ann.start(), ann.end());
            let (cx, cy) = ((s.x + e.x) / 2.0, (s.y + e.y) / 2.0);
            let (rx, ry) = ((e.x - s.x).abs() / 2.0, (e.y - s.y).abs() / 2.0);
            let n = 48usize;
            let mut out = String::new();
            for i in 0..n {
                let t = i as f32 / n as f32 * std::f32::consts::TAU;
                let px = cx + rx * t.cos();
                let py = cy + ry * t.sin();
                if i == 0 {
                    out.push_str(&format!("M {px:.1} {py:.1}"));
                } else {
                    out.push_str(&format!(" L {px:.1} {py:.1}"));
                }
            }
            out.push_str(" Z");
            out
        }
        AnnotationShape::Arrow => {
            let (s, e) = (ann.start(), ann.end());
            let mut out = format!("M {} L {}", fmt(s), fmt(e));
            let (dx, dy) = (e.x - s.x, e.y - s.y);
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > 1e-3 {
                let (dir_x, dir_y) = (dx / dist, dy / dist);
                let head_len =
                    (ann.style().stroke_width * 3.0).max(12.0) / scale.max(0.0001);
                let spread = head_len * 0.5_f32.tan();
                let (perp_x, perp_y) = (-dir_y, dir_x);
                let (back_x, back_y) = (e.x - dir_x * head_len, e.y - dir_y * head_len);
                let left = Position::new(back_x + perp_x * spread, back_y + perp_y * spread);
                let right = Position::new(back_x - perp_x * spread, back_y - perp_y * spread);
                out.push_str(&format!(
                    " M {} L {} M {} L {}",
                    fmt(e),
                    fmt(left),
                    fmt(e),
                    fmt(right)
                ));
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Hit tests (Select tool)
// ---------------------------------------------------------------------------

/// Topmost visible annotation under the canvas point (6px slack, egui
/// reverse-order parity).
fn hit_annotation(c: &RobsController, px: f32, py: f32, scale: f32) -> Option<AnnotationId> {
    for ann in c.annotation.annotations.iter().rev() {
        if !ann.is_visible() {
            continue;
        }
        let Some((x, y, w, h)) = ann_bbox(ann, scale) else {
            continue;
        };
        if px >= x - 6.0 && px <= x + w + 6.0 && py >= y - 6.0 && py <= y + h + 6.0 {
            return Some(ann.id());
        }
    }
    None
}

/// Topmost visible text overlay under the canvas point. The rendered label
/// is the text plus 8x4 padding at (pos - (4, 2)) — mirror of the markup's
/// `OverlayLabel` — with egui's 6px hit slack.
fn hit_overlay(c: &RobsController, px: f32, py: f32, scale: f32) -> Option<ObjectId> {
    for overlay in c.text_overlays.iter().rev().filter(|o| o.is_visible()) {
        let pos = overlay.position();
        let font_px = (overlay.font_size() * scale).max(8.0);
        let w = (overlay.text().len().max(1) as f32 * font_px * 0.6).max(20.0) + 8.0;
        let h = font_px * 1.3 + 4.0;
        let (x, y) = (pos.x * scale - 4.0, pos.y * scale - 2.0);
        if px >= x - 6.0 && px <= x + w + 6.0 && py >= y - 6.0 && py <= y + h + 6.0 {
            return Some(overlay.id());
        }
    }
    None
}

/// One scene item as seen by the canvas: id, rail index, scene position,
/// and rendered size in canvas px (the same dims the pushes use).
struct ItemGeom {
    id: SceneItemId,
    index: usize,
    pos: Position,
    size: (f32, f32),
}

fn item_geoms(c: &RobsController, scale: f32) -> Vec<ItemGeom> {
    c.scenes
        .current_scene()
        .map(|scene| {
            let (scene_w, scene_h) = scene.output_size();
            scene
                .items()
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let (src_w, src_h) = item
                        .capture()
                        .and_then(|cap| cap.native_size())
                        .unwrap_or((scene_w, scene_h));
                    ItemGeom {
                        id: item.id(),
                        index,
                        pos: item.position(),
                        size: (
                            src_w as f32 * item.scale().x * scale,
                            src_h as f32 * item.scale().y * scale,
                        ),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Corner-handle hit test on the selected item (handles sit at the padded
/// corners drawn by the markup). Returns the item id, its scale, and its
/// rendered canvas size at press.
fn handle_hit(
    c: &RobsController,
    selected: i32,
    px: f32,
    py: f32,
    scale: f32,
) -> Option<(SceneItemId, (f32, f32), (f32, f32))> {
    if selected < 0 {
        return None;
    }
    let items = item_geoms(c, scale);
    let item = items.get(selected as usize)?;
    let (w, h) = item.size;
    let (x, y) = (item.pos.x * scale - 5.0, item.pos.y * scale - 5.0);
    let corners = [
        (x, y),
        (x + w + 10.0, y),
        (x, y + h + 10.0),
        (x + w + 10.0, y + h + 10.0),
    ];
    if corners
        .iter()
        .any(|(cx, cy)| (px - cx).abs() <= 6.0 && (py - cy).abs() <= 6.0)
    {
        let item_scale = c
            .scenes
            .current_scene()
            .and_then(|s| s.item(item.id))
            .map(|i| i.scale())?;
        Some((item.id, (item_scale.x, item_scale.y), (w, h)))
    } else {
        None
    }
}

/// Topmost item whose rendered rect contains the canvas point.
fn item_hit(
    c: &RobsController,
    px: f32,
    py: f32,
    scale: f32,
) -> Option<(SceneItemId, usize, Position)> {
    item_geoms(c, scale)
        .iter()
        .rev()
        .find(|item| {
            let (x, y) = (item.pos.x * scale, item.pos.y * scale);
            px >= x && px <= x + item.size.0 && py >= y && py <= y + item.size.1
        })
        .map(|item| (item.id, item.index, item.pos))
}

// ---------------------------------------------------------------------------
// Text editor commit
// ---------------------------------------------------------------------------

/// Commit the inline editor: non-empty text saves, empty text deletes
/// (egui lost-focus parity; Escape has no Slint LineEdit equivalent).
fn commit_edit(c: &mut RobsController, text: String) {
    let Some(id) = c.annotation.editing_text_id.take() else {
        return;
    };
    if !c.annotation.annotations.iter().any(|a| a.id() == id) {
        return;
    }
    if text.is_empty() {
        c.annotation.annotations.retain(|a| a.id() != id);
    } else if let Some(ann) = c.annotation.annotations.iter_mut().find(|a| a.id() == id) {
        ann.set_text(text);
    }
}
