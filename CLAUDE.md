# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

qmarkdown is a native, read-only Markdown viewer in Rust (egui/eframe). It is a rewrite of the viewing half of Markpad (Tauri + SvelteKit). The overriding goal is **time from process start to the first frame showing the document**. Treat any change that adds work before the first frame as a regression unless it is measured.

## Commands

```
cargo build --release          # target\release\qmarkdown.exe (fat LTO, ~1.5 min)
cargo build                    # debug; dependencies are built at opt-level 2
cargo test                     # all unit tests (no integration tests)
cargo test wikilink            # tests whose name contains "wikilink"
cargo test markdown::preprocess::tests::copy_reference_output_becomes_a_real_link
cargo clippy --all-targets     # kept at zero warnings
cargo fmt
```

Measure startup (writes ms-since-process-creation marks as JSON, then exits):

```
target\release\qmarkdown.exe --bench timings.json samples\markdown-syntax.md
```

Marks: `main`, `run_native`, `event_loop`, `window_builder`, `app_new`, `fonts_ready`, `first_frame_start`, `doc_ready`, `doc_frame_end`, `next_frame_end`. Baseline on the dev machine: `doc_frame_end` ≈ 168 ms for the 27 KB sample, ≈ 208 ms for 4.9 MB. About 130 ms of that is OpenGL context creation (`window_builder` → `app_new`). Use `--new-instance` when launching for manual testing so the single-instance hand-off doesn't route the file into an already-open window.

Release builds use `windows_subsystem = "windows"` (no console); debug builds keep the console. Settings are written to `%APPDATA%\qmarkdown\config\settings.json` by any non-`--bench` run.

## Architecture

### Startup pipeline (`main.rs` → `app.rs`)

1. `main` parses args, normalizes paths, and tries `single_instance::hand_off` (local socket / named pipe). If another instance accepts, the process exits.
2. Before creating the window, `main` spawns one loader thread per file (`app::spawn_load`: read → `fs_text::decode_text` → `Document::parse`). Results come back as `AppMsg::Loaded` over an mpsc channel.
3. eframe starts with the **glow** renderer. wgpu was measured slower (DX12 ~240 ms, Vulkan ~290 ms vs ~130 ms for glow). `eframe` uses `default-features = false` so wgpu is not linked; don't re-enable default features.
4. On the first frame, `App::ui` blocks briefly (`wait_for_first_document`) so the window appears with the document rather than empty. Fonts are set in `App::new` but only take effect at the next frame start, which is why `FontChoice::measure_ascents` runs in frame 1.
5. Deferred until frame 2 or later: file watcher, syntax-highlighter warm-up, and CJK fallback fonts (loaded only if `Document::scripts` says the text needs them).

Background threads reach the UI through `CtxCell` (`Arc<OnceLock<egui::Context>>`) plus `request_repaint`. Loads carry a per-tab `generation` so stale reload results are dropped.

### Parsing (`src/markdown/`)

- `preprocess.rs` is ported from Markpad. It does text-level rewrites before comrak: wikilinks `[[file#Heading|alias]]`, `^block-id` anchors, Obsidian embeds `![[img.png|300x200]]`, and parenthesized bare URLs. All are guarded by `code_region_ranges`, a CommonMark-accurate scanner that keeps rewrites out of code. Embeds become Markdown images whose title carries `qmd-size:WxH`. Block ids become `<a id=… class="block-id-anchor">`. `markdown_options()` defines the dialect: GFM plus highlight, insert, inline footnotes, description lists, alerts, dollar math, front matter, and **single newline = line break**. `~sub~`, `^sup^` and `||spoiler||` are deliberately off.
- `document.rs` walks the comrak AST once into an egui-independent model: `Vec<Block>` with `Rich` text (string + style spans + link table). Each `Rich` and code block gets a sequential `id` in document order, and `Document::search` holds `(id, top-level block, text)` for find. Heading slugs use one `Anchorizer` per document, so ids match comrak's HTML (tests assert this against `comrak::markdown_to_html`). `Document::anchors` maps slug, block id, or `fn-<name>` to a top-level block index.
- `html.rs` is a tiny tag tokenizer. Raw HTML is reduced to the README subset (img, a, br, aligned p/div/h*, inline style tags). `Builder::html_align` persists `<p align=center>` alignment across separate HTML blocks so Markdown between them is centered too.
- `highlight.rs` is syntect + two-face on a worker thread. `Highlighter::get` returns `None` until ready and the worker triggers a repaint. The cache key is `Block::Code::key`, computed at parse time, plus the dark/light flag.

### Rendering (`src/ui/viewer.rs`)

This file holds most of the subtle logic:

- **Virtualization.** `ViewState` keeps per-block heights, estimated by `estimate()` and then measured. Each frame lays out only the visible blocks, placed sequentially from the first visible block's offset. It also measures off-screen blocks in an invisible UI within a 4 ms budget. Heights are invalidated on width change, document identity change, or `fonts_generation` change.
- **Scroll anchoring.** The first visible block's top is remembered. If measurements above it change its offset, the scroll offset is shifted by the delta. Programmatic jumps (`jump_to(block, offset_from_view_top)`) are re-applied for several frames (`settle`) while heights converge.
- **Text.** `Rich` becomes a `LayoutJob` and is shown through `Label::layout_in_ui` plus `LabelSelectionState::label_text_selection`, which gives cross-block selection. Links are hit-tested with `char_under`. Colors use `Color32::PLACEHOLDER` so quotes and footnotes can mute text via `override_text_color`.
- **Baseline alignment.** Every run uses the paragraph's line height. Smaller or monospace runs get a reduced line height with `valign = BOTTOM`, computed from measured font ascents, so baselines line up. `first_row_min_height` stops code-only rows from shrinking.
- **Centered content and tables.** Centered rows with images, and centered or right-aligned table cells, cannot use egui's expanding layouts. They pad by width remembered from the previous frame in `ui.data`. `Grid` hands non-last cells the full `max_col_width`, so centered layouts would stretch columns.
- **Images.** On Windows, egui_extras requires `file:///C:/…` (three slashes). Two slashes are parsed as a UNC host. See `file_uri`.

### Other modules

- `style.rs`: GitHub-like palettes, egui visuals, and system font loading (Segoe UI with real bold and italic faces, plus Consolas, on Windows) with egui's bundled fonts as fallbacks.
- `app.rs`: tabs, link resolution (`#anchor`, relative `.md` opened in a tab with fragment, other targets via `open`), shortcuts, find bar, and the TOC panel.
- `watcher.rs` watches the parent directory, not the file, so save-by-rename is caught, and coalesces events for 150 ms.

## Conventions

- `src/markdown/preprocess.rs` and `src/fs_text.rs` contain BSD-3 code from Markpad. Keep their copyright headers, and update `THIRD_PARTY_LICENSES.md` if more Markpad code is ported.
- Before claiming a performance change, check it with `--bench`. Run it several times and ignore the first, cold run.
