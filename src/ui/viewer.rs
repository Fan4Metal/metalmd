//! Draws a `Document` with egui.
//!
//! Only the blocks in view are laid out each frame. Heights of the others are
//! estimated, then measured a few at a time in the background of later frames,
//! and the scroll position is corrected whenever a block above the view turns
//! out taller or shorter than estimated, so the text being read never jumps.

use crate::markdown::document::{
    AlertKind, Align, Block, Document, Fold, ImageRef, Inline, ListItem, Rich, Span, Style, Table,
    Task,
};
use crate::markdown::highlight::{Highlighter, HlSpan};
use crate::markdown::preprocess::percent_decode;
use crate::markdown::source::{self, Part, Selected};
use crate::style::{FontChoice, Palette, body_size, content_width};
use crate::ui::diagram::DiagramRenderer;
use crate::ui::emoji::EmojiRenderer;
use crate::ui::find::{FindState, Match};
use crate::ui::math::MathRenderer;
use crate::ui::render_cache::{Metrics, Picture, State};
use crate::ui::selection;
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Galley, Id, Label, Layout,
    Margin, Rect, Response, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetText,
    pos2, vec2,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Space between blocks.
fn block_gap() -> f32 {
    body_size() * 0.9
}
/// Formulas are this much larger than the text around them, as KaTeX's
/// (its fonts look small next to most text faces).
const MATH_SCALE: f32 = 1.21;
const LINE_HEIGHT: f32 = 1.5;
const CODE_SCALE: f32 = 0.875;
const SIDE_PADDING: f32 = 28.0;
const TOP_PADDING: f32 = 20.0;
const BOTTOM_PADDING: f32 = 120.0;
/// Time per frame spent measuring blocks that are not on screen.
const MEASURE_BUDGET: Duration = Duration::from_millis(4);
/// How long a jump is re-applied at most while heights above it change.
const SETTLE_TIME: Duration = Duration::from_secs(1);

/// Something the user asked for by clicking in the document.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewAction {
    OpenLink(String),
    /// Tick or clear a task list item in the file.
    ToggleTask(Task),
    /// Copy the selection as the Markdown it was written in.
    CopyMarkdown,
    /// Highlight the selection in the file (`==text==`).
    Highlight,
    /// Remove the highlights the selection touches, or with none the one
    /// the context menu was opened on.
    Unhighlight,
    /// Show an image or a diagram on its own, over the window.
    ShowPicture(super::lightbox::Shown),
}

/// The text a context menu was opened on: the place under the pointer, and
/// whether it is highlighted.
#[derive(Clone, Debug)]
pub struct MenuPoint {
    pub at: Selected,
    pub marked: bool,
}

/// Per-tab layout and scroll state.
#[derive(Default)]
pub struct ViewState {
    heights: Vec<f32>,
    measured: Vec<bool>,
    /// Unmeasured because a picture in it was still being made when it was
    /// measured: measured again when the worker that makes it repaints.
    waiting: Vec<bool>,
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
    /// When the jump being settled was made: settling ends after
    /// [`SETTLE_TIME`] too, as frames come only with input once the view is
    /// still.
    settle_since: Option<Instant>,
    /// Scroll by this many points next frame (keyboard scrolling).
    pub scroll_delta: f32,
    pub viewport_top: f32,
    pub viewport_height: f32,
    pub first_visible: usize,
    hover: Option<(u32, u32)>,
    scroll_id: Option<Id>,
    measure_cursor: usize,
    /// `<details>` (by `Block::Details::index`) the reader opened or
    /// closed, the other way from how they are written. Kept across
    /// reloads, so ticking a task inside one leaves it open.
    details: HashSet<u32>,
    /// Top-level blocks inside a closed `<details>`: drawn as nothing.
    /// Empty when the document has none.
    hidden: Vec<bool>,
    hidden_stale: bool,
    /// What of the document was selected when it was last drawn, in
    /// document order.
    selected: Vec<Selected>,
    /// Some of the selection is highlighted.
    selected_marks: bool,
    menu_point: Option<MenuPoint>,
}

impl ViewState {
    /// The `<details>` the reader opened or closed, the other way from how
    /// they are written (by `Block::Details::index`).
    pub fn toggled_details(&self) -> &HashSet<u32> {
        &self.details
    }

    /// The width of the text column, in points, as last laid out.
    pub fn column_width(&self) -> Option<f32> {
        (self.measured_width > 0.0).then_some(self.measured_width)
    }

    /// What of the document is selected, in document order.
    pub fn selection(&self) -> &[Selected] {
        &self.selected
    }

    /// The text the last context menu was opened on.
    pub fn menu_point(&self) -> Option<&MenuPoint> {
        self.menu_point.as_ref()
    }

    /// Scroll position to keep across a reload: first visible block and the
    /// offset into it.
    pub fn reading_position(&self) -> Option<(usize, f32)> {
        let top = *self.offsets.get(self.first_visible)?;
        Some((self.first_visible, top - self.viewport_top))
    }

    /// Where to come back to: the jump not made yet (a tab never shown since
    /// its document loaded), else the reading position.
    pub fn saved_position(&self) -> Option<(usize, f32)> {
        self.jump.or_else(|| self.reading_position())
    }

    pub fn jump_to(&mut self, block: usize, offset: f32) {
        self.jump = Some((block, offset));
    }

    fn prepare(&mut self, doc: &Document, width: f32, fonts: u64) {
        let identity = doc as *const Document as usize;
        if identity != self.doc_identity || self.heights.len() != doc.blocks.len() {
            self.hidden_stale = true;
            self.doc_identity = identity;
            self.heights = doc.blocks.iter().map(|b| estimate(b, width)).collect();
            self.measured = vec![false; doc.blocks.len()];
            self.waiting = vec![false; doc.blocks.len()];
            self.measured_width = width;
            self.measured_fonts = fonts;
            self.anchor = None;
            self.measure_cursor = 0;
        } else if (self.measured_width - width).abs() > 0.5 || self.measured_fonts != fonts {
            // Keep the old heights as estimates; they are closer than new ones.
            self.measured.fill(false);
            self.waiting.fill(false);
            self.measured_width = width;
            self.measured_fonts = fonts;
        }
        if self.hidden_stale {
            self.hidden_stale = false;
            self.update_hidden(doc, width);
        }
        // A jump into a closed `<details>` (find, the contents, a link)
        // opens it.
        if let Some((block, _)) = self.jump
            && self.is_hidden(block)
        {
            self.reveal(doc, block);
            self.hidden_stale = false;
            self.update_hidden(doc, width);
        }
    }

    fn is_hidden(&self, block: usize) -> bool {
        self.hidden.get(block).copied().unwrap_or(false)
    }

    /// Takes the closed `<details>` into account: blocks they hide get no
    /// height, blocks no longer hidden an estimate until measured.
    fn update_hidden(&mut self, doc: &Document, width: f32) {
        let hidden = hidden_blocks(&doc.blocks, &self.details).unwrap_or_default();
        if hidden.is_empty() && self.hidden.is_empty() {
            return;
        }
        for (i, block) in doc.blocks.iter().enumerate() {
            if hidden.get(i).copied().unwrap_or(false) {
                self.heights[i] = 0.0;
                self.measured[i] = true;
                self.waiting[i] = false;
            } else if self.is_hidden(i) {
                self.heights[i] = estimate(block, width);
                self.measured[i] = false;
            }
        }
        self.hidden = hidden;
    }

    /// Opens the closed `<details>` around top-level block `block`.
    fn reveal(&mut self, doc: &Document, block: usize) {
        let mut depth = 0usize;
        for previous in doc.blocks[..block.min(doc.blocks.len())].iter().rev() {
            match previous {
                Block::DetailsEnd => depth += 1,
                Block::Details { index, open, .. } if depth == 0 => {
                    if !details_open(*index, *open, &self.details) {
                        self.toggle_details(*index);
                    }
                }
                Block::Details { .. } => depth -= 1,
                _ => {}
            }
        }
        // A folded callout is unfolded to show what is in it.
        if let Some(Block::Alert { fold: Some(f), .. }) = doc.blocks.get(block)
            && !details_open(f.index, f.open, &self.details)
        {
            self.toggle_details(f.index);
        }
    }

    fn toggle_details(&mut self, index: u32) {
        if !self.details.remove(&index) {
            self.details.insert(index);
        }
        self.hidden_stale = true;
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

/// The renderers of the pictures made from document text.
pub struct Pictures {
    pub math: MathRenderer,
    pub emoji: EmojiRenderer,
    pub diagrams: DiagramRenderer,
}

/// Everything a frame of rendering needs besides the document.
pub struct RenderCtx<'a> {
    pub palette: &'a Palette,
    pub fonts: &'a FontChoice,
    /// Changes whenever the font set changes (CJK fallback loaded).
    pub fonts_generation: u64,
    pub highlighter: &'a Highlighter,
    pub pictures: &'a Pictures,
    pub base_dir: Option<&'a Path>,
    pub find: &'a FindState,
    pub actions: Vec<ViewAction>,
    hover: Option<(u32, u32)>,
    new_hover: Option<(u32, u32)>,
    /// A formula or diagram drawn since this was last cleared is still being
    /// made, so the block's height is not final.
    pending: bool,
    /// `ViewState::details` while the document is drawn.
    details: HashSet<u32>,
    /// The `<details>` whose summary was clicked.
    toggled: Option<u32>,
    /// What is selected in the text drawn so far.
    selected: Vec<Selected>,
    selected_marks: bool,
    /// From the last frame: text is selected, and some of it highlighted.
    had_selection: bool,
    had_marks: bool,
    /// `ViewState::menu_point` while the document is drawn.
    menu_point: Option<MenuPoint>,
}

impl<'a> RenderCtx<'a> {
    pub fn new(
        palette: &'a Palette,
        fonts: &'a FontChoice,
        fonts_generation: u64,
        highlighter: &'a Highlighter,
        pictures: &'a Pictures,
        base_dir: Option<&'a Path>,
        find: &'a FindState,
    ) -> Self {
        RenderCtx {
            palette,
            fonts,
            fonts_generation,
            highlighter,
            pictures,
            base_dir,
            find,
            actions: Vec::new(),
            hover: None,
            new_hover: None,
            pending: false,
            details: HashSet::new(),
            toggled: None,
            selected: Vec::new(),
            selected_marks: false,
            had_selection: false,
            had_marks: false,
            menu_point: None,
        }
    }

    /// Notes that `range` (bytes) of `text`, the text of `part`, is selected.
    fn select(&mut self, part: Part, text: &str, range: std::ops::Range<usize>, marked: bool) {
        self.selected_marks |= marked;
        self.selected.push(Selected {
            part,
            range,
            digest: source::digest(text),
        });
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
        .min(content_width())
        .max(120.0)
        .floor();
    view.prepare(doc, content_width, rc.fonts_generation);
    rc.hover = view.hover;
    rc.new_hover = None;
    rc.details = std::mem::take(&mut view.details);
    rc.had_selection = !view.selected.is_empty();
    rc.had_marks = view.selected_marks;
    rc.menu_point = view.menu_point.take();

    let offsets = view.compute_offsets();
    let mut area = ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false]);

    // Any scrolling by the reader ends the settling of a jump: the wheel,
    // the keys, or a drag (of the scroll bar); so does time.
    let reader_scrolls = view.scroll_delta != 0.0
        || ui.input(|i| i.smooth_scroll_delta.y != 0.0 || i.pointer.is_decidedly_dragging());
    let settled = view.settle_since.is_some_and(|t| t.elapsed() > SETTLE_TIME);
    if view.settle_target.is_some() && (reader_scrolls || settled) {
        view.settle_target = None;
        view.settle = 0;
        view.settle_since = None;
    }
    if let Some((block, delta)) = view.jump.take() {
        view.settle = 8;
        view.settle_target = Some((block, delta));
        view.settle_since = Some(Instant::now());
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
            if view.is_hidden(index) {
                index += 1;
                continue;
            }
            let rect = Rect::from_min_size(pos2(left, origin.y + y), vec2(content_width, 1.0e5));
            let height = draw_top_block(ui, doc, index, rect, rc, salt, false);
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
            if view.is_hidden(cursor) {
                view.heights[cursor] = 0.0;
                view.measured[cursor] = true;
            }
            if !view.measured[cursor] {
                let rect = Rect::from_min_size(
                    pos2(left, origin.y + offsets[cursor]),
                    vec2(content_width, 1.0e5),
                );
                rc.pending = false;
                view.heights[cursor] = draw_top_block(ui, doc, cursor, rect, rc, salt, true);
                // Measured again in a later frame, once its pictures are made.
                view.measured[cursor] = !rc.pending;
                view.waiting[cursor] = rc.pending;
                remaining -= 1;
            }
            cursor = (cursor + 1) % count;
        }
        view.measure_cursor = cursor;
        // Blocks that only wait for pictures need no frame until a worker
        // repaints; asking for one would spin at full frame rate meanwhile.
        let more = view
            .measured
            .iter()
            .zip(&view.waiting)
            .any(|(measured, waiting)| !measured && !waiting);
        if more {
            ui.ctx().request_repaint();
        }

        view.anchor = Some((first, offsets[first]));
        view.first_visible = first;
        view.viewport_top = viewport.min.y;
        view.viewport_height = viewport.height();
    });
    view.scroll_id = Some(output.id);
    paint_find_marks(ui, output.inner_rect, output.content_size.y, &offsets, rc);
    view.offsets = offsets;
    view.details = std::mem::take(&mut rc.details);
    view.selected = std::mem::take(&mut rc.selected);
    view.selected_marks = std::mem::take(&mut rc.selected_marks);
    view.menu_point = rc.menu_point.take();
    if let Some(index) = rc.toggled.take() {
        view.toggle_details(index);
        ui.ctx().request_repaint();
    }
    if view.hover != rc.new_hover {
        view.hover = rc.new_hover;
        ui.ctx().request_repaint();
    }
}

/// Marks where the find matches are along the right edge of the view, as
/// the scroll bar maps the document onto it: one per block, the current
/// match's in its own colour.
fn paint_find_marks(ui: &Ui, view: Rect, content_height: f32, offsets: &[f32], rc: &RenderCtx<'_>) {
    let find = rc.find;
    let last = offsets.len().saturating_sub(1);
    if !find.open || find.matches.is_empty() || content_height <= view.height() || last == 0 {
        return;
    }
    let y_of =
        |block: usize| view.top() + offsets[block.min(last)] / content_height * view.height();
    let mark = |y: f32, color| {
        let rect = Rect::from_min_max(pos2(view.right() - 8.0, y), pos2(view.right(), y + 3.0));
        ui.painter().rect_filled(rect, 1.0, color);
    };
    let mut drawn = f32::NEG_INFINITY;
    for m in &find.matches {
        let y = y_of(m.block);
        if y - drawn >= 2.0 {
            mark(y, rc.palette.search_bg);
            drawn = y;
        }
    }
    if let Some(current) = find.current_match() {
        mark(y_of(current.block), rc.palette.search_current_bg);
    }
}

/// Draws (or, when `invisible`, only measures) top-level block `index` and
/// returns its height including the gap after it. `salt` is the document's:
/// what egui keeps for a block (a code block's sideways scroll, a table's
/// row heights) is then the tab's own, not shared with the block of the
/// same number in another tab.
fn draw_top_block(
    ui: &mut Ui,
    doc: &Document,
    index: usize,
    rect: Rect,
    rc: &mut RenderCtx<'_>,
    salt: Id,
    invisible: bool,
) -> f32 {
    let mut builder = UiBuilder::new()
        .max_rect(rect)
        .id_salt((salt, "block", index))
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
            let (line, _) = ui.allocate_exact_size(vec2(rect.width(), block_gap()), Sense::hover());
            ui.painter().hline(
                line.x_range(),
                line.center().y,
                Stroke::new(1.0, rc.palette.border),
            );
        }
        draw_block(ui, block, rc, index == 0, index as u64);
    });
    let gap = if index + 1 < doc.blocks.len() {
        block_gap()
    } else {
        0.0
    };
    response.response.rect.height() + gap
}

fn draw_blocks(ui: &mut Ui, blocks: &[Block], rc: &mut RenderCtx<'_>, gap: f32, salt: u64) {
    let hidden = hidden_blocks(blocks, &rc.details);
    let mut first = true;
    for (i, block) in blocks.iter().enumerate() {
        if hidden.as_ref().is_some_and(|hidden| hidden[i]) {
            continue;
        }
        if !first {
            ui.add_space(gap);
        }
        draw_block(
            ui,
            block,
            rc,
            first,
            salt.wrapping_mul(31).wrapping_add(i as u64 + 1),
        );
        first = false;
    }
}

/// Whether `<details>` number `index` is open: as written, unless the
/// reader toggled it.
fn details_open(index: u32, open: bool, toggled: &HashSet<u32>) -> bool {
    open != toggled.contains(&index)
}

/// Which of `blocks` draw as nothing: those inside a closed `<details>`, and
/// the `DetailsEnd` markers. `None` if there is no `<details>` among them.
fn hidden_blocks(blocks: &[Block], toggled: &HashSet<u32>) -> Option<Vec<bool>> {
    if !blocks
        .iter()
        .any(|b| matches!(b, Block::Details { .. } | Block::DetailsEnd))
    {
        return None;
    }
    // Whether each `<details>` around the block is open, and how many of
    // them are closed.
    let mut stack: Vec<bool> = Vec::new();
    let mut closed = 0;
    let hidden = blocks
        .iter()
        .map(|block| match block {
            Block::Details { index, open, .. } => {
                let hidden = closed > 0;
                let open = details_open(*index, *open, toggled);
                stack.push(open);
                closed += usize::from(!open);
                hidden
            }
            Block::DetailsEnd => {
                if stack.pop() == Some(false) {
                    closed -= 1;
                }
                true
            }
            _ => closed > 0,
        })
        .collect();
    Some(hidden)
}

/// The `<summary>` row: a triangle and the summary, which opens or closes
/// its `<details>` on a click anywhere in the row but on a link in it.
fn draw_summary(
    ui: &mut Ui,
    index: u32,
    open: bool,
    summary: &[Inline],
    rc: &mut RenderCtx<'_>,
    salt: u64,
) {
    let actions = rc.actions.len();
    let hover = rc.new_hover;
    let row = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let line = body_size() * LINE_HEIGHT;
        let (marker, _) = ui.allocate_exact_size(vec2(body_size() * 1.25, line), Sense::hover());
        // Centred a third of an em above the baseline, as list bullets are.
        let baseline = marker.top() + rc.fonts.body_ascent * body_size();
        let c = pos2(
            marker.left() + body_size() * 0.4,
            baseline - body_size() * 0.33,
        );
        let r = body_size() * 0.28;
        let points = if open {
            vec![
                c + vec2(-r, -0.5 * r),
                c + vec2(r, -0.5 * r),
                c + vec2(0.0, r),
            ]
        } else {
            vec![
                c + vec2(-0.5 * r, -r),
                c + vec2(r, 0.0),
                c + vec2(-0.5 * r, r),
            ]
        };
        let color = ui.visuals().text_color();
        ui.painter()
            .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        ui.vertical(|ui| {
            if summary.is_empty() {
                ui.label(RichText::new(tr!("Details", "Подробности")).size(body_size()));
            } else {
                draw_inlines(
                    ui,
                    summary,
                    rc,
                    body_size(),
                    Style::NONE,
                    Align::Left,
                    salt,
                    false,
                );
            }
        });
    });
    let rect = Rect::from_min_max(
        row.response.rect.min,
        pos2(ui.max_rect().right(), row.response.rect.max.y),
    );
    if ui.is_visible() && ui.rect_contains_pointer(rect) && rc.new_hover == hover {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        if rc.actions.len() == actions && ui.input(|i| i.pointer.primary_clicked()) {
            rc.toggled = Some(index);
        }
    }
}

fn heading_size(level: u8) -> f32 {
    body_size()
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
                ui.add_space(body_size() * 0.5);
            }
            let size = heading_size(*level);
            draw_inlines(ui, inlines, rc, size, Style::STRONG, *align, salt, true);
            if *level <= 2 {
                ui.add_space(body_size() * 0.3);
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
            draw_inlines(
                ui,
                inlines,
                rc,
                body_size(),
                Style::NONE,
                *align,
                salt,
                false,
            );
        }
        Block::Code {
            id,
            key,
            lang,
            code,
        } => {
            if lang.eq_ignore_ascii_case("mermaid") {
                draw_diagram(ui, *id, code, rc, salt);
            } else {
                draw_code(ui, *id, Some(*key), lang, code, rc, salt);
            }
        }
        Block::Math { id, tex } => draw_display_math(ui, *id, tex, rc, salt),
        Block::Details {
            index,
            open,
            summary,
        } => {
            let open = details_open(*index, *open, &rc.details);
            draw_summary(ui, *index, open, summary, rc, salt);
        }
        Block::DetailsEnd => {}
        Block::FrontMatter {
            properties: Some(properties),
            ..
        } if !properties.is_empty() => {
            egui::CollapsingHeader::new(
                egui::RichText::new(tr!("Properties", "Свойства")).color(rc.palette.muted),
            )
            .id_salt(("properties", salt))
            .default_open(true)
            .show(ui, |ui| draw_properties(ui, properties, rc));
        }
        Block::FrontMatter { id, yaml, .. } => {
            egui::CollapsingHeader::new(
                egui::RichText::new(tr!("Front matter", "Метаданные")).color(rc.palette.muted),
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
                draw_blocks(ui, blocks, rc, block_gap(), salt);
            });
        }
        Block::Embed {
            title,
            src,
            content,
        } => draw_embed(ui, title, src, content.as_ref(), rc, salt),
        Block::Gallery { masonry, images } => draw_gallery(ui, *masonry, images, rc),
        Block::Alert {
            kind,
            title,
            fold,
            blocks,
        } => {
            let color = rc.palette.alert(*kind);
            let open = fold.is_none_or(|f| details_open(f.index, f.open, &rc.details));
            draw_barred(ui, color, |ui| {
                let actions = rc.actions.len();
                let hover = rc.new_hover;
                let title = egui::RichText::new(title)
                    .color(color)
                    .size(body_size())
                    .family(rc.fonts.bold.clone());
                let (pos, galley, response) = Label::new(title).selectable(true).layout_in_ui(ui);
                if ui.is_rect_visible(response.rect) {
                    selection::label(ui, &response, pos, galley, color, Stroke::NONE);
                }
                text_menu(&response, rc, false);
                if let Some(fold) = fold {
                    // A chevron after the title, as Obsidian draws it; a
                    // click on the row folds or unfolds the callout.
                    let r = body_size() * 0.25;
                    let c = pos2(
                        response.rect.right() + body_size() * 0.6,
                        response.rect.center().y,
                    );
                    let points = if open {
                        vec![
                            c + vec2(-r, -0.5 * r),
                            c + vec2(r, -0.5 * r),
                            c + vec2(0.0, r),
                        ]
                    } else {
                        vec![
                            c + vec2(-0.5 * r, -r),
                            c + vec2(r, 0.0),
                            c + vec2(-0.5 * r, r),
                        ]
                    };
                    ui.painter()
                        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
                    let row = Rect::from_min_max(
                        response.rect.min,
                        pos2(ui.max_rect().right(), response.rect.max.y),
                    );
                    if ui.is_visible() && ui.rect_contains_pointer(row) && rc.new_hover == hover {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        if rc.actions.len() == actions && ui.input(|i| i.pointer.primary_clicked())
                        {
                            rc.toggled = Some(fold.index);
                        }
                    }
                }
                if open {
                    ui.add_space(4.0);
                    draw_blocks(ui, blocks, rc, block_gap(), salt);
                }
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
                    ui.add_space(block_gap() * 0.5);
                }
                draw_blocks(ui, terms, rc, 2.0, salt.wrapping_add(i as u64 * 7919));
                for (j, detail) in details.iter().enumerate() {
                    ui.horizontal_top(|ui| {
                        ui.add_space(body_size() * 1.5);
                        ui.vertical(|ui| {
                            draw_blocks(
                                ui,
                                detail,
                                rc,
                                block_gap() * 0.5,
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
                let size = body_size() * 0.875;
                let (marker, _) = ui.allocate_exact_size(
                    vec2(body_size() * 2.0, size * LINE_HEIGHT),
                    Sense::hover(),
                );
                ui.painter().text(
                    pos2(marker.right() - 6.0, marker.center().y),
                    egui::Align2::RIGHT_CENTER,
                    format!("{label}."),
                    FontId::proportional(size),
                    muted,
                );
                ui.vertical(|ui| {
                    ui.visuals_mut().override_text_color = Some(muted);
                    draw_blocks(ui, blocks, rc, block_gap() * 0.5, salt);
                });
            });
        }
        Block::Rule => {
            let width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(width, body_size()), Sense::hover());
            ui.painter().rect_filled(
                Rect::from_center_size(rect.center(), vec2(width, 3.0)),
                CornerRadius::same(1),
                rc.palette.border,
            );
        }
    }
}

/// A block with a colored bar on its left (quotes, alerts).
/// Quotes and lists indented at most: deeper ones are drawn at the indent
/// of the last, so the text keeps room (32 levels would be wider than the
/// page, the parser's limit).
const MAX_INDENTS: u32 = 8;

/// Quotes and lists open around what is being drawn.
fn indents(ui: &Ui) -> u32 {
    ui.data(|d| d.get_temp(Id::new("indents"))).unwrap_or(0)
}

/// Draws `add` one quote or list deeper.
fn indented<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let depth = indents(ui);
    ui.data_mut(|d| d.insert_temp(Id::new("indents"), depth + 1));
    let result = add(ui);
    ui.data_mut(|d| d.insert_temp(Id::new("indents"), depth));
    result
}

/// The Media Gallery plugin's defaults: the gap between pictures, the
/// height a row aims at (at the default text size) and the columns of a
/// masonry gallery.
const GALLERY_GAP: f32 = 5.0;
const GALLERY_ROW: f32 = 300.0;
const GALLERY_COLUMNS: usize = 3;

/// Justified rows, as the plugin lays out `[!gallery]`: pictures of the
/// given aspects (width / height) side by side at one height, each row as
/// wide as `width`, aiming at `target`; the last row, if short, keeps the
/// target height. Rectangles from the top left.
fn justified_rows(aspects: &[f32], width: f32, target: f32, gap: f32) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(aspects.len());
    let mut y = 0.0;
    let mut start = 0;
    while start < aspects.len() {
        let mut end = start;
        let mut sum = 0.0;
        let mut full = false;
        while end < aspects.len() {
            sum += aspects[end];
            end += 1;
            if sum * target + gap * (end - start - 1) as f32 >= width {
                full = true;
                break;
            }
        }
        let gaps = gap * (end - start - 1) as f32;
        let height = if full { (width - gaps) / sum } else { target };
        let mut x = 0.0;
        for aspect in &aspects[start..end] {
            let w = aspect * height;
            rects.push(Rect::from_min_size(pos2(x, y), vec2(w, height)));
            x += w + gap;
        }
        y += height + gap;
        start = end;
    }
    rects
}

/// Columns, as the plugin lays out `[!gallery-masonry]`: each picture at
/// the column's width, under the shortest column so far.
fn masonry_columns(aspects: &[f32], width: f32, columns: usize, gap: f32) -> Vec<Rect> {
    let columns = columns.clamp(1, aspects.len().max(1));
    let column = (width - gap * (columns - 1) as f32) / columns as f32;
    let mut bottoms = vec![0.0f32; columns];
    aspects
        .iter()
        .map(|aspect| {
            let (index, top) = bottoms
                .iter()
                .copied()
                .enumerate()
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((0, 0.0));
            let height = column / aspect;
            bottoms[index] = top + height + gap;
            Rect::from_min_size(
                pos2(index as f32 * (column + gap), top),
                vec2(column, height),
            )
        })
        .collect()
}

/// A gallery callout: its pictures in justified rows or masonry columns,
/// each shown on its own with a click. A picture not loaded yet takes 4:3.
fn draw_gallery(ui: &mut Ui, masonry: bool, images: &[ImageRef], rc: &mut RenderCtx<'_>) {
    let width = ui.available_width();
    let items: Vec<(Option<String>, f32)> = images
        .iter()
        .map(|image| {
            let (uri, waiting) = image_source(ui.ctx(), &image.src, rc.base_dir);
            rc.pending |= waiting;
            let aspect = uri
                .as_ref()
                .and_then(|uri| {
                    let shown = super::animation::frame_uri(ui.ctx(), uri).unwrap_or(uri.clone());
                    match egui::Image::new(shown).load_for_size(ui.ctx(), Vec2::INFINITY) {
                        Ok(egui::load::TexturePoll::Ready { texture }) => Some(texture.size),
                        Ok(egui::load::TexturePoll::Pending { size }) => size,
                        Err(_) => None,
                    }
                })
                .filter(|size| size.x > 0.0 && size.y > 0.0)
                .map_or(4.0 / 3.0, |size| size.x / size.y);
            (uri, aspect)
        })
        .collect();
    let aspects: Vec<f32> = items.iter().map(|(_, a)| *a).collect();
    let rects = if masonry {
        masonry_columns(&aspects, width, GALLERY_COLUMNS, GALLERY_GAP)
    } else {
        justified_rows(
            &aspects,
            width,
            GALLERY_ROW * body_size() / 16.0,
            GALLERY_GAP,
        )
    };
    let height = rects.iter().map(|r| r.bottom()).fold(0.0, f32::max);
    let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    for (((uri, _), rect), image) in items.iter().zip(&rects).zip(images) {
        let rect = rect.translate(area.min.to_vec2());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let Some(uri) = uri else {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), rc.palette.code_bg);
            continue;
        };
        let shown = super::animation::frame_uri(ui.ctx(), uri).unwrap_or(uri.clone());
        let widget = egui::Image::new(shown)
            .corner_radius(6)
            .show_loading_spinner(true)
            .alt_text(image.alt.clone())
            .fit_to_exact_size(rect.size())
            .sense(Sense::click());
        let response = ui.put(rect, widget);
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ZoomIn);
        }
        if response.clicked() {
            rc.actions
                .push(ViewAction::ShowPicture(super::lightbox::Shown::Image(
                    uri.clone(),
                )));
        }
    }
}

/// Text of the document that is not a `Rich`: laid out, selectable with
/// the rest (`selection::label`), and its response.
fn doc_label(ui: &mut Ui, text: egui::RichText, color: Color32) -> Response {
    let (pos, galley, response) = Label::new(text).selectable(true).layout_in_ui(ui);
    if ui.is_rect_visible(response.rect) {
        selection::label(ui, &response, pos, galley, color, Stroke::NONE);
    }
    response
}

/// Obsidian's Properties: a row per key, the key muted on the left, the
/// values on the right; a list's values as chips. A value that is a
/// wikilink (`"[[Note]]"`) or a web address is a link.
fn draw_properties(
    ui: &mut Ui,
    properties: &[crate::markdown::properties::Property],
    rc: &mut RenderCtx<'_>,
) {
    let size = body_size() * 0.9;
    let font = egui::FontId::proportional(size);
    let key_width = properties
        .iter()
        .map(|p| {
            ui.fonts_mut(|f| f.layout_no_wrap(p.key.clone(), font.clone(), Color32::PLACEHOLDER))
                .size()
                .x
        })
        .fold(0.0, f32::max)
        .min(ui.available_width() * 0.35)
        .ceil();
    // Whole points throughout: egui lays text out on whole pixels.
    let gap = body_size().round();
    for property in properties {
        ui.horizontal_top(|ui| {
            let (key, _) = ui.allocate_exact_size(vec2(key_width + gap, 0.0), Sense::hover());
            let mut key_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(Rect::from_min_size(key.min, vec2(key_width, 1e4))),
            );
            let muted = rc.palette.muted;
            doc_label(
                &mut key_ui,
                egui::RichText::new(&property.key).size(size).color(muted),
                muted,
            );
            let key_height = key_ui.min_rect().height();
            ui.vertical(|ui| {
                ui.set_min_height(key_height);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing =
                        vec2((body_size() * 0.3).round(), (body_size() * 0.25).round());
                    for value in &property.values {
                        draw_property_value(ui, value, property.list, size, rc);
                    }
                });
            });
        });
        ui.add_space((body_size() * 0.2).round());
    }
}

fn draw_property_value(ui: &mut Ui, value: &str, chip: bool, size: f32, rc: &mut RenderCtx<'_>) {
    // `[[Note]]`, `[[Note|alias]]`, or a web address.
    let wikilink = value
        .strip_prefix("[[")
        .and_then(|v| v.strip_suffix("]]"))
        .filter(|v| !v.contains(['[', ']']));
    let (text, link) = match wikilink {
        Some(inner) => {
            let (target, alias) = inner.split_once('|').unwrap_or((inner, inner));
            let (file, part) = target.split_once('#').unwrap_or((target, ""));
            let mut link = crate::markdown::preprocess::note_destination(file.trim());
            if !part.trim().is_empty() {
                link = format!(
                    "{link}#{}",
                    crate::markdown::preprocess::heading_anchor_id(part)
                );
            }
            (alias.trim().to_owned(), Some(link))
        }
        None if value.starts_with("http://") || value.starts_with("https://") => {
            (value.to_owned(), Some(value.to_owned()))
        }
        None => (value.to_owned(), None),
    };
    let color = if link.is_some() {
        rc.palette.link
    } else {
        ui.visuals().text_color()
    };
    let text = egui::RichText::new(text).size(size).color(color);
    let response = if chip {
        // Laid out first, then the chip painted under the text.
        let (pos, galley, response) = Label::new(text).selectable(true).layout_in_ui(ui);
        if ui.is_rect_visible(response.rect) {
            ui.painter().rect_filled(
                response.rect.expand2(vec2(body_size() * 0.25, 1.0)),
                CornerRadius::same(255),
                rc.palette.code_bg,
            );
            selection::label(ui, &response, pos, galley, color, Stroke::NONE);
        }
        ui.add_space((body_size() * 0.3).round());
        response
    } else {
        doc_label(ui, text, color)
    };
    if let Some(link) = link {
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            rc.actions.push(ViewAction::OpenLink(link));
        }
    }
}

/// An embedded note: its name over its blocks, beside a bar in the accent
/// colour, as Obsidian frames one. A click on the name opens the note.
fn draw_embed(
    ui: &mut Ui,
    title: &str,
    src: &str,
    content: Option<&Result<Vec<Block>, String>>,
    rc: &mut RenderCtx<'_>,
    salt: u64,
) {
    let bar = rc.palette.accent.gamma_multiply(0.6);
    let muted = rc.palette.muted;
    let link = rc.palette.link;
    draw_barred(ui, bar, |ui| {
        let name = egui::RichText::new(title)
            .color(link)
            .size(body_size() * 0.85);
        let (pos, galley, response) = Label::new(name).selectable(true).layout_in_ui(ui);
        if ui.is_rect_visible(response.rect) {
            selection::label(ui, &response, pos, galley, link, Stroke::NONE);
        }
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if response.clicked() {
            rc.actions.push(ViewAction::OpenLink(src.to_owned()));
        }
        text_menu(&response, rc, false);
        ui.add_space(4.0);
        match content {
            Some(Ok(blocks)) => draw_blocks(ui, blocks, rc, block_gap(), salt),
            Some(Err(problem)) => {
                let text = egui::RichText::new(problem).color(muted).italics();
                let (pos, galley, response) = Label::new(text).selectable(true).layout_in_ui(ui);
                if ui.is_rect_visible(response.rect) {
                    selection::label(ui, &response, pos, galley, muted, Stroke::NONE);
                }
            }
            None => {}
        }
    });
}

fn draw_barred(ui: &mut Ui, color: Color32, add: impl FnOnce(&mut Ui)) {
    if indents(ui) >= MAX_INDENTS {
        ui.vertical(|ui| indented(ui, add));
        return;
    }
    let response = ui.horizontal_top(|ui| {
        ui.add_space(4.0 + body_size() * 0.9);
        ui.vertical(|ui| indented(ui, add));
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
        body_size() * (0.75 + 0.55 * digits)
    } else {
        body_size() * 1.5
    };
    let line = body_size() * LINE_HEIGHT;
    // Too deep to indent again: the marker goes where the last level's
    // markers are, left of the text, which they leave empty below their
    // first line.
    let flat = indents(ui) >= MAX_INDENTS;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            ui.add_space(if tight { 2.0 } else { block_gap() * 0.6 });
        }
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let marker = if flat {
                Rect::from_min_size(
                    ui.cursor().min - vec2(marker_width, 0.0),
                    vec2(marker_width, line),
                )
            } else {
                ui.allocate_exact_size(vec2(marker_width, line), Sense::hover())
                    .0
            };
            let painter = ui.painter();
            let text_color = ui.visuals().text_color();
            // The first line's baseline is one ascent below the row's top
            // (see `text_format`). Bullets and boxes are centred a third of
            // an em above it, which is the middle of the row in Segoe UI,
            // so they follow the text in a font of other proportions.
            let baseline = marker.top() + rc.fonts.body_ascent * body_size();
            let center_y = baseline - body_size() * 0.33;
            match item.task {
                Some(task) => {
                    let side = body_size() * 0.85;
                    let rect = Rect::from_center_size(
                        pos2(marker.right() - side / 2.0 - 6.0, center_y),
                        vec2(side, side),
                    );
                    // Line 0: an embedded note's, which cannot be ticked
                    // from here (`markdown::embed`); a status other than
                    // ` ` and `x` is only shown.
                    let editable = task.line > 0 && matches!(task.mark, ' ' | 'x' | 'X');
                    let response = ui.interact(
                        rect.expand(3.0),
                        ui.id().with(("task", task.line, task.column)),
                        if editable {
                            Sense::click()
                        } else {
                            Sense::hover()
                        },
                    );
                    let response = if editable {
                        response.on_hover_cursor(egui::CursorIcon::PointingHand)
                    } else {
                        response
                    };
                    if response.clicked() {
                        rc.actions.push(ViewAction::ToggleTask(task));
                    }
                    let accent = rc.palette.accent;
                    let muted = rc.palette.muted;
                    let outline = |color| {
                        painter.rect_stroke(
                            rect,
                            CornerRadius::same(3),
                            Stroke::new(1.5, color),
                            egui::StrokeKind::Inside,
                        )
                    };
                    if !task.checked && task.mark != ' ' {
                        match task.mark {
                            // Cancelled: a dash across a muted box.
                            '-' => {
                                outline(muted);
                                painter.hline(
                                    rect.x_range().shrink(side * 0.25),
                                    rect.center().y,
                                    Stroke::new(1.5, muted),
                                );
                            }
                            // In progress: half filled.
                            '/' => {
                                outline(accent);
                                let half =
                                    Rect::from_min_max(rect.min, pos2(rect.center().x, rect.max.y));
                                painter.rect_filled(
                                    half,
                                    CornerRadius {
                                        nw: 3,
                                        sw: 3,
                                        ne: 0,
                                        se: 0,
                                    },
                                    accent,
                                );
                            }
                            // Any other status: its mark in the box.
                            mark => {
                                outline(accent);
                                painter.text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    mark,
                                    egui::FontId::proportional(side * 0.8),
                                    accent,
                                );
                            }
                        }
                    } else if task.checked {
                        painter.rect_filled(rect, CornerRadius::same(3), rc.palette.accent);
                        let a = rect.left_center() + vec2(side * 0.22, 0.0);
                        let b = rect.center_bottom() + vec2(-side * 0.08, -side * 0.25);
                        let c = rect.right_top() + vec2(-side * 0.2, side * 0.25);
                        let stroke = Stroke::new(2.0, Color32::WHITE);
                        painter.line_segment([a, b], stroke);
                        painter.line_segment([b, c], stroke);
                    } else {
                        let color = if response.hovered() {
                            rc.palette.accent
                        } else {
                            rc.palette.muted
                        };
                        painter.rect_stroke(
                            rect,
                            CornerRadius::same(3),
                            Stroke::new(1.5, color),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                None if ordered => {
                    // On the baseline: the number is a row of the same
                    // font, whose baseline is one ascent below its top.
                    let _ = painter.text(
                        pos2(marker.right() - 6.0, marker.top()),
                        egui::Align2::RIGHT_TOP,
                        format!("{}.", start + i),
                        FontId::proportional(body_size()),
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
                let gap = if tight { 2.0 } else { block_gap() * 0.6 };
                indented(ui, |ui| {
                    draw_blocks(
                        ui,
                        &item.blocks,
                        rc,
                        gap,
                        salt.wrapping_mul(17).wrapping_add(i as u64),
                    )
                });
                ui.data_mut(|d| d.insert_temp(Id::new("list-depth"), depth));
            });
        });
    }
}

/// Horizontal space between table columns.
const CELL_GAP: f32 = 20.0;
/// Vertical space between table rows.
const ROW_GAP: f32 = 8.0;
/// Text size in table cells.
fn cell_size() -> f32 {
    body_size() * 0.95
}

/// A table laid out as a browser does (`table-layout: auto`): each column
/// as wide as its widest cell when they all fit, else at least as wide as
/// its longest word, sharing the rest in proportion to how much each needs
/// to stop wrapping. Cells are centred vertically in their row, as on GitHub.
fn draw_table(ui: &mut Ui, table: &Table, rc: &mut RenderCtx<'_>, salt: u64) {
    let border = rc.palette.border;
    let columns = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    // An HTML table may have no header; its first row is then striped as
    // the first under a header would be.
    let headless = table.header.is_empty();
    let rows: Vec<&Vec<Vec<Inline>>> = (!headless)
        .then_some(&table.header)
        .into_iter()
        .chain(table.rows.iter())
        .collect();
    let mut min = vec![0.0f32; columns];
    let mut max = vec![0.0f32; columns];
    for row in &rows {
        for (c, cell) in row.iter().enumerate() {
            let (lo, hi) = cell_extent(ui, cell, rc, cell_size());
            min[c] = min[c].max(lo);
            max[c] = max[c].max(hi);
        }
    }
    // Cell heights from the previous frame, to centre cells in their row.
    let heights_id = ui.id().with(("table-cells", salt));
    let previous: Vec<f32> = ui.data(|d| d.get_temp(heights_id)).unwrap_or_default();
    let mut heights = vec![0.0f32; rows.len() * columns];
    Frame::new()
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            let gaps = CELL_GAP * columns.saturating_sub(1) as f32;
            let widths = column_widths(&min, &max, ui.available_width() - gaps);
            let total = widths.iter().sum::<f32>() + gaps;
            ScrollArea::horizontal()
                .id_salt(("table-scroll", salt))
                .auto_shrink([true, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let stripe = ui.visuals().faint_bg_color;
                    for (r, row) in rows.iter().enumerate() {
                        if r > 0 {
                            ui.add_space(ROW_GAP);
                        }
                        let top = ui.cursor().min;
                        let striped = (r + usize::from(headless)) % 2 == 1;
                        let background = striped.then(|| ui.painter().add(egui::Shape::Noop));
                        let cells = r * columns..(r + 1) * columns;
                        let row_height = previous
                            .get(cells.clone())
                            .map_or(0.0, |h| h.iter().copied().fold(0.0, f32::max));
                        let mut x = top.x;
                        let mut bottom = top.y;
                        for (c, &width) in widths.iter().enumerate() {
                            let index = r * columns + c;
                            let cell = row.get(c).map_or(&[][..], Vec::as_slice);
                            let pad = previous
                                .get(index)
                                .map_or(0.0, |h| ((row_height - h) / 2.0).max(0.0).floor());
                            let rect =
                                Rect::from_min_size(pos2(x, top.y + pad), vec2(width, 1.0e5));
                            let mut child = ui.new_child(
                                UiBuilder::new()
                                    .id_salt(("cell", r, c))
                                    .max_rect(rect)
                                    .layout(Layout::top_down(egui::Align::Min)),
                            );
                            child.set_width(width);
                            if !cell.is_empty() {
                                let align = table.aligns.get(c).copied().unwrap_or_default();
                                let cell_salt = salt.wrapping_add((r * 1000 + c) as u64 + 1);
                                draw_inlines(
                                    &mut child,
                                    cell,
                                    rc,
                                    cell_size(),
                                    Style::NONE,
                                    align,
                                    cell_salt,
                                    false,
                                );
                            }
                            let used = child.min_rect();
                            heights[index] = used.height();
                            bottom = bottom.max(used.max.y);
                            x += width + CELL_GAP;
                        }
                        let row_rect = Rect::from_min_max(top, pos2(top.x + total, bottom));
                        if let Some(background) = background {
                            let fill = row_rect.expand2(vec2(2.0, ROW_GAP / 2.0));
                            ui.painter()
                                .set(background, egui::Shape::rect_filled(fill, 2.0, stripe));
                        }
                        ui.advance_cursor_after_rect(row_rect);
                    }
                });
        });
    let changed = heights.len() != previous.len()
        || heights
            .iter()
            .zip(&previous)
            .any(|(a, b)| (a - b).abs() > 0.5);
    if changed {
        ui.data_mut(|d| d.insert_temp(heights_id, heights));
        ui.ctx().request_repaint();
    }
}

/// Column widths for `available` points from the columns' narrowest
/// (longest word) and natural (unwrapped) widths.
fn column_widths(min: &[f32], max: &[f32], available: f32) -> Vec<f32> {
    let min_total: f32 = min.iter().sum();
    let max_total: f32 = max.iter().sum();
    if max_total <= available {
        return max.to_vec();
    }
    if min_total >= available || max_total <= min_total {
        return min.to_vec();
    }
    let share = (available - min_total) / (max_total - min_total);
    min.iter()
        .zip(max)
        .map(|(lo, hi)| (lo + (hi - lo) * share).floor())
        .collect()
}

/// The narrowest width a cell can wrap to (its longest word or widest
/// image) and its width unwrapped.
fn cell_extent(ui: &Ui, cell: &[Inline], rc: &mut RenderCtx<'_>, size: f32) -> (f32, f32) {
    let mut min = 0.0f32;
    let mut max = 0.0f32;
    let mut line = 0.0f32;
    for inline in cell {
        match inline {
            Inline::Text(rich) => {
                let mut job = rich_job(rich, rc, ui, size, Style::NONE).job;
                job.wrap.max_width = f32::INFINITY;
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                min = min.max(longest_word(&galley));
                for (i, row) in galley.rows.iter().enumerate() {
                    if i > 0 {
                        max = max.max(line);
                        line = 0.0;
                    }
                    line += row.rect().width();
                }
            }
            Inline::Image(image) => {
                let width = image_width(ui, image, rc);
                min = min.max(width);
                line += width;
            }
        }
    }
    // Room for rounding, so that text exactly as wide as its column does
    // not wrap.
    (min.ceil() + 1.0, max.max(line).ceil() + 1.0)
}

/// The width of the widest run of glyphs that a line cannot break inside.
fn longest_word(galley: &Galley) -> f32 {
    let mut longest = 0.0f32;
    for row in &galley.rows {
        let mut start: Option<f32> = None;
        for glyph in &row.glyphs {
            if glyph.chr == ' ' || glyph.chr == '\t' {
                start = None;
                continue;
            }
            let left = *start.get_or_insert(glyph.pos.x);
            longest = longest.max(glyph.pos.x + glyph.advance_width - left);
            // A line may break after any CJK character.
            if matches!(glyph.chr, '\u{3000}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}' | '\u{f900}'..='\u{faff}' | '\u{ff00}'..='\u{ffef}')
            {
                start = None;
            }
        }
    }
    longest
}

/// The width [`draw_image`] gives `image` when nothing limits it.
fn image_width(ui: &Ui, image: &ImageRef, rc: &RenderCtx<'_>) -> f32 {
    if let Some(width) = image.width {
        return width;
    }
    let alt_width = || {
        let alt = if image.alt.is_empty() {
            "image"
        } else {
            image.alt.as_str()
        };
        let font = egui::TextStyle::Body.resolve(ui.style());
        ui.fonts_mut(|f| f.layout_no_wrap(format!("[{alt}]"), font, Color32::PLACEHOLDER))
            .size()
            .x
    };
    let Some(uri) = image_source(ui.ctx(), &image.src, rc.base_dir).0 else {
        return alt_width();
    };
    let uri = super::animation::frame_uri(ui.ctx(), &uri).unwrap_or(uri);
    // The texture at its original size, which `draw_image` asks for too
    // when no size is given: the request does not depend on the room.
    match egui::Image::new(uri)
        .fit_to_original_size(1.0)
        .load_for_size(ui.ctx(), Vec2::INFINITY)
    {
        Ok(egui::load::TexturePoll::Ready { texture }) => match image.height {
            Some(height) if texture.size.y > 0.0 => height * texture.size.x / texture.size.y,
            _ => texture.size.x,
        },
        Ok(egui::load::TexturePoll::Pending { size }) => size.map_or(16.0, |s| s.x),
        Err(_) => alt_width(),
    }
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
                // A space between images (a row of badges). As a label it
                // would make the row taller and push the images after it down.
                Inline::Text(rich) if rich.text == " " => {
                    let font = egui::FontId::proportional(size);
                    let width = ui.ctx().fonts_mut(|f| f.glyph_width(&font, ' '));
                    ui.add_space(width);
                    continue;
                }
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
    let color = if style.has(Style::LINK) || style.has(Style::TAG) {
        palette.link
    } else {
        Color32::PLACEHOLDER
    };
    let background = if style.has(Style::MARK) {
        palette.mark_bg
    } else if style.has(Style::TAG) {
        // Obsidian's tag: the accent on a tint of it.
        palette.link.linear_multiply(0.12)
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
    // A superscript keeps its place above the baseline in a row made taller
    // by a formula: in a row of the usual height bottom and top alignment
    // put it at the same place.
    let (run_line_height, valign) = if style.has(Style::SUB) {
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

/// A formula or emoji in a [`LayoutJob`]: a no-break space as wide as the
/// picture, which the picture is painted over.
struct InlinePicture {
    /// The index of the space among the job's characters.
    char_index: usize,
    picture: Arc<Picture<Metrics>>,
    /// Points per em.
    em: f32,
    /// A search match falls in the picture; `true` for the current one.
    found: Option<bool>,
}

/// The text of a `Rich` as a layout job, its typeset formulas and drawn
/// emoji left as spaces for [`paint_inline_pictures`].
struct RichJob {
    job: LayoutJob,
    pictures: Vec<InlinePicture>,
    /// Byte offset in `Rich::text` of each character of the job, when they
    /// differ: a picture is one character in the job.
    char_bytes: Option<Vec<usize>>,
}

fn rich_job(rich: &Rich, rc: &mut RenderCtx<'_>, ui: &Ui, size: f32, extra: Style) -> RichJob {
    let (matches, current) = rc.find.matches_in(rich.id);
    let line_height = (size * LINE_HEIGHT).round();
    let has_pictures = rich
        .spans
        .iter()
        .any(|s| s.style.has(Style::MATH) || s.style.has(Style::EMOJI));
    let mut char_bytes = Vec::new();
    let mut job = LayoutJob::default();
    let mut pictures = Vec::new();
    for span in &rich.spans {
        let style = span.style | extra;
        if (style.has(Style::MATH) || style.has(Style::EMOJI))
            && let Some((format, picture, em)) = inline_picture(rich, span, style, rc, ui, size)
        {
            let found = matches
                .iter()
                .position(|m| m.range.start < span.range.end && span.range.start < m.range.end)
                .map(|k| Some(k) == current);
            let source = &rich.text[span.range.clone()];
            let copied = if !style.has(Style::MATH) {
                source.to_owned()
            } else if style.has(Style::DISPLAY) {
                format!("$${source}$$")
            } else {
                format!("${source}$")
            };
            let placeholder = crate::ui::copy::placeholder(&copied);
            let chars = placeholder.chars().count();
            pictures.push(InlinePicture {
                // The space comes last, after the marker.
                char_index: char_bytes.len() + chars - 1,
                picture,
                em,
                found,
            });
            job.append(&placeholder, 0.0, format);
            char_bytes.extend(std::iter::repeat_n(span.range.start, chars));
            continue;
        }
        let mut format = text_format(rc, style, size);
        // A link to a file that is not there, fainter, as Obsidian shows it.
        if let (Some(link), Some(base)) = (span.link, rc.base_dir)
            && let Some(target) = rich.links.get(link as usize)
            && crate::vault::link_missing(base, target, ui.ctx())
        {
            format.color = format.color.gamma_multiply(0.55);
        }
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
        if has_pictures {
            char_bytes.extend(
                rich.text[span.range.clone()]
                    .char_indices()
                    .map(|(i, _)| span.range.start + i),
            );
        }
    }
    // A row of only inline code would otherwise be shorter than a row of
    // text, which shows as misaligned table cells.
    job.first_row_min_height = line_height;
    RichJob {
        job,
        char_bytes: (!pictures.is_empty()).then_some(char_bytes),
        pictures,
    }
}

/// The format of the space that stands for an inline formula or emoji, its
/// picture and its points per em; `None` (TeX is shown as code, an emoji
/// as text) until the picture is made or if it cannot be.
fn inline_picture(
    rich: &Rich,
    span: &Span,
    style: Style,
    rc: &mut RenderCtx<'_>,
    ui: &Ui,
    size: f32,
) -> Option<(TextFormat, Arc<Picture<Metrics>>, f32)> {
    let color = if style.has(Style::LINK) {
        rc.palette.link
    } else {
        ui.visuals().text_color()
    };
    let source = &rich.text[span.range.clone()];
    let ctx = ui.ctx();
    let (em, state) = if style.has(Style::MATH) {
        let em = size * MATH_SCALE;
        let ppem = em * ctx.pixels_per_point();
        let display = style.has(Style::DISPLAY);
        (em, rc.pictures.math.get(ctx, source, display, color, ppem))
    } else {
        let em = if style.has(Style::SUP) || style.has(Style::SUB) {
            size * 0.75
        } else {
            size
        };
        let ppem = em * ctx.pixels_per_point();
        (em, rc.pictures.emoji.get(ctx, source, color, ppem))
    };
    let picture = match state {
        State::Ready(picture) => picture,
        State::Pending => {
            rc.pending = true;
            return None;
        }
        State::Failed(_) => return None,
    };
    // A formula wider than the text is made smaller to fit, as a line
    // cannot be scrolled and its end would be cut off at the edge; but not
    // below 60%, where it could not be read (copying it still gives it all).
    let available = ui.available_width();
    let width = picture.size(em).x;
    let em = if available > 0.0 && width > available {
        em * (available / width).max(0.6)
    } else {
        em
    };
    // The space is as wide as the picture: its font size makes it so (a
    // space has no outline, so no size of it takes room in the font atlas).
    let space_per_point =
        ui.fonts_mut(|f| f.glyph_width(&FontId::proportional(100.0), '\u{a0}')) / 100.0;
    let font_size = (picture.size(em).x / space_per_point.max(0.01)).max(0.5);
    // Text sits on the bottom of its row, its baseline `below` above it (see
    // `text_format`); the row grows to fit the picture above the baseline.
    // Below it the leading is enough for most formulas.
    let line_height = (size * LINE_HEIGHT).round();
    let below = line_height - rc.fonts.body_ascent * size;
    let needed = (picture.ascent(em) + below + 1.0).ceil();
    let format = TextFormat {
        font_id: FontId::proportional(font_size),
        line_height: Some(needed.max(line_height)),
        color: Color32::TRANSPARENT,
        valign: egui::Align::BOTTOM,
        ..Default::default()
    };
    Some((format, picture, em))
}

/// Paints the formulas and emoji of a laid-out [`RichJob`] over their
/// spaces, on the baseline of their row.
fn paint_inline_pictures(
    ui: &Ui,
    galley: &Galley,
    pos: egui::Pos2,
    pictures: &[InlinePicture],
    rc: &RenderCtx<'_>,
    size: f32,
) {
    let line_height = (size * LINE_HEIGHT).round();
    let below = line_height - rc.fonts.body_ascent * size;
    let painter = ui.painter();
    for inline in pictures {
        let cursor = egui::text::CCursor {
            index: egui::text::CharIndex(inline.char_index),
            prefer_next_row: true,
        };
        // The cursor before the space spans the height of its row.
        let at = galley.pos_from_cursor(cursor).translate(pos.to_vec2());
        let left = at.min.x;
        let baseline = at.max.y - below;
        if let Some(current) = inline.found {
            let rect = Rect::from_min_size(
                pos2(left, baseline - inline.picture.ascent(inline.em)),
                inline.picture.size(inline.em),
            )
            .expand(1.0);
            let fill = if current {
                rc.palette.search_current_bg
            } else {
                rc.palette.search_bg
            };
            painter.rect_filled(rect, 2.0, fill);
        }
        inline.picture.paint(painter, left, baseline, inline.em);
    }
}

/// Index of the character under `local` (relative to the galley).
fn char_under(galley: &Galley, local: Vec2) -> Option<usize> {
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
    Some(index)
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
    let RichJob {
        job,
        pictures,
        char_bytes,
    } = rich_job(rich, rc, ui, size, extra);
    let label = Label::new(WidgetText::LayoutJob(Arc::new(job))).selectable(true);
    let (pos, galley, response) = label.layout_in_ui(ui);
    // The byte of `rich.text` at character `index` of the job.
    let byte_at = |index: usize| match &char_bytes {
        Some(bytes) => bytes.get(index).copied().unwrap_or(rich.text.len()),
        None => rich
            .text
            .char_indices()
            .nth(index)
            .map_or(rich.text.len(), |(byte, _)| byte),
    };
    if ui.is_rect_visible(response.rect) {
        let color = ui.visuals().text_color();
        let shown = Arc::clone(&galley);
        if let Some(range) = selection::label(ui, &response, pos, shown, color, Stroke::NONE) {
            let range = range.as_sorted_char_range();
            let bytes = byte_at(range.start.0)..byte_at(range.end.0);
            let marked = rich.spans.iter().any(|span| {
                span.style.has(Style::MARK)
                    && span.range.start < bytes.end
                    && bytes.start < span.range.end
            });
            rc.select(Part::Text(rich.id), &rich.text, bytes, marked);
        }
        if !pictures.is_empty() {
            paint_inline_pictures(ui, &galley, pos, &pictures, rc, size);
        }
    }
    if response.secondary_clicked() {
        rc.menu_point = response
            .interact_pointer_pos()
            .and_then(|pointer| char_under(&galley, pointer - pos))
            .map(|index| {
                let at = byte_at(index);
                let marked = rich
                    .spans
                    .iter()
                    .any(|s| s.style.has(Style::MARK) && s.range.contains(&at));
                MenuPoint {
                    at: Selected {
                        part: Part::Text(rich.id),
                        range: at..at,
                        digest: source::digest(&rich.text),
                    },
                    marked,
                }
            });
    }
    let on_mark = rc
        .menu_point
        .as_ref()
        .is_some_and(|p| p.marked && p.at.part == Part::Text(rich.id));
    text_menu(&response, rc, on_mark);
    if !rich.links.is_empty() {
        let hovered = response
            .hover_pos()
            .and_then(|pointer| char_under(&galley, pointer - pos))
            .and_then(|index| match &char_bytes {
                Some(bytes) => bytes.get(index).copied(),
                None => rich.text.char_indices().nth(index).map(|(byte, _)| byte),
            })
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

/// The context menu of selectable text: copying the selection, as text or
/// as Markdown, and highlighting it. Opened only where it has something to
/// do: with text selected, or (`on_mark`) on highlighted text.
fn text_menu(response: &Response, rc: &mut RenderCtx<'_>, on_mark: bool) {
    let selected = rc.had_selection;
    if !selected && !on_mark {
        return;
    }
    let item = |ui: &mut Ui, enabled: bool, text: &str, shortcut: &str| {
        let button = egui::Button::new(text).shortcut_text(shortcut);
        ui.add_enabled(enabled, button).clicked()
    };
    super::menu::context_menu(response, |ui| {
        if item(ui, selected, tr!("Copy", "Копировать"), "Ctrl+C") {
            selection::copy(ui.ctx());
            ui.close();
        }
        if item(
            ui,
            selected,
            tr!("Copy as Markdown", "Копировать как Markdown"),
            "Ctrl+Shift+C",
        ) {
            rc.actions.push(ViewAction::CopyMarkdown);
            ui.close();
        }
        ui.separator();
        if item(ui, selected, tr!("Highlight", "Выделить цветом"), "Ctrl+H") {
            rc.actions.push(ViewAction::Highlight);
            ui.close();
        }
        if (rc.had_marks || on_mark)
            && item(
                ui,
                true,
                tr!("Remove highlight", "Снять выделение цветом"),
                "",
            )
        {
            rc.actions.push(ViewAction::Unhighlight);
            ui.close();
        }
    });
}

/// A `$$ … $$` block: the formula centred, scrolled sideways when wider than
/// the page. TeX that cannot be typeset is shown as code with the error.
fn draw_display_math(ui: &mut Ui, id: u32, tex: &str, rc: &mut RenderCtx<'_>, salt: u64) {
    let em = body_size() * MATH_SCALE;
    let color = ui.visuals().text_color();
    let ppem = em * ui.ctx().pixels_per_point();
    let formula = match rc.pictures.math.get(ui.ctx(), tex, true, color, ppem) {
        State::Ready(formula) => formula,
        State::Pending => {
            // About the height of a one-line formula, so little moves once
            // it is set.
            rc.pending = true;
            ui.allocate_exact_size(vec2(ui.available_width(), em * 1.6), Sense::hover());
            return;
        }
        State::Failed(error) => {
            draw_code(ui, id, None, "math", tex, rc, salt);
            ui.label(
                RichText::new(error.as_ref())
                    .size(12.0)
                    .color(rc.palette.alert(AlertKind::Caution)),
            );
            return;
        }
    };
    let (matches, current) = rc.find.matches_in(id);
    let found = (!matches.is_empty()).then(|| current.is_some());
    // A little room above and below for tall symbols' antialiasing.
    let margin = 4.0;
    let size = formula.size(em) + vec2(0.0, 2.0 * margin);
    let draw = |ui: &mut Ui| {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            let baseline = rect.top() + margin + formula.ascent(em);
            if let Some(current) = found {
                let fill = if current {
                    rc.palette.search_current_bg
                } else {
                    rc.palette.search_bg
                };
                ui.painter()
                    .rect_filled(rect.shrink2(vec2(0.0, margin - 2.0)), 3.0, fill);
            }
            formula.paint(ui.painter(), rect.left(), baseline, em);
        }
        super::menu::context_menu(&response, |ui| {
            if ui.button(tr!("Copy TeX", "Копировать TeX")).clicked() {
                ui.ctx().copy_text(tex.to_owned());
                ui.close();
            }
        });
    };
    if size.x <= ui.available_width() {
        ui.vertical_centered(draw);
    } else {
        ScrollArea::horizontal()
            .id_salt(("math-scroll", salt))
            .auto_shrink([false, true])
            .show(ui, draw);
    }
}

/// A ```` ```mermaid ```` block: the diagram centred at its own size, or
/// scaled down to the page's width. A source that cannot be drawn is shown as
/// code with the error.
fn draw_diagram(ui: &mut Ui, id: u32, source: &str, rc: &mut RenderCtx<'_>, salt: u64) {
    let dark = rc.palette.dark;
    let ppp = ui.ctx().pixels_per_point();
    let available = ui.available_width();
    let fit = |natural: Vec2| (available / natural.x).min(1.0);
    // Made at the size it is shown at, once that is known.
    let scale = rc
        .pictures
        .diagrams
        .peek(source, dark)
        .map_or(ppp, |d| fit(d.meta) * ppp);
    let diagram = match rc.pictures.diagrams.get(ui.ctx(), source, dark, scale) {
        State::Ready(diagram) => diagram,
        State::Pending => {
            rc.pending = true;
            ui.allocate_exact_size(vec2(available, 160.0), Sense::hover());
            return;
        }
        State::Failed(error) => {
            draw_code(ui, id, None, "mermaid", source, rc, salt);
            ui.label(
                RichText::new(error.as_ref())
                    .size(12.0)
                    .color(rc.palette.alert(AlertKind::Caution)),
            );
            return;
        }
    };
    let size = diagram.meta * fit(diagram.meta);
    let (matches, current) = rc.find.matches_in(id);
    let found = (!matches.is_empty()).then(|| current.is_some());
    ui.vertical_centered(|ui| {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            if let Some(current) = found {
                let color = if current {
                    rc.palette.search_current_bg
                } else {
                    rc.palette.search_bg
                };
                ui.painter().rect_stroke(
                    rect.expand(4.0),
                    4.0,
                    Stroke::new(2.0, color),
                    egui::StrokeKind::Outside,
                );
            }
            // On whole pixels, so that at the scale it was made for each
            // texel is one pixel.
            let corner = pos2(
                (rect.min.x * ppp).round() / ppp,
                (rect.min.y * ppp).round() / ppp,
            );
            let [w, h] = diagram.texture.size();
            let shown = Rect::from_min_size(
                corner,
                vec2(w as f32, h as f32) * (size.x / diagram.meta.x / diagram.scale),
            );
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter()
                .image(diagram.texture.id(), shown, uv, Color32::WHITE);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ZoomIn);
        }
        if response.clicked() {
            rc.actions
                .push(ViewAction::ShowPicture(super::lightbox::Shown::Diagram(
                    source.to_owned(),
                )));
        }
        super::menu::context_menu(&response, |ui| {
            if ui
                .button(tr!(
                    "Copy Mermaid source",
                    "Копировать исходный код Mermaid"
                ))
                .clicked()
            {
                ui.ctx().copy_text(source.to_owned());
                ui.close();
            }
        });
    });
}

/// The URI to draw an image source from: [`image_uri`], or, when that
/// file could not be read and the source is a relative path, where the
/// document's Obsidian vault has the file (`vault::locate`, which reads
/// nothing while the file is where the source says). Also whether the
/// vault is still being listed.
fn image_source(ctx: &egui::Context, src: &str, base_dir: Option<&Path>) -> (Option<String>, bool) {
    let Some(uri) = image_uri(src, base_dir) else {
        return (None, false);
    };
    if !uri.starts_with("file:") || ctx.try_load_bytes(&uri).is_ok() {
        return (Some(uri), false);
    }
    let (Some(base), Some(relative)) = (base_dir, relative_source(src)) else {
        return (Some(uri), false);
    };
    match crate::vault::locate(base, &relative, ctx) {
        crate::vault::Located::Found(path) => (Some(file_uri(&path)), false),
        crate::vault::Located::Waiting => (Some(uri), true),
        crate::vault::Located::Missing => (Some(uri), false),
    }
}

/// Every picture of `doc` the viewer opens with a click, in document
/// order: images that are not links (galleries' and embedded notes' too)
/// and Mermaid diagrams, by the URI or source the click gives
/// (`ViewAction::ShowPicture`).
pub fn document_pictures(
    ctx: &egui::Context,
    doc: &Document,
    base_dir: Option<&Path>,
) -> Vec<super::lightbox::Item> {
    use super::lightbox::{Item, Shown};
    fn image(ctx: &egui::Context, image: &ImageRef, base: Option<&Path>, out: &mut Vec<Item>) {
        if image.link.is_some() {
            return;
        }
        if let Some(uri) = image_source(ctx, &image.src, base).0 {
            let shown = Shown::Image(uri);
            let name = picture_name(&shown, &image.alt);
            out.push(Item { shown, name });
        }
    }
    fn inlines(ctx: &egui::Context, inlines: &[Inline], base: Option<&Path>, out: &mut Vec<Item>) {
        for inline in inlines {
            if let Inline::Image(i) = inline {
                image(ctx, i, base, out);
            }
        }
    }
    fn walk(ctx: &egui::Context, blocks: &[Block], base: Option<&Path>, out: &mut Vec<Item>) {
        for block in blocks {
            match block {
                Block::Heading { inlines: i, .. } | Block::Paragraph { inlines: i, .. } => {
                    inlines(ctx, i, base, out)
                }
                Block::Details { summary, .. } => inlines(ctx, summary, base, out),
                Block::Code { lang, code, .. } if lang.eq_ignore_ascii_case("mermaid") => {
                    let shown = Shown::Diagram(code.clone());
                    let name = picture_name(&shown, "");
                    out.push(Item { shown, name });
                }
                Block::Quote(blocks)
                | Block::Alert { blocks, .. }
                | Block::Footnote { blocks, .. } => walk(ctx, blocks, base, out),
                Block::Embed {
                    content: Some(Ok(blocks)),
                    ..
                } => walk(ctx, blocks, base, out),
                Block::List { items, .. } => {
                    for item in items {
                        walk(ctx, &item.blocks, base, out);
                    }
                }
                Block::Table(table) => {
                    for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                        inlines(ctx, cell, base, out);
                    }
                }
                Block::DefinitionList(items) => {
                    for (terms, details) in items {
                        walk(ctx, terms, base, out);
                        for detail in details {
                            walk(ctx, detail, base, out);
                        }
                    }
                }
                Block::Gallery { images, .. } => {
                    for i in images {
                        image(ctx, i, base, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(ctx, &doc.blocks, base_dir, &mut out);
    out
}

/// The name the picture viewer shows: an image's file name (its alt text
/// for one without), or "Diagram".
pub fn picture_name(shown: &super::lightbox::Shown, alt: &str) -> String {
    match shown {
        super::lightbox::Shown::Diagram(_) => tr!("Diagram", "Диаграмма").to_owned(),
        super::lightbox::Shown::Image(uri) => {
            let path = uri.split(['?', '#']).next().unwrap_or(uri);
            let name = percent_decode(path.rsplit(['/', '\\']).next().unwrap_or(path));
            if name.is_empty() || uri.starts_with("data:") {
                alt.to_owned()
            } else {
                name
            }
        }
    }
}

/// [`image_uri`] for a page to print: a relative file that is not there
/// is looked for in the vault at once.
pub fn image_uri_now(src: &str, base_dir: Option<&Path>) -> Option<String> {
    let uri = image_uri(src, base_dir)?;
    if let (Some(base), Some(relative)) = (base_dir, relative_source(src))
        && !base.join(&relative).exists()
        && let Some(found) = crate::vault::locate_now(base, &relative)
    {
        return Some(file_uri(&found));
    }
    Some(uri)
}

/// The decoded path of a source relative to the document, if it is one.
fn relative_source(src: &str) -> Option<String> {
    let src = src.trim();
    if src.is_empty() || src.contains("://") || src.starts_with("data:") {
        return None;
    }
    let path = percent_decode(src.split(['?', '#']).next().unwrap_or(src));
    (!path.is_empty() && !Path::new(&path).is_absolute()).then_some(path)
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
    // One next to the document, on its own computer, is fine.
    if crate::ui::confirm::on_another_host(&path, base_dir) {
        return None;
    }
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
    let (source_uri, waiting) = image_source(ui.ctx(), &image.src, rc.base_dir);
    // Measured again once the vault is listed.
    rc.pending |= waiting;
    let Some(source_uri) = source_uri else {
        return fallback(ui, rc);
    };
    let uri = super::animation::frame_uri(ui.ctx(), &source_uri).unwrap_or(source_uri.clone());
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
    let response = ui.add(widget.sense(Sense::click()));
    match &image.link {
        Some(link) => {
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if response.clicked() {
                rc.actions.push(ViewAction::OpenLink(link.clone()));
            }
        }
        // Not a link: a click shows it on its own.
        None => {
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::ZoomIn);
            }
            if response.clicked() {
                rc.actions
                    .push(ViewAction::ShowPicture(super::lightbox::Shown::Image(
                        source_uri,
                    )));
            }
        }
    }
    response
}

fn code_job(code: &str, highlights: Option<&[HlSpan]>, rc: &RenderCtx<'_>, id: u32) -> LayoutJob {
    let size = body_size() * CODE_SCALE;
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
                        let text = galley.text();
                        let byte_at = |index: usize| {
                            let at = text.char_indices().nth(index).map_or(text.len(), |c| c.0);
                            at.min(code.len())
                        };
                        let color = rc.palette.text;
                        let shown = Arc::clone(&galley);
                        let selected =
                            selection::label(ui, &response, pos, shown, color, Stroke::NONE);
                        if let Some(range) = selected {
                            let range = range.as_sorted_char_range();
                            let bytes = byte_at(range.start.0)..byte_at(range.end.0);
                            rc.select(Part::Code(id), code, bytes, false);
                        }
                    }
                    text_menu(&response, rc, false);
                });
        });
    let rect = frame.response.rect;
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect);
    if hovered {
        let button_rect =
            Rect::from_min_size(rect.right_top() + vec2(-32.0, 6.0), vec2(26.0, 24.0));
        let copied_id = ui.id().with(("copied", salt, id));
        let copied_at: Option<f64> = ui.data(|d| d.get_temp(copied_id));
        let now = ui.input(|i| i.time);
        let recently = copied_at.is_some_and(|t| now - t < 1.5);
        if super::copy_button(ui, button_rect, recently).clicked() {
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
    let line = body_size() * LINE_HEIGHT;
    let chars_per_line = (width / (body_size() * 0.5)).max(10.0);
    let text_height = |len: usize, newlines: usize, size: f32| {
        let scale = size / body_size();
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
    let gap = block_gap();
    match block {
        Block::Heading { level, inlines, .. } => {
            inline_height(inlines, heading_size(*level))
                + body_size() * 0.5
                + if *level <= 2 { 6.0 } else { 0.0 }
                + gap
        }
        Block::Paragraph { inlines, .. } => inline_height(inlines, body_size()) + gap,
        Block::Code { lang, .. } if lang.eq_ignore_ascii_case("mermaid") => 300.0 + gap,
        Block::Code { code, .. } | Block::Math { tex: code, .. } => {
            (code.lines().count().max(1) as f32) * (body_size() * CODE_SCALE * 1.45).round()
                + 24.0
                + gap
        }
        Block::FrontMatter {
            properties: Some(properties),
            ..
        } => (properties.len() + 1) as f32 * line + gap,
        Block::FrontMatter { .. } => line + gap,
        // Folded as written: its title row (measured anew if unfolded).
        Block::Alert {
            fold: Some(Fold { open: false, .. }),
            ..
        } => line + gap,
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
        Block::Rule => body_size() + gap,
        Block::Details { .. } => line + gap,
        Block::DetailsEnd => 0.0,
        Block::Gallery { masonry, images } => {
            let rows = if *masonry {
                images.len().div_ceil(GALLERY_COLUMNS) as f32 * width / GALLERY_COLUMNS as f32
            } else {
                images.len().div_ceil(3) as f32 * GALLERY_ROW * body_size() / 16.0
            };
            rows + gap
        }
        Block::Embed { content, .. } => {
            let inner = match content {
                Some(Ok(blocks)) => blocks.iter().map(|b| estimate(b, width - 40.0)).sum(),
                _ => line,
            };
            line + inner + gap
        }
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
    fn galleries_fill_rows_and_columns() {
        // Two 2:1 pictures and a 1:1 at 100 high want 500 of 450: one row
        // of all three, scaled to fit; then a short last row at 100.
        let rects = justified_rows(&[2.0, 2.0, 1.0, 1.0], 450.0, 100.0, 0.0);
        assert_eq!(rects.len(), 4);
        assert!((rects[2].right() - 450.0).abs() < 0.01);
        assert!((rects[0].height() - 90.0).abs() < 0.01);
        assert_eq!(
            rects[3],
            Rect::from_min_size(pos2(0.0, 90.0), vec2(100.0, 100.0))
        );
        // With gaps, a full row still ends at the width.
        let rects = justified_rows(&[1.5; 7], 500.0, 120.0, 5.0);
        let first_row: Vec<&Rect> = rects.iter().filter(|r| r.top() == 0.0).collect();
        assert!((first_row.last().unwrap().right() - 500.0).abs() < 0.01);

        // Masonry: under the shortest column.
        let rects = masonry_columns(&[1.0, 0.5, 1.0, 1.0], 310.0, 3, 5.0);
        assert_eq!(
            rects[0],
            Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0))
        );
        assert_eq!(rects[1].height(), 200.0);
        assert_eq!(rects[3].left(), 0.0, "column 0 is shortest again");
        assert_eq!(rects[3].top(), 105.0);
    }

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
    fn images_on_another_computer_are_not_loaded() {
        let local = Some(Path::new(r"C:\docs"));
        assert_eq!(image_uri("//evil/share/x.png", local), None);
        assert_eq!(image_uri(r"\\evil\share\x.png", local), None);
        let share = Some(Path::new(r"\\srv\docs"));
        assert_eq!(
            image_uri("img/x.png", share).as_deref(),
            Some(r"file://srv\docs\img/x.png")
        );
        assert_eq!(image_uri(r"\\evil\share\x.png", share), None);
    }

    #[cfg(windows)]
    #[test]
    fn unc_paths_keep_their_host() {
        assert_eq!(
            file_uri(Path::new(r"\\srv\share\a.png")),
            r"file://srv\share\a.png"
        );
    }

    #[test]
    fn closed_details_hide_what_is_inside_them() {
        let blocks: Vec<Block> = [
            "a",
            "<details open>",
            "b",
            "<details>",
            "c",
            "</details>",
            "d",
            "</details>",
            "e",
        ]
        .iter()
        .scan(0, |index, item| {
            Some(match *item {
                "<details>" | "<details open>" => {
                    *index += 1;
                    Block::Details {
                        index: *index - 1,
                        open: *item == "<details open>",
                        summary: Vec::new(),
                    }
                }
                "</details>" => Block::DetailsEnd,
                _ => Block::Rule,
            })
        })
        .collect();
        let hidden = |toggled: &[u32]| {
            let toggled: HashSet<u32> = toggled.iter().copied().collect();
            hidden_blocks(&blocks, &toggled).unwrap()
        };
        let (f, t) = (false, true);
        assert_eq!(hidden(&[]), [f, f, f, f, t, t, f, t, f]);
        assert_eq!(hidden(&[1]), [f, f, f, f, f, t, f, t, f]);
        assert_eq!(hidden(&[0]), [f, f, t, t, t, t, t, t, f]);
        assert_eq!(hidden_blocks(&blocks[..1], &HashSet::new()), None);
    }
}
