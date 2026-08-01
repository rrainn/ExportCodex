# Releasing ExportCodex

ExportCodex releases are built from an existing `v`-prefixed Git tag. GitHub
Actions verifies the source, builds four native binaries, packages reproducible
archives, tests the generated Homebrew formula, publishes the GitHub Release,
and then updates `rrainn/homebrew-tap`.

## Public Homebrew prerequisite

`rrainn/homebrew-tap` is currently private. Public users cannot run the normal
unauthenticated `brew tap rrainn/tap` or `brew install rrainn/tap/export-codex`
commands while that remains true.

Before advertising Homebrew as a public installation method, choose one of
these approaches:

1. Audit and migrate any private-only formula content, then make
   `rrainn/homebrew-tap` public.
2. Publish ExportCodex from a separate public tap, such as
   `rrainn/homebrew-exportcodex`, and update the workflow and installation docs
   to use `rrainn/exportcodex/export-codex`.

Do not make the existing tap public without reviewing the LabelTruth formula
and its history. Until the visibility decision is complete, only collaborators
with access to the private tap can install through it.

## One-time GitHub configuration

Create a fine-grained personal access token with these minimum permissions:

- Repository access: only `rrainn/homebrew-tap`.
- Repository permissions: Contents, read and write.
- No organization or account permissions.

Add it to the ExportCodex `release` environment as
`HOMEBREW_TAP_PAT`. Protecting that environment with required reviewers is
recommended. The token updates one formula through GitHub's Contents API and
authenticates the post-publication tap smoke test; it does not need access to
ExportCodex.

Protect `main` with the aggregate `CI / CI` check. Disallow force pushes and
branch deletion so a release tag always refers to reviewable history.

## Prepare a release

1. Ensure CI passes on `main`.
2. Update the package version in `Cargo.toml` and refresh `Cargo.lock`.
3. Move the relevant entries in `CHANGELOG.md` from Unreleased into a dated
   version section.
4. Commit the version and changelog changes to `main`.
5. Create and push an annotated tag whose version exactly matches Cargo:

   ```console
   git tag -a v0.1.0 -m "ExportCodex 0.1.0"
   git push origin v0.1.0
   ```

Pre-release identifiers are supported, for example `v0.2.0-rc.1`. Build
metadata such as `v0.2.0+build.1` is intentionally rejected because release
filenames should have one canonical version.

## What the workflow verifies

The release workflow performs these stages in order:

1. Confirms the tag is semantic versioning, matches `Cargo.toml`, and points to
   a commit contained in `origin/main`.
2. Runs formatting, Clippy with warnings denied, the Rust test suite, and the
   Homebrew renderer tests.
3. Builds native binaries for:

   - `aarch64-apple-darwin`
   - `x86_64-apple-darwin`
   - `aarch64-unknown-linux-musl`
   - `x86_64-unknown-linux-musl`

4. Packages all binaries on one Linux runner with normalized ordering,
   ownership, and timestamps. Each archive contains the executable, README,
   and license under one top-level directory.
5. Produces `SHA256SUMS` and renders `Formula/export-codex.rb` from those exact
   archives.
6. Installs and runs the local formula on every supported Homebrew architecture
   before publishing any release. The formula test discovers a synthetic Codex
   rollout by UUID and verifies visible user and agent text in a Markdown export.
7. Creates or repairs the GitHub Release and uploads the four archives plus
   `SHA256SUMS`.
8. Updates only `Formula/export-codex.rb` in the tap with optimistic concurrency.
9. Installs the published formula on macOS and Linux and runs `brew test`.

The workflow does not publish the crate to crates.io. That is a separate public
distribution decision.

## Local formula validation

The renderer uses only the Python standard library. Run its tests with:

```console
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

Render a syntax-checkable development formula with placeholder checksums:

```console
python3 scripts/render_homebrew_formula.py \
  --version 0.0.0 \
  --tag v0.0.0 \
  --darwin-arm64-sha256 0000000000000000000000000000000000000000000000000000000000000000 \
  --darwin-x64-sha256 1111111111111111111111111111111111111111111111111111111111111111 \
  --linux-arm64-sha256 2222222222222222222222222222222222222222222222222222222222222222 \
  --linux-x64-sha256 3333333333333333333333333333333333333333333333333333333333333333 \
  --output /tmp/export-codex.rb
ruby -c /tmp/export-codex.rb
```

## Repair or retry

The workflow is safe to rerun from **Actions → Release → Run workflow** with an
existing tag:

- Existing GitHub Release assets are replaced with the deterministically rebuilt
  versions.
- An identical tap formula is detected and skipped.
- A changed formula updates the current file SHA, preventing an accidental
  overwrite of a concurrent edit.

If the tap update fails after the GitHub Release succeeds, fix the token,
environment approval, or tap visibility problem and rerun the same tag. Do not
delete or move a published tag. If an artifact itself is wrong, publish a new
patch version instead.
