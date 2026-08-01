//! Output naming, rendering dispatch, and protected atomic file writes.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

use crate::domain::{Chat, OutputFormat, RenderOptions};
use crate::render;
use crate::render::theme::clean_catalog_title;

/// One fully rendered artifact before it is written to its destination.
#[derive(Debug)]
pub struct RenderedArtifact {
	/// Concrete output kind.
	pub format: OutputFormat,
	/// Encoded document bytes.
	pub bytes: Vec<u8>,
}

/// Renders every concrete artifact requested by a format selection.
pub fn render_artifacts(
	chat: &Chat,
	format: OutputFormat,
	options: &RenderOptions,
) -> Result<Vec<RenderedArtifact>> {
	format
		.concrete()
		.into_iter()
		.map(|format| {
			let bytes = match format {
				OutputFormat::Markdown => render::markdown::render(chat, options).into_bytes(),
				OutputFormat::Html => render::html::render(chat, options).into_bytes(),
				OutputFormat::Pdf => render::pdf::render(chat, options)
					.context("could not render selectable-text PDF")?,
				OutputFormat::All => {
					unreachable!("aggregate formats are expanded before rendering")
				}
			};
			Ok(RenderedArtifact { format, bytes })
		})
		.collect()
}

/// Resolves all destinations before rendering so collisions fail without partial output.
pub fn plan_paths(
	chat: &Chat,
	format: OutputFormat,
	explicit: Option<&Path>,
	default_directory: Option<&Path>,
	force: bool,
) -> Result<Vec<(OutputFormat, PathBuf)>> {
	let formats = format.concrete();
	if explicit == Some(Path::new("-")) {
		if formats.len() != 1 || formats[0] == OutputFormat::Pdf {
			bail!("stdout output is available only for one Markdown or HTML export");
		}
		return Ok(vec![(formats[0], PathBuf::from("-"))]);
	}

	let base_name = output_stem(chat);
	let multiple = formats.len() > 1;
	let paths = formats
		.into_iter()
		.map(|format| {
			let extension = format
				.extension()
				.expect("concrete formats always have extensions");
			let generated = format!("{base_name}.{extension}");
			let path = match explicit {
				Some(path) if multiple || path.is_dir() => path.join(generated),
				Some(path) => path.to_path_buf(),
				None => default_directory
					.map(Path::to_path_buf)
					.unwrap_or_else(|| PathBuf::from("."))
					.join(generated),
			};
			(format, path)
		})
		.collect::<Vec<_>>();

	if !force {
		for (_, path) in &paths {
			if path.exists() {
				bail!(
					"refusing to replace {}; pass --force to overwrite it",
					path.display()
				);
			}
		}
	}

	Ok(paths)
}

/// Writes rendered artifacts to a preflighted path plan.
pub fn write_artifacts(
	artifacts: Vec<RenderedArtifact>,
	paths: &[(OutputFormat, PathBuf)],
	force: bool,
) -> Result<Vec<PathBuf>> {
	if artifacts.len() != paths.len() {
		bail!("internal output plan did not match the rendered artifact count");
	}

	let mut written = Vec::with_capacity(artifacts.len());
	for (artifact, (format, path)) in artifacts.into_iter().zip(paths) {
		if artifact.format != *format {
			bail!("internal output plan changed format order");
		}
		if path == Path::new("-") {
			let mut stdout = io::stdout().lock();
			stdout
				.write_all(&artifact.bytes)
				.context("could not write export to stdout")?;
			stdout.flush().context("could not flush export to stdout")?;
			written.push(path.clone());
			continue;
		}

		write_atomic(path, &artifact.bytes, force)?;
		written.push(path.clone());
	}
	Ok(written)
}

fn write_atomic(path: &Path, bytes: &[u8], force: bool) -> Result<()> {
	let parent = path
		.parent()
		.filter(|path| !path.as_os_str().is_empty())
		.unwrap_or(Path::new("."));
	fs::create_dir_all(parent)
		.with_context(|| format!("could not create output directory {}", parent.display()))?;

	let mut temporary = NamedTempFile::new_in(parent)
		.with_context(|| format!("could not create a temporary file in {}", parent.display()))?;
	temporary
		.write_all(bytes)
		.with_context(|| format!("could not write temporary export for {}", path.display()))?;
	temporary
		.as_file()
		.sync_all()
		.with_context(|| format!("could not sync temporary export for {}", path.display()))?;

	if force {
		temporary
			.persist(path)
			.map_err(|error| error.error)
			.with_context(|| format!("could not replace {}", path.display()))?;
	} else {
		temporary
			.persist_noclobber(path)
			.map_err(|error| error.error)
			.with_context(|| format!("could not create {}", path.display()))?;
	}
	Ok(())
}

fn output_stem(chat: &Chat) -> String {
	let mut slug = String::new();
	let mut previous_dash = false;
	for character in clean_catalog_title(chat.thread.display_title()).chars() {
		if character.is_ascii_alphanumeric() {
			slug.push(character.to_ascii_lowercase());
			previous_dash = false;
		} else if !previous_dash && !slug.is_empty() {
			slug.push('-');
			previous_dash = true;
		}
		if slug.len() >= 56 {
			break;
		}
	}
	let slug = slug.trim_matches('-');
	let slug = if slug.is_empty() { "codex-chat" } else { slug };
	format!("{slug}-{}", chat.thread.id.short())
}

#[cfg(test)]
mod tests {
	use std::str::FromStr;

	use crate::domain::ThreadId;

	use super::*;

	fn sample_chat() -> Chat {
		Chat {
			thread: crate::domain::ThreadSummary {
				id: ThreadId::from_str("019f874c-1e53-7d12-97db-fac3249ce327")
					.expect("fixture UUID should parse"),
				title: "A careful export: Markdown & PDF".to_owned(),
				preview: String::new(),
				cwd: PathBuf::from("/tmp/project"),
				rollout_path: PathBuf::from("chat.jsonl"),
				created_at: None,
				updated_at: None,
				archived: false,
				is_agent: false,
				history_mode: "legacy".to_owned(),
				model: None,
			},
			messages: Vec::new(),
			warnings: Vec::new(),
		}
	}

	#[test]
	fn all_format_uses_one_stable_stem() {
		let paths = plan_paths(
			&sample_chat(),
			OutputFormat::All,
			Some(Path::new("exports")),
			None,
			false,
		)
		.expect("path plan should succeed");

		assert_eq!(paths.len(), 3);
		assert_eq!(
			paths[0].1,
			PathBuf::from("exports/a-careful-export-markdown-pdf-019f874c.md")
		);
		assert_eq!(
			paths[2].1,
			PathBuf::from("exports/a-careful-export-markdown-pdf-019f874c.pdf")
		);
	}

	#[test]
	fn pdf_cannot_be_streamed_to_terminal_bytes() {
		let error = plan_paths(
			&sample_chat(),
			OutputFormat::Pdf,
			Some(Path::new("-")),
			None,
			false,
		)
		.expect_err("PDF stdout should be rejected");
		assert!(error.to_string().contains("Markdown or HTML"));
	}
}
