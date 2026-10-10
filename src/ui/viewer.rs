//! Draws a `Document` with egui.
//!
//! Only the blocks in view are laid out each frame. Heights of the others are
//! estimated, then measured a few at a time in the background of later frames,
//! and the scroll position is corrected whenever a block above the view turns
//! out taller or shorter than estimated, so the text being read never jumps.

use crate::markdown::document::{
    Align, Block, Document, ImageRef, Inline, ListItem, Rich, Style, Table,
};
use crate::markdown::highlight::{Highlighter, HlSpan};
use crate::markdown::preprocess::percent_decode;
use crate::style::{BODY_SIZE, FontChoice, MAX_CONTENT_WIDTH, Palette};
use crate::ui::find::{FindState, Match};
use egui::text::{LayoutJob, TextFormat};
use egui::text_selection::LabelSelectionState;
use egui::{
    Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Galley, Id, Label, Layout,
    Margin, Rect, Response, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetText,
    pos2, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Space between blocks.
const GAP: f32 = BODY_SIZE * 0.9;
const LINE_HEIGHT: f32 = 1.5;
const CODE_SCALE: f32 = 0.875;
const SIDE_PADDING: f32 = 28.0;
const TOP_PADDING: f32 = 20.0;
const BOTTOM_PADDING: f32 = 120.0;
/// Time per frame spent measuring blocks that are not on screen.
const MEASURE_BUDGET: Duration = Duration::from_millis(4);

/// Something the user asked for by clicking in the document.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewAction {
    OpenLink(String),
}

/// Per-tab layout and scroll state.
#[derive(Default)]
pub struct ViewState {
    heights: Vec<f32>,
    measured: Vec<bool>,
    measured_width: f32,
    measured_fonts: u64,
    doc_identity: usize,
    /// Prefix sums of `heights` as used by the last frame (len = blocks + 1).
    pub offsets: Vec<f32>,
    /// First visible block and its top, as laid out last frame.
    anchor: Option<(usize, f32)>,
    /// Scroll so that block `.0` starts `.1` points below the top of the view.
    pub jump: Option<(usize, f32)>,
    /// Frames left in which `settle` re-applies the last jump while heights
    /// above the target are still being measured.
    settle: u8,
    settle_target: Option<(usize, f32)>,
    /// Scroll by this many points next frame (keyboard scrolling).
    pub scroll_delta: f32,
    pub viewport_top: f32,
    pub viewport_height: f32,
    pub first_visible: usize,
    hover: Option<(u32, u32)>,
    scroll_id: Option<Id>,
    measure_cursor: usize,
}

impl ViewState {
    /// Scroll position to keep across a reload: first visible block and the
    /// offset into it.
    pub fn reading_position(&self) -> Option<(usize, f32)> {
        let top = *self.offsets.get(self.first_visible)?;
        Some((self.first_visible, top - self.viewport_top))
    }

    pub fn jump_to(&mut self, block: usize, offset: f32) {
        self.jump = Some((block, offset));
    }

    fn prepare(&mut self, doc: &Document, width: f32, fonts: u64) {
        let identity = doc as *const Document as usize;
        if identity != self.doc_identity || self.heights.len() != doc.blocks.len() {
            self.doc_identity = identity;
            self.heights = doc.blocks.iter().map(|b| estimate(b, width)).collect();
            self.measured = vec![false; doc.blocks.len()];
            self.measured_width = width;
            self.measured_fonts = fonts;
            self.anchor = None;
            self.measure_cursor = 0;
        } else if (self.measured_width - width).abs() > 0.5 || self.measured_fonts != fonts {
            // Keep the old heights as estimates; they are closer than new ones.
            self.measured.fill(false);
            self.measured_width = width;
            self.measured_fonts = fonts;
        }
    }

    fn compute_offsets(&self) -> Vec<f32> {
        let mut offsets = Vec::with_capacity(self.heights.len() + 1);
        let mut y = TOP_PADDING;
        offsets.push(y);
        for h in &self.heights {
            y += h;
            offsets.push(y);
        }
        offsets
    }
}

/// Everything a frame of rendering needs besides the document.
pub struct RenderCtx<'a> {
    pub palette: &'a Palette,
    pub fonts: &'a FontChoice,
    /// Changes whenever the font set changes (CJK fallback loaded).
    pub fonts_generation: u64,
    pub highlighter: &'a Highlighter,
    pub base_dir: Option<&'a Path>,
    pub find: &'a FindState,
    pub actions: Vec<ViewAction>,
    hover: Option<(u32, u32)>,
    new_hover: Option<(u32, u32)>,
}

impl<'a> RenderCtx<'a> {
    pub fn new(
        palette: &'a Palette,
        fonts: &'a FontChoice,
        fonts_generation: u64,
        highlighter: &'a Highlighter,
        base_dir: Option<&'a Path>,
        find: &'a FindState,
    ) -> Self {
        RenderCtx {
            palette,
            fonts,
            fonts_generation,
            highlighter,
            base_dir,
            find,
            actions: Vec::new(),
            hover: None,
            new_hover: None,
        }
    }
}

/// Draws the document into the remaining space of `ui`.
pub fn show_document(
    ui: &mut Ui,
    doc: &Document,
    view: &mut ViewState,
    rc: &mut RenderCtx<'_>,
    salt: Id,
) {
    let count = doc.blocks.len();
    let total_width = ui.available_width();
    let content_width = (total_width - 2.0 * SIDE_PADDING)
        .clamp(120.0, MAX_CONTENT_WIDTH)
        .floor();
    view.prepare(doc, content_width, rc.fonts_generation);
    rc.hover = view.hover;
    rc.new_hover = None;

    let offsets = view.compute_offsets();
    let mut area = ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false]);

    if let Some((block, delta)) = view.jump.take() {
        view.settle = 8;
        view.settle_target = Some((block, delta));
    }
    if let Some((block, delta)) = view.settle_target {
        let target = offsets[block.min(count)] - delta;
        area = area.vertical_scroll_offset(target.max(0.0));
        view.settle = view.settle.saturating_sub(1);
        if view.settle == 0 {
            view.settle_target = None;
        }
        // A jump wins over anchoring; the anchor is re-taken below.
        view.anchor = None;
    } else if let (Some((index, old_top)), Some(id)) = (view.anchor, view.scroll_id) {
        let delta = offsets[index.min(count)] - old_top;
        if delta.abs() > 0.5
            && let Some(state) = egui::scroll_area::State::load(ui.ctx(), id)
        {
            area = area.vertical_scroll_offset((state.offset.y + delta).max(0.0));
        }
    }
    // Any user scrolling cancels the settling of a jump.
    if view.settle_target.is_some() && ui.input(|i| i.smooth_scroll_delta.y != 0.0) {
        view.settle_target = None;
        view.settle = 0;
    }

    let scroll_delta = std::mem::take(&mut view.scroll_delta);
    let output = area.show_viewport(ui, |ui, viewport| {
        let origin = ui.min_rect().min;
        let left = origin.x
            + ((ui.available_width() - content_width) / 2.0)
                .max(SIDE_PADDING)
                .floor();
        ui.set_min_height(offsets[count] + BOTTOM_PADDING);
        if scroll_delta != 0.0 {
            ui.scroll_with_delta(vec2(0.0, scroll_delta));
        }
        if count == 0 {
            return;
        }

        let first = offsets
            .partition_point(|&o| o <= viewport.min.y)
            .saturating_sub(1)
            .min(count - 1);
        // Visible blocks are placed one after another at their real heights,
        // starting from the estimated top of the first one.
        let mut y = offsets[first];
        let mut index = first;
        while index < count && y < viewport.max.y + 1.0 {
            let rect = Rect::from_min_size(pos2(left, origin.y + y), vec2(content_width, 1.0e5));
            let height = draw_top_block(ui, doc, index, rect, rc, false);
            view.heights[index] = height;
            view.measured[index] = true;
            y += height;
            index += 1;
        }

        // Measure off-screen blocks within a small time budget.
        let started = Instant::now();
        let mut remaining = view.measured.iter().filter(|m| !**m).count();
        let mut cursor = view.measure_cursor.min(count - 1);
        while remaining > 0 && started.elapsed() < MEASURE_BUDGET {
            if !view.measured[cursor] {
                let rect = Rect::from_min_size(
                    pos2(left, origin.y + offsets[cursor]),
                    vec2(content_width, 1.0e5),
                );
                view.heights[cursor] = draw_top_block(ui, doc, cursor, rect, rc, true);
                view.measured[cursor] = true;
                remaining -= 1;
            }
            cursor = (cursor + 1) % count;
        }
        view.measure_cursor = cursor;
        if remaining > 0 {
            ui.ctx().request_repaint();
        }

        view.anchor = Some((first, offsets[first]));
        view.first_visible = first;
        view.viewport_top = viewport.min.y;
        view.viewport_height = viewport.height();
    });
    view.scroll_id = Some(output.id);
    view.offsets = offsets;
    if view.hover != rc.new_hover {
        view.hover = rc.new_hover;
        ui.ctx().request_repaint();
    }
}

/// Draws (or, when `invisible`, only measures) top-level block `index` and
/// returns its height including the gap after it.
fn draw_top_block(
    ui: &mut Ui,
    doc: &Document,
    index: usize,
    rect: Rect,
    rc: &mut RenderCtx<'_>,
    invisible: bool,
) -> f32 {
    let mut builder = UiBuilder::new()
        .max_rect(rect)
        .id_salt(("block", index))
        .layout(Layout::top_down(egui::Align::Min));
    if invisible {
        builder = builder.invisible();
    }
    let block = &doc.blocks[index];
    let previous = index.checked_sub(1).map(|i| &doc.blocks[i]);
    let response = ui.scope_builder(builder, |ui| {
        ui.set_width(rect.width());
        if matches!(block, Block::Footnote { .. })
            && !matches!(previous, Some(Block::Footnote { .. }))
        {
            // Separate the footnotes from the text above them.
            let (line, _) = ui.allocate_exact_size(vec2(rect.width(), GAP), Sense::hover());
            ui.painter().hline(
                line.x_range(),
                line.center().y,
                Stroke::new(1.0, rc.palette.border),
            );
        }
        draw_block(ui, block, rc, index == 0, index as u64);
    });
    let gap = if index + 1 < doc.blocks.len() {
        GAP
    } else {
        0.0
    };
    response.response.rect.height() + gap
}

fn draw_blocks(ui: &mut Ui, blocks: &[Block], rc: &mut RenderCtx<'_>, gap: f32, salt: u64) {
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            ui.add_space(gap);
        }
        draw_block(
            ui,
            block,
            rc,
            i == 0,
            salt.wrapping_mul(31).wrapping_add(i as u64 + 1),
        );
    }
}

fn heading_size(level: u8) -> f32 {
    BODY_SIZE
        * match level {
            1 => 2.0,
            2 => 1.5,
            3 => 1.25,
            4 => 1.0,
            5 => 0.875,
            _ => 0.85,
        }
}

fn draw_block(ui: &mut Ui, block: &Block, rc: &mut RenderCtx<'_>, first: bool, salt: u64) {
    match block {
        Block::Heading {
            level,
            inlines,
            align,
            ..
        } => {
            if !first {
                ui.add_space(BODY_SIZE * 0.5);
            }
            let size = heading_size(*level);
            draw_inlines(ui, inlines, rc, size, Style::STRONG, *align, salt, true);
            if *level <= 2 {
                ui.add_space(BODY_SIZE * 0.3);
                let width = ui.available_width();
                let (line, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
                ui.painter().hline(
                    line.x_range(),
                    line.center().y,
                    Stroke::new(1.0, rc.palette.border),
                );
            }
        }
        Block::Paragraph { inlines, align } => {
            draw_inlines(ui, inlines, rc, BODY_SIZE, Style::NONE, *align, salt, false);
        }
        Block::Code {
            id,
            key,
            lang,
            code,
        } => {
            draw_code(ui, *id, Some(*key), lang, code, rc, salt);
        }
        Block::Math { id, tex } => draw_code(ui, *id, None, "math", tex, rc, salt),
        Block::FrontMatter { id, yaml } => {
            egui::CollapsingHeader::new(
                egui::RichText::new("Front matter").color(rc.palette.muted),
            )
            .id_salt(("front-matter", salt))
            .default_open(false)
            .show(ui, |ui| draw_code(ui, *id, None, "yaml", yaml, rc, salt));
        }
        Block::Quote(blocks) => {
            let bar = rc.palette.quote_bar;
            let muted = rc.palette.muted;
            draw_barred(ui, bar, |ui| {
                ui.visuals_mut().override_text_color = Some(muted);
                draw_blocks(ui, blocks, rc, GAP, salt);
            });
        }
        Block::Alert {
            kind,
            title,
            blocks,
        } => {
            let color = rc.palette.alert(*kind);
            draw_barred(ui, color, |ui| {
                let title = egui::RichText::new(title)
                    .color(color)
                    .size(BODY_SIZE)
                    .family(rc.fonts.bold.clone());
                ui.add(Label::new(title).selectable(true));
                ui.add_space(4.0);
                draw_blocks(ui, blocks, rc, GAP, salt);
            });
        }
        Block::List {
            ordered,
            start,
            tight,
            items,
        } => draw_list(ui, *ordered, *start, *tight, items, rc, salt),
        Block::Table(table) => draw_table(ui, table, rc, salt),
        Block::DefinitionList(items) => {
            for (i, (terms, details)) in items.iter().enumerate() {
                if i > 0 {
                    ui.add_space(GAP * 0.5);
                }
                draw_blocks(ui, terms, rc, 2.0, salt.wrapping_add(i as u64 * 7919));
                for (j, detail) in details.iter().enumerate() {
                    ui.horizontal_top(|ui| {
                        ui.add_space(BODY_SIZE * 1.5);
                        ui.vertical(|ui| {
                            draw_blocks(
                                ui,
                                detail,
                                rc,
                                GAP * 0.5,
                                salt.wrapping_add((i * 131 + j) as u64),
                            )
                        });
                    });
                }
            }
        }
        Block::Footnote { label, blocks } => {
            let muted = rc.palette.muted;
            ui.horizontal_top(|ui| {
                let size = BODY_SIZE * 0.875;
                let (marker, _) = ui
                    .allocate_exact_size(vec2(BODY_SIZE * 2.0, size * LINE_HEIGHT), Sense::hover());
                ui.painter().text(
                    pos2(marker.right() - 6.0, marker.center().y),
                    egui::Align2::RIGHT_CENTER,
                    format!("{label}."),
                    FontId::proportional(size),
                    muted,
                );
                ui.vertical(|ui| {
                    ui.visuals_mut().override_text_color = Some(muted);
                    draw_blocks(ui, blocks, rc, GAP * 0.5, salt);
                });
            });
        }
        Block::Rule => {
            let width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(width, BODY_SIZE), Sense::hover());
            ui.painter().rect_filled(
                Rect::from_center_size(rect.center(), vec2(width, 3.0)),
                CornerRadius::same(1),
                rc.palette.border,
            );
        }
    }
}

/// A block with a colored bar on its left (quotes, alerts).
fn draw_barred(ui: &mut Ui, color: Color32, add: impl FnOnce(&mut Ui)) {
    let response = ui.horizontal_top(|ui| {
        ui.add_space(4.0 + BODY_SIZE * 0.9);
        ui.vertical(add);
    });
    let rect = response.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(rect.left_top(), pos2(rect.left() + 4.0, rect.bottom())),
        CornerRadius::same(2),
        color,
    );
}

fn draw_list(
    ui: &mut Ui,
    ordered: bool,
    start: usize,
    tight: bool,
    items: &[ListItem],
    rc: &mut RenderCtx<'_>,
    salt: u64,
) {
    let depth = ui
        .data(|d| d.get_temp::<u32>(Id::new("list-depth")))
        .unwrap_or(0);
    let digits = (start + items.len()).to_string().len() as f32;
    let marker_width = if ordered {
        BODY_SIZE * (0.75 + 0.55 * digits)
    } else {
        BODY_SIZE * 1.5
    };
    let line = BODY_SIZE * LINE_HEIGHT;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            ui.add_space(if tight { 2.0 } else { GAP * 0.6 });
        }
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (marker, _) = ui.allocate_exact_size(vec2(marker_width, line), Sense::hover());
            let painter = ui.painter();
            let text_color = ui.visuals().text_color();
            let center_y = marker.center().y;
            match item.task {
                Some(checked) => {
                    let side = BODY_SIZE * 0.85;
                    let rect = Rect::from_center_size(
                        pos2(marker.right() - side / 2.0 - 6.0, center_y),
                        vec2(side, side),
                    );
                    if checked {
                        painter.rect_filled(rect, CornerRadius::same(3), rc.palette.accent);
                        let a = rect.left_center() + vec2(side * 0.22, 0.0);
                        let b = rect.center_bottom() + vec2(-side * 0.08, -side * 0.25);
                        let c = rect.right_top() + vec2(-side * 0.2, side * 0.25);
                        let stroke = Stroke::new(2.0, Color32::WHITE);
                        painter.line_segment([a, b], stroke);
                        painter.line_segment([b, c], stroke);
                    } else {
                        painter.rect_stroke(
                            rect,
                            CornerRadius::same(3),
                            Stroke::new(1.5, rc.palette.muted),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                None if ordered => {
                    let _ = painter.text(
                        pos2(marker.right() - 6.0, center_y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{}.", start + i),
                        FontId::proportional(BODY_SIZE),
                        text_color,
                    );
                }
                None => {
                    let center = pos2(marker.right() - 10.0, center_y);
                    match depth % 3 {
                        0 => painter.circle_filled(center, 2.6, text_color),
                        1 => painter.circle_stroke(center, 2.6, Stroke::new(1.2, text_color)),
                        _ => painter.rect_filled(
                            Rect::from_center_size(center, vec2(4.5, 4.5)),
                            CornerRadius::ZERO,
                            text_color,
                        ),
                    };
                }
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.data_mut(|d| d.insert_temp(Id::new("list-depth"), depth + 1));
                let gap = if tight { 2.0 } else { GAP * 0.6 };
                draw_blocks(
                    ui,
                    &item.blocks,
                    rc,
                    gap,
                    salt.wrapping_mul(17).wrapping_add(i as u64),
                );
                ui.data_mut(|d| d.insert_temp(Id::new("list-depth"), depth));
            });
        });
    }
}

fn draw_table(ui: &mut Ui, table: &Table, rc: &mut RenderCtx<'_>, salt: u64) {
    let border = rc.palette.border;
    let columns = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    // Natural column widths from the previous frame, used to center and
    // right-align cells without letting them stretch their column.
    let widths_id = Id::new(("table-widths", salt));
    let previous: Vec<f32> = ui.data(|d| d.get_temp(widths_id)).unwrap_or_default();
    let mut measured = vec![0.0f32; columns];
    Frame::new()
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ScrollArea::horizontal()
                .id_salt(("table-scroll", salt))
                .auto_shrink([true, true])
                .show(ui, |ui| {
                    egui::Grid::new(("table", salt))
                        .num_columns(columns)
                        .striped(true)
                        .spacing(vec2(20.0, 8.0))
                        .min_col_width(24.0)
                        .max_col_width(420.0)
                        .show(ui, |ui| {
                            let rows = std::iter::once(&table.header).chain(table.rows.iter());
                            for (r, row) in rows.enumerate() {
                                for (c, column_max) in measured.iter_mut().enumerate() {
                                    let cell = row.get(c).map_or(&[][..], Vec::as_slice);
                                    let align = table.aligns.get(c).copied().unwrap_or_default();
                                    let cell_salt = salt.wrapping_add((r * 1000 + c) as u64 + 1);
                                    let width = draw_cell(
                                        ui,
                                        cell,
                                        align,
                                        previous.get(c).copied(),
                                        rc,
                                        cell_salt,
                                    );
                                    *column_max = column_max.max(width);
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    if measured != previous {
        ui.data_mut(|d| d.insert_temp(widths_id, measured));
        ui.ctx().request_repaint();
    }
}

/// Draws one table cell and returns its natural (unpadded) width.
fn draw_cell(
    ui: &mut Ui,
    cell: &[Inline],
    align: Align,
    column_width: Option<f32>,
    rc: &mut RenderCtx<'_>,
    salt: u64,
) -> f32 {
    if cell.is_empty() {
        ui.label("");
        return 0.0;
    }
    let width_id = Id::new(("cell-width", salt));
    let natural: Option<f32> = ui.data(|d| d.get_temp(width_id));
    let pad = match (align, column_width, natural) {
        (Align::Center, Some(column), Some(width)) => ((column - width) / 2.0).max(0.0).floor(),
        (Align::Right, Some(column), Some(width)) => (column - width).max(0.0).floor(),
        _ => 0.0,
    };
    let response = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(pad);
        ui.vertical(|ui| {
            draw_inlines(
                ui,
                cell,
                rc,
                BODY_SIZE * 0.95,
                Style::NONE,
                Align::Left,
                salt,
                false,
            )
        })
        .response
        .rect
        .width()
    });
    let width = response.inner;
    if natural.is_none_or(|n| (n - width).abs() > 0.5) {
        ui.data_mut(|d| d.insert_temp(width_id, width));
        ui.ctx().request_repaint();
    }
    width
}

/// Draws a run of text and images. `extra` is added to every span's style.
#[allow(clippy::too_many_arguments)]
fn draw_inlines(
    ui: &mut Ui,
    inlines: &[Inline],
    rc: &mut RenderCtx<'_>,
    size: f32,
    extra: Style,
    align: Align,
    salt: u64,
    heading: bool,
) {
    if let [Inline::Text(rich)] = inlines {
        match align {
            Align::Left => {
                draw_rich(ui, rich, rc, size, extra, heading);
            }
            Align::Center => {
                ui.vertical_centered(|ui| draw_rich(ui, rich, rc, size, extra, heading));
            }
            Align::Right => {
                ui.with_layout(Layout::top_down(egui::Align::Max), |ui| {
                    draw_rich(ui, rich, rc, size, extra, heading)
                });
            }
        }
        return;
    }

    // Draws the items and tells whether they took more than one row: an item
    // starts below the row so far, or a text run wraps by itself. A tall
    // image alone is still one row.
    let mut flow = |ui: &mut Ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
        let mut row_bottom = f32::NEG_INFINITY;
        let mut wrapped = false;
        for inline in inlines {
            let rect = match inline {
                Inline::Text(rich) => {
                    // Not the height: in a row with a tall image egui makes
                    // the text's first row as tall as the image.
                    let (response, rows) = draw_rich_rows(ui, rich, rc, size, extra, heading);
                    wrapped |= rows > 1;
                    response.rect
                }
                Inline::Image(image) => draw_image(ui, image, rc).rect,
            };
            if row_bottom.is_finite() {
                wrapped |= rect.min.y >= row_bottom - 0.5;
            }
            row_bottom = row_bottom.max(rect.max.y);
        }
        wrapped
    };
    if align == Align::Left {
        ui.horizontal_wrapped(|ui| flow(ui));
        return;
    }
    // Wrapped rows cannot be centered by egui's layouts, so remember the
    // row's width from the previous frame and indent by half the slack.
    let id = ui.id().with(("aligned-row", salt));
    let previous: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(-1.0);
    let available = ui.available_width();
    let response = ui.horizontal_wrapped(|ui| {
        let pad = if previous > 0.0 && previous < available {
            match align {
                Align::Center => ((available - previous) / 2.0).floor(),
                _ => (available - previous).floor(),
            }
        } else {
            0.0
        };
        ui.add_space(pad);
        let start = ui.cursor().min.x;
        if flow(ui) {
            available
        } else {
            (ui.min_rect().max.x - start).max(1.0)
        }
    });
    let width = response.inner;
    if (width - previous).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, width));
        ui.ctx().request_repaint();
    }
}

fn text_format(rc: &RenderCtx<'_>, style: Style, size: f32) -> TextFormat {
    let palette = rc.palette;
    let mono = style.has(Style::CODE) || style.has(Style::MATH);
    let small = style.has(Style::SUP) || style.has(Style::SUB);
    let font_size = if small {
        size * 0.75
    } else if mono {
        size * CODE_SCALE
    } else {
        size
    };
    let family = if mono {
        FontFamily::Monospace
    } else {
        match (style.has(Style::STRONG), style.has(Style::EMPH)) {
            (true, true) => rc.fonts.bold_italic.clone(),
            (true, false) => rc.fonts.bold.clone(),
            (false, true) => rc.fonts.italic.clone(),
            (false, false) => FontFamily::Proportional,
        }
    };
    let color = if style.has(Style::LINK) {
        palette.link
    } else {
        Color32::PLACEHOLDER
    };
    let background = if style.has(Style::MARK) {
        palette.mark_bg
    } else if mono {
        palette.code_bg
    } else {
        Color32::TRANSPARENT
    };
    let line_stroke_color = if style.has(Style::LINK) {
        palette.link
    } else {
        palette.text
    };
    // Every run gets the paragraph's line height, so rows are evenly spaced.
    // epaint puts each run's baseline one ascent below the top of the row;
    // a smaller or monospace run is pushed down (bottom-aligned within a
    // shorter line height) by the difference in ascents, so all runs share
    // the body text's baseline.
    let line_height = (size * LINE_HEIGHT).round();
    let body_ascent = rc.fonts.body_ascent * size;
    let run_ascent = font_size
        * if mono {
            rc.fonts.mono_ascent
        } else {
            rc.fonts.body_ascent
        };
    let (run_line_height, valign) = if style.has(Style::SUP) {
        (line_height, egui::Align::TOP)
    } else if style.has(Style::SUB) {
        (
            line_height - (body_ascent - run_ascent) - size * 0.2,
            egui::Align::BOTTOM,
        )
    } else if (body_ascent - run_ascent).abs() > 0.01 {
        (
            line_height - (body_ascent - run_ascent),
            egui::Align::BOTTOM,
        )
    } else {
        (line_height, egui::Align::BOTTOM)
    };
    TextFormat {
        font_id: FontId::new(font_size, family),
        line_height: Some(run_line_height),
        color,
        background,
        expand_bg: if mono { 2.0 } else { 0.0 },
        italics: style.has(Style::EMPH) && rc.fonts.fake_italic && !mono,
        underline: if style.has(Style::INSERT) {
            Stroke::new(1.0, line_stroke_color)
        } else {
            Stroke::NONE
        },
        strikethrough: if style.has(Style::STRIKE) {
            Stroke::new(1.0, line_stroke_color)
        } else {
            Stroke::NONE
        },
        valign,
        ..Default::default()
    }
}

/// Appends `text[range]` with `format`, painting search matches inside it.
fn append_with_matches(
    job: &mut LayoutJob,
    text: &str,
    range: std::ops::Range<usize>,
    format: &TextFormat,
    matches: &[Match],
    current: Option<usize>,
    palette: &Palette,
) {
    let mut pos = range.start;
    while pos < range.end {
        let mut end = range.end;
        let mut hit = None;
        for (k, m) in matches.iter().enumerate() {
            if m.range.end <= pos {
                continue;
            }
            if m.range.start <= pos {
                end = end.min(m.range.end);
                hit = Some(k);
            } else {
                end = end.min(m.range.start);
            }
            break;
        }
        let mut piece = format.clone();
        if let Some(k) = hit {
            piece.background = if Some(k) == current {
                palette.search_current_bg
            } else {
                palette.search_bg
            };
            if piece.color == Color32::PLACEHOLDER && palette.dark {
                piece.color = Color32::WHITE;
            }
        }
        job.append(&text[pos..end], 0.0, piece);
        pos = end;
    }
}

fn rich_job(rich: &Rich, rc: &RenderCtx<'_>, size: f32, extra: Style) -> LayoutJob {
    let (matches, current) = rc.find.matches_in(rich.id);
    let mut job = LayoutJob::default();
    for span in &rich.spans {
        let mut format = text_format(rc, span.style | extra, size);
        if let (Some(link), Some((id, hovered))) = (span.link, rc.hover)
            && id == rich.id
            && hovered == link
        {
            format.underline = Stroke::new(1.0, rc.palette.link);
        }
        append_with_matches(
            &mut job,
            &rich.text,
            span.range.clone(),
            &format,
            matches,
            current,
            rc.palette,
        );
    }
    // A row of only inline code would otherwise be shorter than a row of
    // text, which shows as misaligned table cells.
    job.first_row_min_height = (size * LINE_HEIGHT).round();
    job
}

/// Byte offset of the character under `local` (relative to the galley).
fn char_under(galley: &Galley, local: Vec2, text: &str) -> Option<usize> {
    let point = local.to_pos2();
    if !galley
        .rows
        .iter()
        .any(|row| row.rect().expand2(vec2(0.0, 1.0)).contains(point))
    {
        return None;
    }
    let cursor = galley.cursor_from_pos(local);
    let mut index = cursor.index.0;
    // The cursor is the nearest boundary; step back if it is right of the
    // pointer so the character under the pointer is the one reported.
    if index > 0 && galley.pos_from_cursor(cursor).min.x > point.x {
        index -= 1;
    }
    text.char_indices().nth(index).map(|(byte, _)| byte)
}

fn draw_rich(
    ui: &mut Ui,
    rich: &Rich,
    rc: &mut RenderCtx<'_>,
    size: f32,
    extra: Style,
    heading: bool,
) -> Response {
    draw_rich_rows(ui, rich, rc, size, extra, heading).0
}

/// [`draw_rich`], also returning the number of rows the text took.
fn draw_rich_rows(
    ui: &mut Ui,
    rich: &Rich,
    rc: &mut RenderCtx<'_>,
    size: f32,
    extra: Style,
    heading: bool,
) -> (Response, usize) {
    let _ = heading;
    let job = rich_job(rich, rc, size, extra);
    let label = Label::new(WidgetText::LayoutJob(Arc::new(job))).selectable(true);
    let (pos, galley, response) = label.layout_in_ui(ui);
    if ui.is_rect_visible(response.rect) {
        LabelSelectionState::label_text_selection(
            ui,
            &response,
            pos,
            Arc::clone(&galley),
            ui.visuals().text_color(),
            Stroke::NONE,
        );
    }
    if !rich.links.is_empty() {
        let hovered = response
            .hover_pos()
            .and_then(|pointer| char_under(&galley, pointer - pos, &rich.text))
            .and_then(|byte| rich.link_at(byte));
        if let Some(link) = hovered {
            let target = rich.links[link as usize].clone();
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            rc.new_hover = Some((rich.id, link));
            if response.clicked() {
                rc.actions.push(ViewAction::OpenLink(target.clone()));
            }
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                Id::new("link-tooltip"),
                egui::PopupAnchor::Pointer,
            )
            .gap(12.0)
            .show(|ui| ui.label(RichText::new(target).size(12.0)));
        }
    }
    (response, galley.rows.len())
}

/// `file://` URI or http(s) URL for an image source written in a document.
pub fn image_uri(src: &str, base_dir: Option<&Path>) -> Option<String> {
    let src = src.trim();
    if src.starts_with("http://") || src.starts_with("https://") {
        return Some(src.to_owned());
    }
    if src.is_empty() || src.starts_with("data:") {
        return None;
    }
    let path_part = src.split(['?', '#']).next().unwrap_or(src);
    let path: PathBuf = if let Some(rest) = path_part.strip_prefix("file://") {
        let rest = percent_decode(rest);
        let rest = if cfg!(windows) {
            rest.trim_start_matches('/').to_owned()
        } else {
            rest
        };
        PathBuf::from(rest)
    } else {
        let decoded = percent_decode(path_part);
        let relative = Path::new(&decoded);
        if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            base_dir?.join(relative)
        }
    };
    Some(file_uri(&path))
}

/// A `file://` URI in the form egui_extras' file loader parses: on Windows
/// `file:///C:/dir/x.png` for drive paths and `file://host/share/x.png` for
/// UNC paths (two slashes alone would be read as a network host).
fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        match text.strip_prefix(r"\\") {
            Some(unc) => format!("file://{unc}"),
            None => format!("file:///{text}"),
        }
    } else {
        format!("file://{text}")
    }
}

fn draw_image(ui: &mut Ui, image: &ImageRef, rc: &mut RenderCtx<'_>) -> Response {
    let available = ui.available_width().max(16.0);
    let fallback = |ui: &mut Ui, rc: &mut RenderCtx<'_>| {
        let alt = if image.alt.is_empty() {
            "image"
        } else {
            image.alt.as_str()
        };
        let text = egui::RichText::new(format!("[{alt}]")).color(rc.palette.muted);
        let response = ui.add(Label::new(text).sense(Sense::click()));
        if let Some(link) = &image.link {
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if response.clicked() {
                rc.actions.push(ViewAction::OpenLink(link.clone()));
            }
        }
        response
    };
    let Some(uri) = image_uri(&image.src, rc.base_dir) else {
        return fallback(ui, rc);
    };
    let uri = super::animation::frame_uri(ui.ctx(), &uri).unwrap_or(uri);
    // Space before an image that follows text on the same row.
    let mut widget = egui::Image::new(uri)
        .show_loading_spinner(true)
        .alt_text(image.alt.clone());
    widget = match (image.width, image.height) {
        (Some(w), Some(h)) => {
            let w2 = w.min(available);
            widget.fit_to_exact_size(vec2(w2, h * w2 / w))
        }
        (Some(w), None) => widget
            .fit_to_exact_size(vec2(w.min(available), f32::INFINITY))
            .maintain_aspect_ratio(true),
        (None, Some(h)) => widget
            .fit_to_exact_size(vec2(available, h))
            .maintain_aspect_ratio(true),
        (None, None) => widget.fit_to_original_size(1.0).max_width(available),
    };
    if widget
        .load_for_size(ui.ctx(), vec2(available, f32::INFINITY))
        .is_err()
    {
        return fallback(ui, rc);
    }
    let sense = if image.link.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.add(widget.sense(sense));
    if let Some(link) = &image.link {
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if response.clicked() {
            rc.actions.push(ViewAction::OpenLink(link.clone()));
        }
    }
    response
}

fn code_job(code: &str, highlights: Option<&[HlSpan]>, rc: &RenderCtx<'_>, id: u32) -> LayoutJob {
    let size = BODY_SIZE * CODE_SCALE;
    let (matches, current) = rc.find.matches_in(id);
    let base = TextFormat {
        font_id: FontId::monospace(size),
        line_height: Some((size * 1.45).round()),
        color: rc.palette.text,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    match highlights {
        Some(spans) if !spans.is_empty() => {
            let mut covered = 0;
            for span in spans {
                if span.range.start > covered {
                    append_with_matches(
                        &mut job,
                        code,
                        covered..span.range.start,
                        &base,
                        matches,
                        current,
                        rc.palette,
                    );
                }
                let mut format = base.clone();
                format.color = Color32::from_rgb(span.rgb[0], span.rgb[1], span.rgb[2]);
                format.italics = span.italic;
                let end = span.range.end.min(code.len());
                append_with_matches(
                    &mut job,
                    code,
                    span.range.start..end,
                    &format,
                    matches,
                    current,
                    rc.palette,
                );
                covered = end;
            }
            if covered < code.len() {
                append_with_matches(
                    &mut job,
                    code,
                    covered..code.len(),
                    &base,
                    matches,
                    current,
                    rc.palette,
                );
            }
        }
        _ => append_with_matches(
            &mut job,
            code,
            0..code.len(),
            &base,
            matches,
            current,
            rc.palette,
        ),
    }
    if code.is_empty() {
        job.append(" ", 0.0, base);
    }
    job
}

fn draw_code(
    ui: &mut Ui,
    id: u32,
    key: Option<u64>,
    lang: &str,
    code: &str,
    rc: &mut RenderCtx<'_>,
    salt: u64,
) {
    let highlights = key.and_then(|key| rc.highlighter.get(key, lang, code, rc.palette.dark));
    let job = code_job(code, highlights.as_deref().map(Vec::as_slice), rc, id);
    let frame = Frame::new()
        .fill(rc.palette.code_block_bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::horizontal()
                .id_salt(("code-scroll", salt))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let label = Label::new(WidgetText::LayoutJob(Arc::new(job)))
                        .selectable(true)
                        .extend();
                    let (pos, galley, response) = label.layout_in_ui(ui);
                    if ui.is_rect_visible(response.rect) {
                        LabelSelectionState::label_text_selection(
                            ui,
                            &response,
                            pos,
                            galley,
                            rc.palette.text,
                            Stroke::NONE,
                        );
                    }
                });
        });
    let rect = frame.response.rect;
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect);
    if hovered {
        let button_rect =
            Rect::from_min_size(rect.right_top() + vec2(-64.0, 6.0), vec2(58.0, 22.0));
        let copied_id = Id::new(("copied", salt, id));
        let copied_at: Option<f64> = ui.data(|d| d.get_temp(copied_id));
        let now = ui.input(|i| i.time);
        let recently = copied_at.is_some_and(|t| now - t < 1.5);
        let text = if recently { "Copied" } else { "Copy" };
        let button = egui::Button::new(egui::RichText::new(text).size(12.0)).corner_radius(4);
        if ui.put(button_rect, button).clicked() {
            ui.ctx().copy_text(code.to_owned());
            ui.data_mut(|d| d.insert_temp(copied_id, now));
        }
        if recently {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
    } else if !lang.is_empty() {
        ui.painter().text(
            rect.right_top() + vec2(-10.0, 6.0),
            egui::Align2::RIGHT_TOP,
            lang,
            FontId::proportional(11.0),
            rc.palette.muted,
        );
    }
}

/// A cheap guess at a block's height before it is laid out.
fn estimate(block: &Block, width: f32) -> f32 {
    let line = BODY_SIZE * LINE_HEIGHT;
    let chars_per_line = (width / (BODY_SIZE * 0.5)).max(10.0);
    let text_height = |len: usize, newlines: usize, size: f32| {
        let scale = size / BODY_SIZE;
        let lines = ((len as f32 * scale) / chars_per_line).ceil().max(1.0) + newlines as f32;
        lines * size * LINE_HEIGHT
    };
    let inline_height = |inlines: &[Inline], size: f32| -> f32 {
        let mut len = 0;
        let mut newlines = 0;
        let mut images = 0.0f32;
        for inline in inlines {
            match inline {
                Inline::Text(rich) => {
                    len += rich.text.len();
                    newlines += rich.text.matches('\n').count();
                }
                Inline::Image(image) => images = images.max(image.height.unwrap_or(120.0)),
            }
        }
        text_height(len, newlines, size).max(images)
    };
    let gap = GAP;
    match block {
        Block::Heading { level, inlines, .. } => {
            inline_height(inlines, heading_size(*level))
                + BODY_SIZE * 0.5
                + if *level <= 2 { 6.0 } else { 0.0 }
                + gap
        }
        Block::Paragraph { inlines, .. } => inline_height(inlines, BODY_SIZE) + gap,
        Block::Code { code, .. } | Block::Math { tex: code, .. } => {
            (code.lines().count().max(1) as f32) * (BODY_SIZE * CODE_SCALE * 1.45).round()
                + 24.0
                + gap
        }
        Block::FrontMatter { .. } => line + gap,
        Block::Quote(blocks) | Block::Alert { blocks, .. } | Block::Footnote { blocks, .. } => {
            blocks
                .iter()
                .map(|b| estimate(b, width - 40.0))
                .sum::<f32>()
                + gap
        }
        Block::List { items, .. } => {
            items
                .iter()
                .map(|item| {
                    item.blocks
                        .iter()
                        .map(|b| estimate(b, width - 30.0) - gap * 0.6)
                        .sum::<f32>()
                        .max(line)
                })
                .sum::<f32>()
                + gap
        }
        Block::Table(table) => (table.rows.len() + 1) as f32 * (line + 8.0) + 16.0 + gap,
        Block::DefinitionList(items) => items.len() as f32 * line * 2.0 + gap,
        Block::Rule => BODY_SIZE + gap,
    }
}

/// The y position (in content coordinates) where each heading starts, for
/// the table of contents.
pub fn block_top(view: &ViewState, block: usize) -> Option<f32> {
    view.offsets.get(block).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_sources_become_loadable_uris() {
        assert_eq!(
            image_uri("https://img.shields.io/x.svg", None).as_deref(),
            Some("https://img.shields.io/x.svg")
        );
        assert_eq!(image_uri("data:image/png;base64,AAAA", None), None);
        assert_eq!(image_uri("pic.png", None), None, "relative needs a base");
        let base = if cfg!(windows) {
            Path::new(r"C:\docs")
        } else {
            Path::new("/docs")
        };
        let uri = image_uri("img/my%20pic.png?raw=1", Some(base)).unwrap();
        if cfg!(windows) {
            assert_eq!(uri, r"file:///C:\docs\img/my pic.png");
        } else {
            assert_eq!(uri, "file:///docs/img/my pic.png");
        }
    }

    #[cfg(windows)]
    #[test]
    fn unc_paths_keep_their_host() {
        assert_eq!(
            file_uri(Path::new(r"\\srv\share\a.png")),
            r"file://srv\share\a.png"
        );
    }
}
