# Third-party licenses

## Markpad

MetalMD is a rewrite of the viewing part of [Markpad](https://github.com/sftwrdotdev/Markpad).
The following files contain code ported from Markpad's `src-tauri/src/markdown.rs`
and `src-tauri/src/fs_safety.rs`, together with their tests:

- `src/markdown/preprocess.rs` — comrak options, wikilinks, block ids, embeds,
  parenthesized autolinks and the code-region scanner.
- `src/fs_text.rs` — encoding detection and decoding.

`samples/markdown-syntax.md` is Markpad's syntax reference document, copied unchanged.

```
BSD 3-Clause License

Copyright (c) 2023, Alec Ames

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## egui

`src/ui/selection.rs` is adapted from egui 0.36's
`src/text_selection/label_text_selection.rs`
([egui](https://github.com/emilk/egui), dual-licensed MIT or Apache-2.0; used
here under the MIT License).

```
MIT License

Copyright (c) 2018-2021 Emil Ernerfeldt <emil.ernerfeldt@gmail.com>

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Fonts embedded in the binary

egui's `default_fonts` feature embeds these fonts, used as fallbacks for
symbols and emoji (and as the main fonts where no system font is found):

- Ubuntu Light — Ubuntu Font Licence 1.0
- Hack — MIT License (Bitstream Vera derivative)
- Noto Emoji — SIL Open Font License 1.1
- emoji-icon-font — MIT License

Their license texts are in `licenses/THIRD_PARTY_NOTICES.txt`.

The KaTeX fonts (`KaTeX_*.ttf`, from [KaTeX](https://github.com/KaTeX/KaTeX)),
used for formulas, are embedded through the `ratex-katex-fonts` crate under the
SIL Open Font License 1.1; its notice is in `licenses/THIRD_PARTY_NOTICES.txt` too.

On Windows these system fonts are read from `%WINDIR%\Fonts` at run time; they
are not redistributed: Segoe UI, Consolas or Courier New and Segoe UI Symbol
for text; Microsoft YaHei, Yu Gothic, SimSun and Malgun Gothic for CJK
documents; Segoe UI Emoji for colour emoji; Verdana, Arial, Times New Roman and
Courier New for text in SVG images.

## Everything else

The licences and copyright notices of the Rust crates compiled into the
program, of the fonts above and of the WebView2 SDK's static loader (linked in
to start WebView2 for printing) are in
[licenses/THIRD_PARTY_NOTICES.txt](licenses/THIRD_PARTY_NOTICES.txt), written
by `tools/notices.py` from `cargo metadata`. The licences of the syntax
definitions and colour themes used for code, which the two-face crate embeds
(collected by the bat project from Sublime Text packages), are in
[licenses/SYNTAXES_AND_THEMES.txt](licenses/SYNTAXES_AND_THEMES.txt), as
`two_face::acknowledgement::listing()` gives them. The program shows all these
texts in About > Licenses…, and the installer and the portable archive ship
them as files.
