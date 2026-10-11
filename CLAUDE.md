# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

MetalMD is a native, read-only Markdown viewer in Rust (egui/eframe). It is a rewrite of the viewing half of Markpad (Tauri + SvelteKit). The overriding goal is **time from process start to the first frame showing the document**. Treat any change that adds work before the first frame as a regression unless it is measured.

The name is written `MetalMD` wherever people read it (UI, window title, READMEs, installer, ProgID `MetalMD.md`, `%APPDATA%\MetalMD`, `%LOCALAPPDATA%\Programs\MetalMD`, `MetalMD_<ver>_Setup.exe`) and `metalmd` wherever it is an identifier (Cargo package, `metalmd.exe`, `metalmd.json`, the single-instance socket, the GitHub repo `Fan4Metal/metalmd`). This follows disk_flashlight (`Disk Flashlight` / `disk_flashlight.exe`).

## Commands

```
cargo build --release          # target\release\metalmd.exe (fat LTO, ~1.5 min)
cargo build                    # debug; dependencies are built at opt-level 2
cargo test                     # all unit tests (no integration tests)
cargo test wikilink            # tests whose name contains "wikilink"
cargo test markdown::preprocess::tests::copy_reference_output_becomes_a_real_link
cargo clippy --all-targets     # kept at zero warnings
cargo fmt
uv run build.py                # release build, first closing a metalmd.exe running from target\
python tools/make_release.py   # tests + release build + dist\MetalMD_<ver>_Setup.exe and _portable.zip (needs Inno Setup 6); --no-tests, --install
```

Releases: pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`, which runs `make_release.py` and publishes both files. The tag must equal the version in `Cargo.toml`. `tools/setup.iss` installs per user. Its `assoc` task registers `.md`, `.markdown`, `.mdown` and `.mkd` through `[Registry]` entries: a ProgID, OpenWithProgids and Default apps capabilities. qview does this from its exe with `--register`; MetalMD has no such code. `make_release.py` gets the installer icon from `metalmd.exe --export-icon <file>`. `.cargo/config.toml` links the CRT statically, so no VCRUNTIME140.dll is needed. These scripts mirror qview's and disk_flashlight's; keep them in step.

Measure startup (writes ms-since-process-creation marks as JSON, then exits):

```
target\release\metalmd.exe --bench timings.json samples\markdown-syntax.md
```

Marks: `main`, `run_native`, `event_loop`, `window_builder`, `app_new`, `fonts_ready`, `first_frame_start`, `doc_ready`, `doc_frame_end`, `next_frame_end`. Baseline on the dev machine: `doc_frame_end` ≈ 168 ms for the 27 KB sample, ≈ 208 ms for 4.9 MB. About 130 ms of that is OpenGL context creation (`window_builder` → `app_new`). Use `--new-instance` when launching for manual testing so the single-instance hand-off doesn't route the file into an already-open window.

Release builds use `windows_subsystem = "windows"` (no console); debug builds keep the console. Any non-`--bench` run writes the settings. They go to `%APPDATA%\MetalMD\config\settings.json`, or to `metalmd.json` next to the exe when that file exists and can be written (portable mode, `settings::location`). The portable zip ships an empty `metalmd.json`.

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
- `emoji.rs` finds the emoji that a browser would draw in colour (UTS #51: `Emoji_Presentation` or U+FE0F, with skin tones, ZWJ sequences, flags, keycaps; `©` or `✔` alone stay text), using `unicode-properties`' table. `InlineCtx::push_span` makes each one a `Style::EMOJI` span of its own, outside code and math. The scan skips to UTF-8 lead bytes that can begin one, so text in other scripts costs almost nothing.
- `document.rs` walks the comrak AST once into an egui-independent model: `Vec<Block>` with `Rich` text (string + style spans + link table). Each `Rich` and code block gets a sequential `id` in document order, and `Document::search` holds `(id, top-level block, text)` for find. Heading slugs use one `Anchorizer` per document, so ids match comrak's HTML (tests assert this against `comrak::markdown_to_html`). `Document::anchors` maps slug, block id, or `fn-<name>` to a top-level block index.
- `html.rs` is a tiny tag tokenizer. Raw HTML is reduced to the README subset (img, a, br, aligned p/div/h*, inline style tags). `Builder::html_align` persists `<p align=center>` alignment across separate HTML blocks so Markdown between them is centered too.
- `highlight.rs` is syntect + two-face on a worker thread. `Highlighter::get` returns `None` until ready and the worker triggers a repaint. The cache key is `Block::Code::key`, computed at parse time, plus the dark/light flag.

### Rendering (`src/ui/viewer.rs`)

This file holds most of the subtle logic:

- **Virtualization.** `ViewState` keeps per-block heights, estimated by `estimate()` and then measured. Each frame lays out only the visible blocks, placed sequentially from the first visible block's offset. It also measures off-screen blocks in an invisible UI within a 4 ms budget. Heights are invalidated on width change, document identity change, or `fonts_generation` change.
- **Scroll anchoring.** The first visible block's top is remembered. If measurements above it change its offset, the scroll offset is shifted by the delta. Programmatic jumps (`jump_to(block, offset_from_view_top)`) are re-applied for several frames (`settle`) while heights converge.
- **Text.** `Rich` becomes a `LayoutJob` and is shown through `Label::layout_in_ui` plus `LabelSelectionState::label_text_selection`, which gives cross-block selection. Links are hit-tested with `char_under`. Colors use `Color32::PLACEHOLDER` so quotes and footnotes can mute text via `override_text_color`.
- **Baseline alignment.** Every run uses the paragraph's line height. Smaller or monospace runs get a reduced line height with `valign = BOTTOM`, computed from measured font ascents, so baselines line up. `first_row_min_height` stops code-only rows from shrinking.
- **Centered content.** Centered rows with images cannot use egui's expanding layouts. They pad by width remembered from the previous frame in `ui.data`.
- **Tables** do not use `egui::Grid`, which gives the last column only what is left of the view and centres cells unevenly. `draw_table` lays them out as a browser does (`table-layout: auto`): `cell_extent` measures each cell's longest word (`longest_word`, from a galley laid out unwrapped) and unwrapped width, `column_widths` gives every column its unwrapped width when they all fit, else its minimum plus a share of the rest, and a table whose minimums don't fit scrolls sideways. Each cell is a child `Ui` of its column's width, so `draw_inlines` aligns it; cells are centred vertically in their row by heights from the previous frame.
- **Pictures from text.** `ui/render_cache.rs` turns a source into a texture on a worker thread, like the highlighter: `RenderCache::get(source, variant, scale)` returns `Pending` until the texture is ready, then the picture plus scale-independent layout data (`meta`); after a zoom or resize the old texture is drawn scaled until one at the new scale arrives (`peek` reads the last one without asking). A block measured off screen while a picture in it was pending stays unmeasured (`RenderCtx::pending`). A picture set in text (formula, emoji) has `render_cache::Metrics` (width, height, depth in ems) and is painted by `Picture::paint` on the baseline, its texture starting `PAD_PX` left of and above its box. `viewer::Pictures` holds the three users:
- **Math.** `ui/math.rs` typesets TeX with RaTeX (`ratex-parser` → `ratex-layout` → `ratex-svg` with the embedded KaTeX fonts) and rasterises the SVG with resvg. The variant is display style + colour; the meta is width/height/depth in ems, the scale pixels per em. ratex-svg writes its size in `pt`, which usvg reads as 4/3 px, hence the scale in `render`. A `$$ … $$` paragraph is `Block::Math` (centred, scrolled sideways if too wide, code plus the error when it fails, "Copy TeX" in its context menu). Inline `$…$` is a `Style::MATH` span of its own (`Style::DISPLAY` for `$$` inside text); `rich_job` replaces a typeset one with a transparent no-break space whose font size makes it as wide as the formula and whose line height makes the row tall enough above the baseline, and `paint_inline_pictures` paints the formula on the row's baseline at that character (`pos_from_cursor`). `RichJob::char_bytes` maps job characters back to `Rich::text` bytes for links. egui's selection copies the job's text, so `ui/copy.rs` puts a marker of characters egui draws as nothing (U+2063, then the source's number in U+2062/U+2064 binary digits) before each picture's space, and `CopyPlugin::output_hook` replaces marker and space in `CopyText` with the source (`$tex$`, `$$tex$$`, the emoji). The marker goes before the space because the pointer puts the cursor before the marker or after the space, never inside, so a selection holds all of it or none. Until typeset (or when it fails) the TeX shows as inline code. Release builds use `panic = "abort"`, so a panic inside RaTeX or mermaid-svg ends the process.
- **Emoji.** egui draws every glyph in one colour, so a `Style::EMOJI` span is a picture too, through the same no-break space as an inline formula: `ui/emoji.rs` sets it with resvg in the system colour font (Segoe UI Emoji's COLR; Noto Color Emoji or Apple Color Emoji elsewhere), read on the worker with the first emoji. The variant is the text colour. Its width is where a trailing space lands (the shaper gives U+FE0F and joiners no advance), or its ink if wider; usvg's boxes of text are the line's and of COLR glyphs in font units, so the ink is found by drawing it with room and scanning the pixels, then drawing it again in its box. Until drawn, or when the font lacks it (newer emoji on Windows 10), it is text in egui's monochrome fallback fonts; the TOC and code stay text.
- **Diagrams.** A code block whose language is `mermaid` is drawn by `ui/diagram.rs`: mermaid-svg lays it out (text measured by character widths, no fonts) and writes SVG in Mermaid's default or dark theme (the variant), with `responsive = false` for a fixed size; resvg draws it with `svg::text_options` (system fonts). The meta is its own size in points; `draw_diagram` shows it centred at that size or scaled down to the page, and asks for the scale it is shown at. A source mermaid-svg rejects is shown as code with the error.
- **Images.** On Windows, egui_extras requires `file:///C:/…` (three slashes). Two slashes are parsed as a UNC host. See `file_uri`.
- **Animation.** egui animates GIF/WebP only for URIs ending in `.gif`/`.webp`. `ui/animation.rs` covers the rest: `frame_uri` sniffs the loaded bytes (query-string URLs, extensionless GitHub `user-attachments` links, APNG) and picks `uri#frame` itself; `ApngLoader` decodes APNG; frame delays ≤ 10 ms play at 100 ms as in browsers.
- **SVG.** egui_extras' `svg` feature is off: its loader takes only `.svg` URIs and draws no text. `ui/svg.rs` replaces it, sniffing SVG by bytes or media type (shields.io badges have no extension) and rendering text with resvg's `text` feature. Fonts (Verdana, Arial, Segoe UI…) are read on the first SVG that has `<text>`, never at startup. `rasterize` keeps an SVG within the GPU's `max_texture_side` and 64 M texels, whatever size it names.
- **Texture limit.** egui_glow asserts that a texture fits `max_texture_side`, so with `panic = "abort"` an oversized image ended the app. `ui/limit.rs` (`LimitLoader`, installed after every other image loader) asks the loaders before it itself and halves a too-large image by averaging texels; its `source_size` stays, so it is drawn at its own size.

### Other modules

- `style.rs`: GitHub-like palettes, egui visuals, and system font loading (Segoe UI with real bold and italic faces, plus Consolas, on Windows) with egui's bundled fonts as fallbacks.
- `app.rs`: tabs, link resolution (`#anchor`, relative `.md` opened in a tab with fragment, `file:` URLs as paths, other targets via `open` on a background thread; `ui/confirm.rs` asks first for runnable files, other schemes than http(s)/mailto, and UNC paths on another host than the document's), shortcuts, find bar, and the TOC panel (left or right, `Settings::toc_side`, chosen in the context menu of its button or of the panel, `ui::toc_side_menu`).
- `src/icon.rs` draws the app icon procedurally and has no dependencies. `build.rs` includes it via `#[path]`, writes the `.ico` embedded by `winresource`, and writes `app_icon_{64,128,256}.rgba` into `OUT_DIR`. `main::embedded_icon` includes those bytes for the window icon, the start page and About (`ui::app_icon`), so nothing is rasterized at startup. This is the same scheme as the sibling apps qview and disk_flashlight, whose About dialog, start screen and theme button (`ui::theme_button`) these UI pieces mirror.
- `placement.rs`: in `App::new`, while the window is hidden, moves it into the nearest monitor's work area if its caption cannot be reached (the saved monitor is gone). The saved geometry is in unzoomed points (`remember_window` multiplies by the zoom), because the window is created before `set_zoom_factor`.
- `watcher.rs` watches the parent directory, not the file, so save-by-rename is caught, and coalesces events for 150 ms.

## Conventions

- Every user-visible string goes through `tr!("English", "Русский")` (`src/i18n.rs`, the scheme of qview). The language follows the Windows UI language (`i18n::system_lang`) unless chosen in About (`Settings::language`, `system`/`en`/`ru`); `main` sets it from the settings before the window is created, About applies a new choice at once. Strings are built every frame, so nothing caches translated text. Log messages, alert titles (`[!NOTE]` and others, as on GitHub) and file dialog type names stay English.
- `src/markdown/preprocess.rs` and `src/fs_text.rs` contain BSD-3 code from Markpad. Keep their copyright headers, and update `THIRD_PARTY_LICENSES.md` if more Markpad code is ported.
- Before claiming a performance change, check it with `--bench`. Run it several times and ignore the first, cold run.
