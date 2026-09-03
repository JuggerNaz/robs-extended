//! Annotation / mark-up tools and on-canvas rendering. Extracted verbatim
//! from `app.rs`.

use robs_controller::state::EventLogKind;
use super::RobsApp;
use eframe::egui;
use robs_core::scene::Position;

impl RobsApp {
    /// Toolbar for the annotation / mark-up tools: tool selection, stroke
    /// color, stroke width, and undo/clear. Shown as a thin panel directly
    /// below the menu bar while `show_annotations` is enabled.
    pub(crate) fn annotation_toolbar(&mut self, ctx: &egui::Context) {
        if !self.annotation.show_annotations {
            return;
        }

        egui::TopBottomPanel::top("annotation_toolbar")
            .exact_height(36.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // Tool buttons: Select first, then the drawable shapes.
                    let mut tools: Vec<robs_core::AnnotationTool> = vec![robs_core::AnnotationTool::Select];
                    tools.extend(
                        robs_core::AnnotationShape::ALL
                            .iter()
                            .map(|s| tool_from_shape(*s)),
                    );

                    for tool in tools {
                        let selected = self.annotation.annotation_tool == tool;
                        let label = match tool {
                            robs_core::AnnotationTool::Select => "🖱 Select".to_owned(),
                            _ => {
                                let s = tool.shape().unwrap();
                                format!("{} {}", s.icon(), s.label())
                            }
                        };
                        if ui.selectable_label(selected, label).clicked() {
                            self.annotation.annotation_tool = tool;
                            if tool == robs_core::AnnotationTool::Select {
                                self.annotation.selected_annotation = None;
                            }
                        }
                    }

                    ui.separator();

                    // Stroke color
                    let mut color = [
                        self.annotation.annotation_style.color[0],
                        self.annotation.annotation_style.color[1],
                        self.annotation.annotation_style.color[2],
                    ];
                    ui.label("Color");
                    if ui.color_edit_button_srgb(&mut color).changed() {
                        self.annotation.annotation_style.color = [color[0], color[1], color[2], 255];
                    }

                    ui.separator();

                    // Stroke width
                    ui.label("Width");
                    ui.add(
                        egui::Slider::new(&mut self.annotation.annotation_style.stroke_width, 1.0..=24.0)
                            .clamping(egui::SliderClamping::Always),
                    );

                    // Fill toggle for closed shapes
                    let closed = self
                        .annotation.annotation_tool
                        .shape()
                        .map(|s| s.is_closed())
                        .unwrap_or(false);
                    if closed {
                        ui.checkbox(&mut self.annotation.annotation_style.filled, "Fill");
                    }

                    ui.separator();

                    // Undo / Clear
                    if ui.button("\u{238C} Undo").clicked() {
                        self.annotation.annotations.pop();
                    }
                    if ui.button("\u{1F5D1} Clear").clicked() {
                        self.annotation.annotations.clear();
                        self.annotation.selected_annotation = None;
                    }

                    ui.separator();

                    ui.label(
                        egui::RichText::new(if self.annotation.annotation_tool == robs_core::AnnotationTool::Select {
                            "Click an annotation to select, drag to move, Del to remove"
                        } else {
                            "Click and drag on the preview to draw"
                        })
                        .color(egui::Color32::from_rgb(150, 150, 150))
                        .small(),
                    );
                });
                ui.add_space(4.0);
            });
    }

    /// Render committed and in-progress annotations over the canvas and
    /// handle tool interaction (draw / select / move / delete).
    ///
    /// `canvas_rect` is the on-screen rectangle representing the whole scene
    /// and `canvas_scale` converts scene coordinates -> screen pixels (see
    /// [`Self::preview_panel`] for the canonical letterbox math).
    pub(crate) fn render_annotations(
        &mut self,
        ui: &mut egui::Ui,
        canvas_rect: egui::Rect,
        canvas_scale: f32,
    ) {
        if !self.annotation.show_annotations {
            return;
        }

        let to_screen = |p: Position| {
            egui::pos2(
                canvas_rect.min.x + p.x * canvas_scale,
                canvas_rect.min.y + p.y * canvas_scale,
            )
        };
        let to_scene = |sp: egui::Pos2| {
            Position::new(
                (sp.x - canvas_rect.min.x) / canvas_scale,
                (sp.y - canvas_rect.min.y) / canvas_scale,
            )
        };
        let painter = ui.painter();
        let canvas_min = canvas_rect.min;

        // Compute the screen-space bounding rect of an annotation (for
        // selection highlighting + hit-testing).
        let text_font_screen =
            (robs_controller::annotation_raster::TEXT_FONT_SIZE * canvas_scale).max(8.0);
        let screen_bbox = |ann: &robs_core::Annotation| -> egui::Rect {
            match ann.shape() {
                robs_core::AnnotationShape::Pen => {
                    if ann.points().is_empty() {
                        return egui::Rect::NOTHING;
                    }
                    let pts: Vec<egui::Pos2> =
                        ann.points().iter().map(|&p| to_screen(p)).collect();
                    let mut r = egui::Rect::from_two_pos(pts[0], pts[0]);
                    for p in &pts[1..] {
                        r.extend_with(*p);
                    }
                    r
                }
                robs_core::AnnotationShape::Text => {
                    let anchor = to_screen(ann.start());
                    let w = (ann.text().len().max(1) as f32) * text_font_screen * 0.6;
                    egui::Rect::from_min_size(
                        anchor,
                        egui::vec2(w.max(20.0), text_font_screen * 1.3),
                    )
                }
                _ => egui::Rect::from_two_pos(to_screen(ann.start()), to_screen(ann.end())),
            }
        };

        // 1. Collect (id, index, screen-bbox) for visible annotations.
        let draw_info: Vec<(robs_core::AnnotationId, usize, egui::Rect)> = self
            .annotation.annotations
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_visible())
            .map(|(i, a)| (a.id(), i, screen_bbox(a)))
            .collect();

        let selected = self.annotation.selected_annotation;

        // 2. Paint committed annotations + selection highlight.
        for &(id, idx, bbox) in &draw_info {
            let ann = &self.annotation.annotations[idx];
            paint_annotation(painter, ann, canvas_min, canvas_scale);
            if selected == Some(id) {
                painter.rect_stroke(
                    bbox.expand(4.0),
                    2.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(0, 180, 255)),
                );
            }
        }

        // 3. Paint the in-progress annotation.
        if let Some(ann) = self.annotation.annotation_drawing.as_ref() {
            paint_annotation(painter, ann, canvas_min, canvas_scale);
        }

        // 4. Tool interaction
        // Inset the interaction rect so it doesn't overlap with side-panel
        // resize handles (which are rendered by egui and need to receive
        // drag events at the panel boundary).
        let interact_rect = {
            let mut r = canvas_rect;
            r.max.x -= 8.0;
            r
        };
        let tool = self.annotation.annotation_tool;
        match tool {
            // ---- Pen: drag to accumulate freehand points ----
            robs_core::AnnotationTool::Pen => {
                let resp = ui.interact(
                    interact_rect,
                    ui.make_persistent_id("annotation_pen_canvas"),
                    egui::Sense::drag(),
                );
                if resp.drag_started() {
                    if let Some(pos) = resp.hover_pos() {
                        let scene_pos = to_scene(pos);
                        let mut ann =
                            robs_core::Annotation::new(robs_core::AnnotationShape::Pen, scene_pos, scene_pos);
                        ann.set_style(self.annotation.annotation_style);
                        ann.push_point(scene_pos);
                        self.annotation.annotation_drawing = Some(ann);
                    }
                }
                if resp.dragged() {
                    if let Some(pos) = resp.hover_pos() {
                        if let Some(ann) = self.annotation.annotation_drawing.as_mut() {
                            ann.push_point(to_scene(pos));
                        }
                    }
                }
                if resp.drag_stopped() {
                    if let Some(ann) = self.annotation.annotation_drawing.take() {
                        if ann.points().len() > 1 {
                            self.log_event("Annotation added: Pen", EventLogKind::Annotation);
                            self.annotation.annotations.push(ann);
                        }
                    }
                }
            }
            // ---- Text: click to place, then type inline ----
            robs_core::AnnotationTool::Text => {
                let resp = ui.interact(
                    interact_rect,
                    ui.make_persistent_id("annotation_text_canvas"),
                    egui::Sense::click(),
                );
                if resp.clicked() {
                    if let Some(pos) = resp.hover_pos() {
                        let scene_pos = to_scene(pos);
                        let mut ann = robs_core::Annotation::new(
                            robs_core::AnnotationShape::Text,
                            scene_pos,
                            scene_pos,
                        );
                        ann.set_style(self.annotation.annotation_style);
                        self.log_event("Annotation added: Text", EventLogKind::Annotation);
                        self.annotation.annotations.push(ann);
                        let new_id = self.annotation.annotations.last().map(|a| a.id());
                        self.annotation.editing_text_id = new_id;
                        self.annotation.text_input.clear();
                    }
                }
            }
            // ---- Select: click to select, drag to move, Del to remove ----
            robs_core::AnnotationTool::Select => {
                let mut clicked_id = None;
                for &(id, _idx, bbox) in draw_info.iter().rev() {
                    let resp = ui.interact(
                        bbox.expand(6.0),
                        ui.make_persistent_id(format!("annotation_{}", id.0 .0)),
                        egui::Sense::click_and_drag(),
                    );
                    if clicked_id.is_none() && resp.clicked() {
                        clicked_id = Some(id);
                    }
                    if resp.dragged() {
                        let delta = resp.drag_delta();
                        let dx = delta.x / canvas_scale;
                        let dy = delta.y / canvas_scale;
                        if let Some(ann) =
                            self.annotation.annotations.iter_mut().find(|a| a.id() == id)
                        {
                            ann.translate(dx, dy);
                        }
                    }
                }
                if let Some(id) = clicked_id {
                    self.annotation.selected_annotation = Some(id);
                }
                if let Some(sel) = self.annotation.selected_annotation {
                    let delete_pressed = ui.input(|i| {
                        i.key_pressed(egui::Key::Delete)
                            || i.key_pressed(egui::Key::Backspace)
                    });
                    if delete_pressed {
                        self.annotation.annotations.retain(|a| a.id() != sel);
                        self.annotation.selected_annotation = None;
                    }
                }
            }
            // ---- Regular shapes (Arrow/Line/Rectangle/Ellipse): drag to create ----
            _ => {
                let shape = tool.shape().unwrap();
                let resp = ui.interact(
                    interact_rect,
                    ui.make_persistent_id("annotation_draw_canvas"),
                    egui::Sense::drag(),
                );
                if resp.drag_started() {
                    if let Some(pos) = resp.hover_pos() {
                        let scene_pos = to_scene(pos);
                        let mut ann = robs_core::Annotation::new(shape, scene_pos, scene_pos);
                        ann.set_style(self.annotation.annotation_style);
                        self.annotation.annotation_drawing = Some(ann);
                    }
                }
                if resp.dragged() {
                    if let Some(pos) = resp.hover_pos() {
                        if let Some(ann) = self.annotation.annotation_drawing.as_mut() {
                            ann.set_end(to_scene(pos));
                        }
                    }
                }
                if resp.drag_stopped() {
                    if let Some(ann) = self.annotation.annotation_drawing.take() {
                        let (w, h) = ann.size();
                        if w > 2.0 || h > 2.0 {
                            let shape_name = format!("{:?}", ann.shape());
                            self.log_event(format!("Annotation added: {}", shape_name), EventLogKind::Annotation);
                            self.annotation.annotations.push(ann);
                        }
                    }
                }
            }
        }

        // 5. Inline text editor for the annotation being edited.
        if let Some(edit_id) = self.annotation.editing_text_id {
            let exists = self.annotation.annotations.iter().any(|a| a.id() == edit_id);
            if !exists {
                self.annotation.editing_text_id = None;
            } else {
                let anchor = to_screen(
                    self.annotation.annotations
                        .iter()
                        .find(|a| a.id() == edit_id)
                        .map(|a| a.start())
                        .unwrap_or_default(),
                );
                let edit_rect = egui::Rect::from_min_size(
                    anchor,
                    egui::vec2(200.0, text_font_screen + 8.0),
                );
                let resp = ui.put(
                    edit_rect,
                    egui::TextEdit::singleline(&mut self.annotation.text_input)
                        .font(egui::FontId::proportional(text_font_screen))
                        .desired_width(200.0),
                );
                // Focus the field once on first appearance.
                if !resp.has_focus() && !resp.lost_focus() {
                    resp.request_focus();
                }
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if resp.lost_focus() || escape {
                    let text = std::mem::take(&mut self.annotation.text_input);
                    if text.is_empty() || escape {
                        self.annotation.annotations.retain(|a| a.id() != edit_id);
                    } else if let Some(ann) =
                        self.annotation.annotations.iter_mut().find(|a| a.id() == edit_id)
                    {
                        ann.set_text(text);
                    }
                    self.annotation.editing_text_id = None;
                }
            }
        }
    }

    /// Render draggable text overlays on the canvas.
    pub(crate) fn render_text_overlays(
        &mut self,
        ui: &mut egui::Ui,
        canvas_rect: egui::Rect,
        canvas_scale: f32,
    ) {
        if self.text_overlays.is_empty() {
            return;
        }
        let canvas_min = canvas_rect.min;
        let painter = ui.painter();

        // Collect overlay data for rendering + interaction.
        let items: Vec<(
            robs_core::ObjectId,
            String,
            egui::Pos2,
            f32,
            [u8; 4],
        )> = self
            .text_overlays
            .iter()
            .filter(|o| o.is_visible())
            .map(|o| {
                let screen_pos = egui::pos2(
                    canvas_min.x + o.position().x * canvas_scale,
                    canvas_min.y + o.position().y * canvas_scale,
                );
                let font_size = (o.font_size() * canvas_scale).max(8.0);
                (
                    o.id(),
                    o.text().to_string(),
                    screen_pos,
                    font_size,
                    o.color(),
                )
            })
            .collect();

        for (id, text, pos, font_size, color) in &items {
            let text_color = egui::Color32::from_rgba_unmultiplied(
                color[0], color[1], color[2], color[3],
            );
            let font = egui::FontId::proportional(*font_size);
            let galley =
                painter
                    .ctx()
                    .fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), text_color));
            let bg_rect = egui::Rect::from_min_size(
                *pos - egui::vec2(4.0, 2.0),
                galley.size() + egui::vec2(8.0, 4.0),
            );
            painter.rect_filled(bg_rect, 2.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 160));
            painter.text(*pos, egui::Align2::LEFT_TOP, text, font, text_color);

            // Drag to reposition
            let resp = ui.interact(
                bg_rect,
                ui.make_persistent_id(format!("text_overlay_{}", id.0)),
                egui::Sense::drag(),
            );
            if resp.dragged() {
                let delta = resp.drag_delta();
                let dx = delta.x / canvas_scale;
                let dy = delta.y / canvas_scale;
                if let Some(ov) = self.text_overlays.iter_mut().find(|o| o.id() == *id) {
                    let p = ov.position();
                    ov.set_position(Position::new(p.x + dx, p.y + dy));
                }
            }
        }
    }
}

/// Map an [`robs_core::AnnotationShape`] to its corresponding drawing tool.
fn tool_from_shape(shape: robs_core::AnnotationShape) -> robs_core::AnnotationTool {
    match shape {
        robs_core::AnnotationShape::Arrow => robs_core::AnnotationTool::Arrow,
        robs_core::AnnotationShape::Line => robs_core::AnnotationTool::Line,
        robs_core::AnnotationShape::Rectangle => robs_core::AnnotationTool::Rectangle,
        robs_core::AnnotationShape::Ellipse => robs_core::AnnotationTool::Ellipse,
        robs_core::AnnotationShape::Pen => robs_core::AnnotationTool::Pen,
        robs_core::AnnotationShape::Text => robs_core::AnnotationTool::Text,
    }
}

/// Paint a single annotation shape in screen coordinates.
///
/// `canvas_min` + `canvas_scale` are used to transform the annotation's
/// scene-space control points / polyline / text anchor into screen pixels.
fn paint_annotation(
    painter: &egui::Painter,
    ann: &robs_core::Annotation,
    canvas_min: egui::Pos2,
    canvas_scale: f32,
) {
    let to_screen = |p: Position| {
        egui::pos2(
            canvas_min.x + p.x * canvas_scale,
            canvas_min.y + p.y * canvas_scale,
        )
    };

    let style = ann.style();
    let color = egui::Color32::from_rgba_unmultiplied(
        style.color[0],
        style.color[1],
        style.color[2],
        style.color[3],
    );
    let stroke = egui::Stroke::new(style.stroke_width, color);
    let start = to_screen(ann.start());
    let end = to_screen(ann.end());

    match ann.shape() {
        robs_core::AnnotationShape::Line => {
            painter.line_segment([start, end], stroke);
        }
        robs_core::AnnotationShape::Arrow => {
            painter.line_segment([start, end], stroke);
            let delta = end - start;
            let dist = delta.length();
            if dist > 1.0 {
                let dir = delta / dist;
                let head_len = (stroke.width * 3.0).max(12.0);
                let head_ang = 0.5_f32;
                let perp = egui::vec2(-dir.y, dir.x);
                let back = end - dir * head_len;
                let left = back + perp * (head_len * head_ang.tan());
                let right = back - perp * (head_len * head_ang.tan());
                painter.line_segment([end, left], stroke);
                painter.line_segment([end, right], stroke);
            }
        }
        robs_core::AnnotationShape::Rectangle => {
            let r = egui::Rect::from_two_pos(start, end);
            if style.filled {
                painter.rect_filled(r, 0.0, color);
            }
            painter.rect_stroke(r, 0.0, stroke);
        }
        robs_core::AnnotationShape::Ellipse => {
            let r = egui::Rect::from_two_pos(start, end);
            let center = r.center();
            let rx = (r.width() / 2.0).abs();
            let ry = (r.height() / 2.0).abs();
            let n = 48usize;
            let points: Vec<egui::Pos2> = (0..n)
                .map(|i| {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    center + egui::vec2(rx * t.cos(), ry * t.sin())
                })
                .collect();
            if style.filled {
                painter.add(egui::epaint::Shape::convex_polygon(
                    points.clone(),
                    color,
                    stroke,
                ));
            }
            for w in points.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
            if n >= 2 {
                painter.line_segment([points[n - 1], points[0]], stroke);
            }
        }
        robs_core::AnnotationShape::Pen => {
            let pts: Vec<egui::Pos2> = ann.points().iter().map(|&p| to_screen(p)).collect();
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
        }
        robs_core::AnnotationShape::Text => {
            let text = ann.text();
            if !text.is_empty() {
                let font_size =
                    (robs_controller::annotation_raster::TEXT_FONT_SIZE * canvas_scale).max(8.0);
                let font = egui::FontId::proportional(font_size);
                painter.text(start, egui::Align2::LEFT_TOP, text, font, color);
            }
        }
    }
}
