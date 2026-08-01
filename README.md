# ExportCodex

ExportCodex turns local Codex Desktop chats into polished Markdown, self-contained HTML, and searchable PDF documents. Give it a thread ID or a `codex://` deeplink for the fastest path, or use the terminal browser when you only remember what a task was about.

```console
export-codex codex://threads/019f874c-1e53-7d12-97db-fac3249ce327
```

That creates a print-ready PDF such as `build-a-release-pipeline-019f874c.pdf`. The PDF contains real embedded text rather than page screenshots, so conversation text and code remain selectable, searchable, and copyable.

> [!NOTE]
> ExportCodex is an independent project and is not affiliated with or endorsed by OpenAI.

## What it does

- Selects chats by technical thread ID, canonical Codex deeplink, rollout file, or a full-screen terminal browser.
- Finds active and archived Desktop tasks without making users move or unarchive them.
- Keeps the initial browser clean by hiding archived and spawned-agent tasks until requested.
- Exports Markdown, a self-contained responsive HTML document, a selectable-text PDF, or all three at once.
- Uses restrained print typography, semantic headings, code panels, page numbers, and sensible page breaks by default.
- Offers themes, accent color, title, paper size, margins, font size, timestamps, progress-message, metadata, and custom-CSS controls.
- Reads local Codex data without modifying chats or contacting a server.

## Install

### Homebrew

The release workflow publishes native macOS and Linux binaries and updates `rrainn/homebrew-tap`:

```console
brew install rrainn/tap/export-codex
```

The existing `rrainn/homebrew-tap` repository must be made public before this works for unauthenticated public users. Maintainers should complete the one-time visibility and token setup in [RELEASING.md](RELEASING.md) before advertising the first release.

### From source

ExportCodex requires Rust 1.88 or newer:

```console
cargo install --git https://github.com/rrainn/ExportCodex --locked
```

You can also download a platform archive and `SHA256SUMS` from [GitHub Releases](https://github.com/rrainn/ExportCodex/releases) after the first tagged release.

## Quick start

Export directly from a copied Codex link:

```console
export-codex 'codex://threads/019f874c-1e53-7d12-97db-fac3249ce327'
```

Export with the raw thread ID:

```console
export-codex 019f874c-1e53-7d12-97db-fac3249ce327
```

Open the interactive browser:

```console
export-codex
```

Produce every format in an `exports` directory:

```console
export-codex 019f874c-1e53-7d12-97db-fac3249ce327 \
  --format all \
  --output exports
```

Export a single HTML document to standard output:

```console
export-codex 019f874c-1e53-7d12-97db-fac3249ce327 \
  --format html \
  --output - > chat.html
```

Existing output files are protected. Pass `--force` only when replacing one is intentional.

## Finding the right chat

Codex Desktop’s canonical local-chat link is `codex://threads/<thread-id>` ([Codex deep-link reference](https://developers.openai.com/codex/app/commands#deep-links)). In Desktop, copy the task link and pass it directly to ExportCodex. The UUID portion is also accepted on its own.

When no selector is supplied, the browser opens with recent active, top-level tasks:

| Key | Action |
| --- | --- |
| `↑` / `↓` or `j` / `k` | Move through tasks |
| `Enter` | Export the selected task |
| `/` | Search title, preview, workspace, or ID |
| `a` | Cycle Active, Archived, and All views |
| `g` | Show or hide spawned-agent tasks |
| `q` or `Esc` | Exit |

Direct IDs and deeplinks always resolve archived and agent tasks even when those categories are hidden in the initial browser.

For a script-friendly catalog:

```console
export-codex list
export-codex list --archived --search release
export-codex list --agents --json
```

## Formats

### PDF

PDF is the default because it is ready to print and share. ExportCodex renders it locally through a pure-Rust typesetting pipeline with embedded text, safe clickable links, document metadata, page numbers, Unicode text, code styling, and Letter or A4 layouts. Authored LaTeX stays literal instead of becoming non-selectable vector artwork. It does not require Chrome, Python, LaTeX, or a network connection.

### HTML

HTML exports are one self-contained file with semantic markup, responsive screen styles, and a separate print stylesheet. Message Markdown is rendered locally; raw HTML from the chat is escaped. No scripts, remote fonts, or external stylesheets are added.

### Markdown

Markdown exports are deliberately portable. They use ordinary headings, blockquotes, tables, code fences, and links so the result remains useful in GitHub, editors, static-site tools, and future conversion pipelines.

## Customize an export

```console
export-codex 019f874c-1e53-7d12-97db-fac3249ce327 \
  --format pdf \
  --title "Release engineering notes" \
  --theme warm \
  --accent '#A1492D' \
  --paper a4 \
  --margins wide \
  --font-size 11.5 \
  --timestamps \
  --no-progress
```

Useful options:

| Option | Values or behavior |
| --- | --- |
| `--format` | `pdf` (default), `html`, `markdown`, or `all` |
| `--theme` | `paper` (default), `warm`, or `mono` |
| `--title TEXT` | Replaces the Codex task title in the document |
| `--accent '#RRGGBB'` | Overrides the theme accent |
| `--paper` | `letter` (default) or `a4` |
| `--margins` | `compact`, `normal` (default), or `wide` |
| `--font-size POINTS` | Body size from 8 through 20 |
| `--timestamps` / `--no-timestamps` | Shows or hides timestamps, overriding saved defaults |
| `--progress` / `--no-progress` | Includes or omits intermediate commentary |
| `--metadata` / `--no-metadata` | Includes or removes the compact task metadata header |
| `--css FILE` | Appends trusted CSS to HTML output |
| `--output PATH` | One file, or a directory with `--format all` |
| `--force` | Allows an existing artifact to be replaced |

Run `export-codex themes` for a one-line description of each built-in theme, or `export-codex --help` for the full command contract.

## Persistent defaults

ExportCodex reads an optional `export-codex/config.toml` below the normal platform config directory. You can always use an explicit file with `--config PATH`.

```toml
# PDF stays the one-command default.
format = "pdf"
output_directory = "./exports"
theme = "paper"
accent = "#2F5D8A"
paper = "letter"
margins = "normal"
font_size = 11.0
timestamps = false
progress = true
metadata = true

# This file is trusted and is only used for HTML output.
# css = "./print-overrides.css"
```

Command-line flags override values from the config file. Unknown config keys and invalid colors or font sizes fail with an actionable error instead of being silently ignored.

## Codex data and privacy

The data directory resolves in this order:

1. `--codex-home DIR`
2. `CODEX_HOME`
3. `~/.codex`

ExportCodex opens the root Codex state catalog read-only and falls back to scanning `sessions` and `archived_sessions` rollout files when needed. It understands both `.jsonl` and `.jsonl.zst` histories and takes a bounded snapshot when a task is still being written.

Only user-visible `user_message` and `agent_message` events enter the normal transcript. Codex rollout files also contain model-facing developer instructions, environment context, and duplicated response records; ExportCodex intentionally does not render those records. This both avoids duplicates and prevents hidden context from leaking into a shared export. Image attachments are represented by a count-only placeholder so image-only prompts are not silently lost, while local paths and image data are never copied into the document.

Message bodies and catalog strings are treated as untrusted input. Raw HTML is escaped, unsafe link schemes are neutralized, terminal control characters are stripped, remote assets are not fetched, and malformed trailing records produce a warning rather than corrupting an otherwise usable export. Records larger than 8 MiB are skipped with a warning so hidden tool output and compressed inputs cannot cause unbounded per-record memory use.

## Development

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo doc --workspace --all-features --no-deps --locked
```

The test suite covers selector parsing, active and archived discovery, schema fallbacks, bounded JSONL parsing, hidden-context exclusion, attachment placeholders, terminal filtering, output collision protection, Markdown boundary safety, HTML safety, selectable PDF generation, and CLI behavior. GitHub Actions runs formatting, linting, documentation, and tests across Linux, macOS, and Windows. A separate release workflow builds and smoke-tests all four Homebrew platforms before publishing.

See [RELEASING.md](RELEASING.md) for release and tap administration, and [CHANGELOG.md](CHANGELOG.md) for user-facing changes.

## License

[MIT](LICENSE) © 2026 rrainn
