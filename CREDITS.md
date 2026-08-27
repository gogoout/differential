# Credits

`differential` stands on other people's work. This page names it.

## Vendored code

Some of the terminal reviewer is adapted from two MIT-licensed projects. Every adapted
file carries its own attribution header. The upstream copyright notices are also in
[`LICENSE-MIT`](LICENSE-MIT).

### `agavra/tuicr`

Commit `0dacb6b`. MIT License, Copyright (c) 2025 tuicr contributors.

- [`crates/tui/src/vendor/syntax.rs`](crates/tui/src/vendor/syntax.rs) — syntax
  highlighting over `syntect`. The markdown layer and the whole-file highlight heuristic
  were removed. `highlight_ranges`, `Highlighted` and `LOOKBACK` are ours, not tuicr's.
- [`crates/tui/src/vendor/terminal.rs`](crates/tui/src/vendor/terminal.rs) — the terminal
  lifecycle: enter, draw, restore, and a teardown that runs on `Drop`.
- [`crates/tui/src/vendor/text_utils.rs`](crates/tui/src/vendor/text_utils.rs) — one span
  truncation helper.

The cached process-wide highlighter in `crates/tui/src/theme.rs` follows tuicr's
`OnceLock<Arc<..>>` pattern. The single row builder in `crates/tui/src/rows.rs` applies a
lesson learned from reading tuicr.

### `jnsahaj/lumen`

Commit `f600389`. MIT License, Copyright (c) 2024 Sahaj Jain.

- [`crates/tui/src/vendor/diff_algo.rs`](crates/tui/src/vendor/diff_algo.rs) — the
  blob-to-rows diff engine with word-level emphasis.
- [`crates/tui/src/vendor/diff_types.rs`](crates/tui/src/vendor/diff_types.rs) — the row
  and segment types, plus tab expansion.

The colour field schema in `crates/tui/src/theme.rs` is modelled on lumen's `DiffColors`.

## Prior art

- **`semantic-diff`.** An open-source tool that asks a model to assign hunks to groups
  directly. We measured that shape and it dropped up to 73% of hunks while reporting
  success. That measurement is why this tool lets the model name only class ids. See
  [`adr/0001-llm-merges-class-ids-never-hunks.md`](adr/0001-llm-merges-class-ids-never-hunks.md).
- **Git's own `diff-highlight`.** The word-level emphasis follows its approach: highlight
  words only when most of the line is unchanged.
- **SCIP and code indexers.** Considered for the ordering stage, then rejected. See
  [`adr/0015-language-abstraction.md`](adr/0015-language-abstraction.md).

## git

Every repository read shells out to the real `git` binary, plumbing commands only. The
byte-exactness guarantees were validated against git's own output and nothing else. See
[`adr/0002-shell-out-to-git.md`](adr/0002-shell-out-to-git.md).

## Crates

The terminal reviewer:

| crate | what it does here |
|---|---|
| [`ratatui`](https://crates.io/crates/ratatui) | The terminal UI framework. |
| [`crossterm`](https://crates.io/crates/crossterm) | Terminal control and key events. |
| [`syntect`](https://crates.io/crates/syntect) | Syntax highlighting. |
| [`two-face`](https://crates.io/crates/two-face) | Extra syntaxes and themes for syntect. |
| [`similar`](https://crates.io/crates/similar) | Line and word diffing inside a hunk. |
| [`tui-textarea-2`](https://crates.io/crates/tui-textarea-2) | The finding composer's text box. |
| [`arboard`](https://crates.io/crates/arboard) | Copying the findings summary to a local clipboard. |
| [`base64`](https://crates.io/crates/base64) | The OSC 52 payload, for a clipboard that is not local. |
| [`unicode-width`](https://crates.io/crates/unicode-width) | Correct column widths for wide characters. |

The engine and the application:

| crate | what it does here |
|---|---|
| [`serde`](https://crates.io/crates/serde) and [`serde_json`](https://crates.io/crates/serde_json) | The frozen JSON contract. |
| [`toml`](https://crates.io/crates/toml) | Reading both config files. |
| [`globset`](https://crates.io/crates/globset) | The `generated` and `not_generated` globs. |
| [`regex`](https://crates.io/crates/regex) | Diff parsing and shape normalisation. |
| [`sha1`](https://crates.io/crates/sha1) and [`hex`](https://crates.io/crates/hex) | Shape hashes, hunk digests, cache keys. |
| [`thiserror`](https://crates.io/crates/thiserror) and [`anyhow`](https://crates.io/crates/anyhow) | Error types and error context. |
| [`tempfile`](https://crates.io/crates/tempfile) | Temporary index files for git plumbing. |
| [`etcetera`](https://crates.io/crates/etcetera) | Finding the user config directory. |
| [`clap`](https://crates.io/crates/clap) | Argument parsing. |

## Tooling

- [`git-cliff`](https://git-cliff.org/) generates the changelog for each release from the
  commit subjects.

## Licence note

The MIT licence requires the upstream copyright notices to travel with the code. They are
in [`LICENSE-MIT`](LICENSE-MIT), alongside this project's own. If you vendor more code
into this repository, add its notice there and add a section here.
