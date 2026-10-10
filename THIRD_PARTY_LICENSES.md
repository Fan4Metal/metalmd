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

## Fonts embedded in the binary

egui's `default_fonts` feature embeds these fonts, used as fallbacks for
symbols and emoji (and as the main fonts where no system font is found):

- Ubuntu Light — Ubuntu Font Licence 1.0
- Hack — MIT License (Bitstream Vera derivative)
- Noto Emoji — SIL Open Font License 1.1
- emoji-icon-font — MIT License

The license texts ship with the `epaint_default_fonts` crate.

On Windows the system fonts Segoe UI, Consolas, Segoe UI Symbol and (for CJK
documents) Microsoft YaHei / Malgun Gothic are read from `%WINDIR%\Fonts` at run
time; they are not redistributed.

## Rust crates

All other dependencies are Rust crates under MIT, Apache-2.0, BSD, Zlib or
similar permissive licenses. To list them:

```
cargo install cargo-license
cargo license
```
