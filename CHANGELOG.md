# Changelog

All notable changes to ExportCodex will be documented in this file. The project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Initial CLI for finding and exporting Codex chats.
- Markdown, HTML, and searchable PDF output.
- Cross-platform CI, release checksums, and Homebrew release automation.

### Fixed

- Recognize segmented Codex archive filenames without misidentifying their suffix UUID as the session ID.
- Reconstruct explicitly linked rollout segments while discarding superseded overlap.
- Parse the current completed-item message schema used by Codex Desktop and editor clients.
- Keep unrelated stale catalog diagnostics in `doctor` instead of printing them during successful exports.
- Keep long PDF running headers inside the printable page width.

[Unreleased]: https://github.com/rrainn/ExportCodex/commits/main
