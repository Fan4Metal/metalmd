//! Selecting text across the labels of a document. egui's own
//! `LabelSelectionState`, adapted: a right click (or a middle one) leaves
//! the selection as it is, so the context menu can act on it, and so does
//! a click in a menu above the document; each label tells its caller what
//! of it is selected, which the viewer maps to the document; and the
//! selection can be copied from a menu (`copy`), not only with Ctrl+C.
//!
//! Adapted from egui 0.36's `text_selection/label_text_selection.rs`,
//! copyright (c) 2018-2021 Emil Ernerfeldt, MIT OR Apache-2.0.

use egui::text::CCursor;
use egui::text_selection::text_cursor_state::cursor_rect;
use egui::text_selection::visuals::{RowVertexIndices, paint_text_selection};
use egui::text_selection::{CCursorRange, TextCursorState};
use egui::{
    Context, CursorIcon, Event, Galley, Id, LayerId, Pos2, Rect, Response, Ui, ViewportIdMap,
    emath::TSTransform, epaint, layers::ShapeIdx,
};
use std::sync::Arc;

/// One end of a text selection, inside any widget.
#[derive(Clone, Copy)]
struct WidgetTextCursor {
    widget_id: Id,
    ccursor: CCursor,
    /// Last known screen position.
    pos: Pos2,
}

impl WidgetTextCursor {
    fn new(
        widget_id: Id,
        cursor: impl Into<CCursor>,
        global_from_galley: TSTransform,
        galley: &Galley,
    ) -> Self {
        let ccursor = cursor.into();
        let pos = global_from_galley * pos_in_galley(galley, ccursor);
        Self {
            widget_id,
            ccursor,
            pos,
        }
    }
}

fn pos_in_galley(galley: &Galley, ccursor: CCursor) -> Pos2 {
    galley.pos_from_cursor(ccursor).center()
}

#[derive(Clone, Copy)]
struct CurrentSelection {
    /// The selection is in this layer, which keeps it to one window.
    layer_id: LayerId,
    /// Where the mouse was released, or what moves with Shift+arrows.
    /// The two ends can come in any order, and be equal (no selection).
    primary: WidgetTextCursor,
    /// Where the mouse was first pressed.
    secondary: WidgetTextCursor,
}

/// The text selection of the labels drawn with [`label`], per viewport
/// (they are drawn in separate passes). An egui plugin.
#[derive(Default)]
pub struct TextSelection {
    states: ViewportIdMap<ViewportState>,
}

struct ViewportState {
    selection: Option<CurrentSelection>,
    selection_bbox_last_frame: Rect,
    selection_bbox_this_frame: Rect,
    /// Any label hovered this frame?
    any_hovered: bool,
    /// Selecting by dragging.
    is_dragging: bool,
    has_reached_primary: bool,
    has_reached_secondary: bool,
    /// The selected text of this pass, in the order the labels were drawn.
    text: String,
    last_galley_rect: Option<Rect>,
    /// Copy `text` at the end of this pass ([`copy`]).
    copy_requested: bool,
    /// Painted selections this pass, undone if both ends were not seen.
    painted_selections: Vec<(ShapeIdx, Vec<RowVertexIndices>)>,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            selection: None,
            selection_bbox_last_frame: Rect::NOTHING,
            selection_bbox_this_frame: Rect::NOTHING,
            any_hovered: false,
            is_dragging: false,
            has_reached_primary: false,
            has_reached_secondary: false,
            text: String::new(),
            last_galley_rect: None,
            copy_requested: false,
            painted_selections: Vec::new(),
        }
    }
}

impl egui::plugin::Plugin for TextSelection {
    fn debug_name(&self) -> &'static str {
        "document text selection"
    }

    fn on_begin_pass(&mut self, ui: &mut Ui) {
        self.states
            .entry(ui.ctx().viewport_id())
            .or_default()
            .on_begin_pass();
    }

    fn on_end_pass(&mut self, ui: &mut Ui) {
        let viewport_id = ui.ctx().viewport_id();
        let state = self.states.entry(viewport_id).or_default();
        state.on_end_pass(ui);
        if state.selection.is_none() && !state.is_dragging {
            self.states.remove(&viewport_id);
        }
    }
}

impl TextSelection {
    /// Deselects everything, in every viewport.
    pub fn clear(&mut self) {
        self.states.clear();
    }
}

/// Copies the selection at the end of this pass, as Ctrl+C would.
pub fn copy(ctx: &Context) {
    let plugin = ctx.plugin::<TextSelection>();
    plugin
        .lock()
        .states
        .entry(ctx.viewport_id())
        .or_default()
        .copy_requested = true;
}

/// Draws a label's galley with its part of the selection, handling the
/// pointer and keys that select; gives that part (as characters of the
/// galley), `None` if none of it is selected.
pub fn label(
    ui: &Ui,
    response: &Response,
    galley_pos: Pos2,
    mut galley: Arc<Galley>,
    fallback_color: epaint::Color32,
    underline: epaint::Stroke,
) -> Option<CCursorRange> {
    let plugin = ui.ctx().plugin::<TextSelection>();
    let mut plugin = plugin.lock();
    let state = plugin.states.entry(ui.ctx().viewport_id()).or_default();
    let (range, new_vertex_indices) = state.on_label(ui, response, galley_pos, &mut galley);
    let shape_idx = ui
        .painter()
        .add(epaint::TextShape::new(galley_pos, galley, fallback_color).with_underline(underline));
    if !new_vertex_indices.is_empty() {
        state
            .painted_selections
            .push((shape_idx, new_vertex_indices));
    }
    range.filter(|range| !range.is_empty())
}

/// A press of the primary button this frame, which starts a selection or
/// ends one; the other buttons open menus and leave it alone.
fn primary_pressed(ui: &Ui) -> bool {
    ui.input(|i| i.pointer.primary_pressed())
}

impl ViewportState {
    fn on_begin_pass(&mut self) {
        self.selection_bbox_last_frame = self.selection_bbox_this_frame;
        self.selection_bbox_this_frame = Rect::NOTHING;
        self.any_hovered = false;
        self.has_reached_primary = false;
        self.has_reached_secondary = false;
        self.text.clear();
        self.last_galley_rect = None;
        self.painted_selections.clear();
    }

    fn on_end_pass(&mut self, ui: &Ui) {
        if self.is_dragging {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }

        if !self.has_reached_primary || !self.has_reached_secondary {
            // Both ends were not seen this frame (scrolled out of view, or
            // one disappeared): deselect, and hide what was painted of it.
            if let Some(selection) = self.selection.take() {
                ui.ctx().graphics_mut(|layers| {
                    if let Some(list) = layers.get_mut(selection.layer_id) {
                        for (shape_idx, row_selections) in self.painted_selections.drain(..) {
                            list.mutate_shape(shape_idx, |shape| {
                                if let epaint::Shape::Text(text_shape) = &mut shape.shape {
                                    hide_selection(text_shape, &row_selections);
                                }
                            });
                        }
                    }
                });
                self.text.clear();
            }
        }

        let pressed_escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        // A press in a menu or a window over the selection's layer leaves it.
        let pressed_here = primary_pressed(ui)
            && self.selection.is_some_and(|selection| {
                ui.input(|i| i.pointer.interact_pos())
                    .and_then(|pos| ui.ctx().layer_id_at(pos))
                    .is_none_or(|layer| layer == selection.layer_id)
            });
        if pressed_escape || (pressed_here && !self.any_hovered) {
            self.selection = None;
        }

        if ui.input(|i| i.pointer.any_released()) {
            self.is_dragging = false;
        }

        let copy = std::mem::take(&mut self.copy_requested)
            || ui.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, Event::Copy | Event::Cut))
            });
        if copy && !self.text.is_empty() {
            ui.ctx().copy_text(std::mem::take(&mut self.text));
        }
    }

    /// Adds a label's selected text to this pass's, joined as egui joins
    /// it: a line end (or two) below the last one, a space beside it.
    /// Unlike egui, a label with only the cursor in it adds nothing (egui
    /// copies all of it).
    fn add_text(&mut self, galley_rect: Rect, galley: &Galley, range: &CCursorRange) {
        let new_text = range.slice_str(galley);
        if new_text.is_empty() {
            return;
        }
        let Some(last) = self.last_galley_rect.replace(galley_rect) else {
            self.text = new_text.to_owned();
            return;
        };
        if self.text.is_empty() {
            self.text = new_text.to_owned();
            return;
        }
        if last.bottom() <= galley_rect.top() {
            self.text.push('\n');
            if estimate_row_height(galley) * 0.5 < galley_rect.top() - last.bottom() {
                self.text.push('\n');
            }
        } else {
            let ends_with_space = self.text.chars().last().map(char::is_whitespace);
            let starts_with_space_or_punctuation = new_text
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c.is_ascii_punctuation());
            if ends_with_space == Some(false) && !starts_with_space_or_punctuation {
                self.text.push(' ');
            }
        }
        self.text.push_str(new_text);
    }

    fn cursor_for(
        &mut self,
        ui: &Ui,
        response: &Response,
        global_from_galley: TSTransform,
        galley: &Galley,
    ) -> TextCursorState {
        let Some(selection) = &mut self.selection else {
            return TextCursorState::default();
        };
        if selection.layer_id != response.layer_id {
            return TextCursorState::default();
        }

        let galley_from_global = global_from_galley.inverse();
        let multi_widget_text_select = ui.style().interaction.multi_widget_text_select;
        let may_select_widget =
            multi_widget_text_select || selection.primary.widget_id == response.id;

        if self.is_dragging
            && may_select_widget
            && let Some(pointer_pos) = ui.ctx().pointer_interact_pos()
        {
            let galley_rect = global_from_galley * Rect::from_min_size(Pos2::ZERO, galley.size());
            let galley_rect = galley_rect.intersect(ui.clip_rect());
            let is_in_same_column = galley_rect
                .x_range()
                .intersects(self.selection_bbox_last_frame.x_range());
            let has_reached_primary =
                self.has_reached_primary || response.id == selection.primary.widget_id;
            let has_reached_secondary =
                self.has_reached_secondary || response.id == selection.secondary.widget_id;

            let new_primary = if response.contains_pointer() {
                Some(galley.cursor_from_pos((galley_from_global * pointer_pos).to_vec2()))
            } else if is_in_same_column
                && !self.has_reached_primary
                && selection.primary.pos.y <= selection.secondary.pos.y
                && pointer_pos.y <= galley_rect.top()
                && galley_rect.top() <= selection.secondary.pos.y
            {
                // Dragging upwards, above this, the first selected label.
                Some(galley.begin())
            } else if is_in_same_column
                && has_reached_secondary
                && has_reached_primary
                && selection.secondary.pos.y <= selection.primary.pos.y
                && selection.secondary.pos.y <= galley_rect.bottom()
                && galley_rect.bottom() <= pointer_pos.y
            {
                // Dragging downwards, below this label: all of it to its end.
                Some(galley.end())
            } else {
                None
            };

            if let Some(new_primary) = new_primary {
                selection.primary =
                    WidgetTextCursor::new(response.id, new_primary, global_from_galley, galley);
                // Not `drag_started`, which comes later.
                if primary_pressed(ui) && !ui.input(|i| i.modifiers.shift) {
                    selection.secondary = selection.primary;
                }
            }
        }

        let has_primary = response.id == selection.primary.widget_id;
        let has_secondary = response.id == selection.secondary.widget_id;
        if has_primary {
            selection.primary.pos =
                global_from_galley * pos_in_galley(galley, selection.primary.ccursor);
        }
        if has_secondary {
            selection.secondary.pos =
                global_from_galley * pos_in_galley(galley, selection.secondary.ccursor);
        }
        self.has_reached_primary |= has_primary;
        self.has_reached_secondary |= has_secondary;

        let primary = has_primary.then_some(selection.primary.ccursor);
        let secondary = has_secondary.then_some(selection.secondary.ccursor);

        // Labels are assumed to be drawn left to right, top to bottom, and
        // both ends to be met.
        match (primary, secondary) {
            (Some(primary), Some(secondary)) => TextCursorState::from(CCursorRange {
                primary,
                secondary,
                h_pos: None,
            }),
            (Some(primary), None) => {
                let secondary = if self.has_reached_secondary {
                    galley.begin()
                } else {
                    galley.end()
                };
                TextCursorState::from(CCursorRange {
                    primary,
                    secondary,
                    h_pos: None,
                })
            }
            (None, Some(secondary)) => {
                let primary = if self.has_reached_primary {
                    galley.begin()
                } else {
                    galley.end()
                };
                TextCursorState::from(CCursorRange {
                    primary,
                    secondary,
                    h_pos: None,
                })
            }
            (None, None) => {
                // Between the two ends, so all of it.
                if self.has_reached_primary != self.has_reached_secondary {
                    TextCursorState::from(CCursorRange::two(galley.begin(), galley.end()))
                } else {
                    TextCursorState::default()
                }
            }
        }
    }

    /// This label's selection, and what was painted of it.
    fn on_label(
        &mut self,
        ui: &Ui,
        response: &Response,
        galley_pos_in_layer: Pos2,
        galley: &mut Arc<Galley>,
    ) -> (Option<CCursorRange>, Vec<RowVertexIndices>) {
        let widget_id = response.id;
        let global_from_layer = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        let layer_from_galley = TSTransform::from_translation(galley_pos_in_layer.to_vec2());
        let galley_from_global = layer_from_galley.inverse() * global_from_layer.inverse();
        let global_from_galley = global_from_layer * layer_from_galley;

        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        self.any_hovered |= response.hovered();
        let primary_down = ui.input(|i| i.pointer.primary_down());
        self.is_dragging |= primary_down && response.is_pointer_button_down_on();

        let old_selection = self.selection;
        let mut cursor_state = self.cursor_for(ui, response, global_from_galley, galley);
        let old_range = cursor_state.range(galley);

        // A press of another button would start a selection here: only the
        // primary one may.
        let other_pressed = ui.input(|i| i.pointer.any_pressed() && !i.pointer.primary_pressed());
        if let Some(pointer_pos) = ui.ctx().pointer_interact_pos()
            && response.contains_pointer()
            && !other_pressed
        {
            let cursor_at_pointer =
                galley.cursor_from_pos((galley_from_global * pointer_pos).to_vec2());
            // Start of a drag and double or triple clicks; dragging itself
            // is handled in `cursor_for`.
            cursor_state.pointer_interaction(ui, response, cursor_at_pointer, galley, false);
        }

        if let Some(mut cursor_range) = cursor_state.range(galley) {
            let galley_rect = global_from_galley * Rect::from_min_size(Pos2::ZERO, galley.size());
            self.selection_bbox_this_frame |= galley_rect;
            if let Some(selection) = &self.selection
                && selection.primary.widget_id == response.id
            {
                process_selection_key_events(ui.ctx(), galley, response.id, &mut cursor_range);
            }
            self.add_text(galley_rect, galley, &cursor_range);
            cursor_state.set_char_range(Some(cursor_range));
        }

        let new_range = cursor_state.range(galley);
        if let (true, Some(range)) = (old_range != new_range, new_range) {
            if let Some(selection) = &mut self.selection {
                let primary_changed = Some(range.primary) != old_range.map(|r| r.primary);
                let secondary_changed = Some(range.secondary) != old_range.map(|r| r.secondary);
                selection.layer_id = response.layer_id;
                if primary_changed || !ui.style().interaction.multi_widget_text_select {
                    selection.primary =
                        WidgetTextCursor::new(widget_id, range.primary, global_from_galley, galley);
                    self.has_reached_primary = true;
                }
                if secondary_changed || !ui.style().interaction.multi_widget_text_select {
                    selection.secondary = WidgetTextCursor::new(
                        widget_id,
                        range.secondary,
                        global_from_galley,
                        galley,
                    );
                    self.has_reached_secondary = true;
                }
            } else {
                // A new selection.
                let cursor = |at| WidgetTextCursor::new(widget_id, at, global_from_galley, galley);
                self.selection = Some(CurrentSelection {
                    layer_id: response.layer_id,
                    primary: cursor(range.primary),
                    secondary: cursor(range.secondary),
                });
                self.has_reached_primary = true;
                self.has_reached_secondary = true;
            }
        }

        // Keep the moving end in view.
        if let Some(range) = new_range
            && let Some(new_primary) = self.selection.as_ref().map(|s| s.primary)
        {
            let old_primary = old_selection.map(|s| s.primary);
            let primary_changed = old_primary.is_none_or(|old| {
                old.widget_id != new_primary.widget_id || old.ccursor != new_primary.ccursor
            });
            if primary_changed
                && new_primary.widget_id == widget_id
                && old_range != new_range
                && !ui.clip_rect().contains_rect(response.rect)
            {
                let row_height = estimate_row_height(galley);
                let rect = global_from_galley * cursor_rect(galley, &range.primary, row_height);
                ui.scroll_to_rect(rect, None);
            }
        }

        let range = cursor_state.range(galley);
        let mut new_vertex_indices = vec![];
        if let Some(range) = range {
            paint_text_selection(galley, ui.visuals(), &range, Some(&mut new_vertex_indices));
        }
        (range, new_vertex_indices)
    }
}

/// Makes painted selection rectangles transparent again.
fn hide_selection(text_shape: &mut epaint::TextShape, rows: &[RowVertexIndices]) {
    let galley = Arc::make_mut(&mut text_shape.galley);
    for row_selection in rows {
        let Some(placed_row) = galley.rows.get_mut(row_selection.row) else {
            continue;
        };
        let row = Arc::make_mut(&mut placed_row.row);
        for &index in &row_selection.vertex_indices {
            if let Some(vertex) = row.visuals.mesh.vertices.get_mut(index as usize) {
                vertex.color = epaint::Color32::TRANSPARENT;
            }
        }
    }
}

fn process_selection_key_events(
    ctx: &Context,
    galley: &Galley,
    widget_id: Id,
    cursor_range: &mut CCursorRange,
) {
    let os = ctx.os();
    ctx.input(|i| {
        // `ctx` is locked here: nothing in this may call into it.
        for event in &i.events {
            cursor_range.on_event(os, event, galley, widget_id);
        }
    });
}

fn estimate_row_height(galley: &Galley) -> f32 {
    galley
        .rows
        .first()
        .map_or(galley.size().y, |row| row.height())
}
