//! Persistent user defaults and deterministic CLI override merging.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::ExportArgs;
use crate::domain::{MarginSize, OutputFormat, PaperSize, RenderOptions, ThemeName};

/// Optional values read from the user's configuration file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
	/// Default artifact format.
	pub format: Option<OutputFormat>,
	/// Directory used when `--output` is omitted.
	pub output_directory: Option<PathBuf>,
	/// Default visual theme.
	pub theme: Option<ThemeName>,
	/// Optional hexadecimal accent override.
	pub accent: Option<String>,
	/// Default PDF page size.
	pub paper: Option<PaperSize>,
	/// Default page margin density.
	pub margins: Option<MarginSize>,
	/// Default body size in points.
	pub font_size: Option<f32>,
	/// Show message timestamps by default.
	pub timestamps: Option<bool>,
	/// Include assistant progress updates by default.
	pub progress: Option<bool>,
	/// Include compact task metadata by default.
	pub metadata: Option<bool>,
	/// Trusted CSS file appended to HTML exports.
	pub css: Option<PathBuf>,
}

/// Fully resolved application preferences for one export.
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
	/// Selected concrete or aggregate format.
	pub format: OutputFormat,
	/// Optional default destination directory.
	pub output_directory: Option<PathBuf>,
	/// Shared renderer options.
	pub render: RenderOptions,
}

impl EffectiveConfig {
	/// Merges defaults, a file, and explicit CLI flags in increasing precedence.
	pub fn resolve(file: &FileConfig, args: &ExportArgs) -> Result<Self> {
		let defaults = RenderOptions::default();
		let format = args.format.or(file.format).unwrap_or(OutputFormat::Pdf);
		let accent = args.accent.clone().or_else(|| file.accent.clone());
		if let Some(color) = &accent {
			validate_accent(color)?;
		}

		let font_size = args
			.font_size
			.or(file.font_size)
			.unwrap_or(defaults.font_size);
		if !(8.0..=20.0).contains(&font_size) {
			bail!("font size must be between 8 and 20 points");
		}

		let includes_html = matches!(format, OutputFormat::Html | OutputFormat::All);
		if args.css.is_some() && !includes_html {
			bail!("--css applies only to HTML exports or --format all");
		}
		let custom_css = match (includes_html, args.css.as_deref().or(file.css.as_deref())) {
			(true, Some(path)) => Some(
				fs::read_to_string(path)
					.with_context(|| format!("could not read CSS file {}", path.display()))?,
			),
			_ => None,
		};
		let include_timestamps = resolve_boolean(
			"timestamps",
			args.timestamps,
			args.no_timestamps,
			file.timestamps,
			defaults.include_timestamps,
		)?;
		let include_progress = resolve_boolean(
			"progress",
			args.progress,
			args.no_progress,
			file.progress,
			defaults.include_progress,
		)?;
		let include_metadata = resolve_boolean(
			"metadata",
			args.metadata,
			args.no_metadata,
			file.metadata,
			defaults.include_metadata,
		)?;

		Ok(Self {
			format,
			output_directory: file.output_directory.clone(),
			render: RenderOptions {
				title: args.title.clone(),
				theme: args.theme.or(file.theme).unwrap_or(defaults.theme),
				accent,
				paper_size: args.paper.or(file.paper).unwrap_or(defaults.paper_size),
				margins: args.margins.or(file.margins).unwrap_or(defaults.margins),
				font_size,
				include_timestamps,
				include_progress,
				include_metadata,
				custom_css,
			},
		})
	}
}

fn resolve_boolean(
	name: &str,
	enabled: bool,
	disabled: bool,
	configured: Option<bool>,
	default: bool,
) -> Result<bool> {
	if enabled && disabled {
		bail!("--{name} and --no-{name} cannot be used together");
	}
	Ok(if enabled {
		true
	} else if disabled {
		false
	} else {
		configured.unwrap_or(default)
	})
}

/// Loads an explicit config or the platform default when it exists.
pub fn load(path: Option<&Path>) -> Result<FileConfig> {
	let explicit = path.is_some();
	let resolved = path.map(Path::to_path_buf).or_else(default_path);
	let Some(path) = resolved else {
		return Ok(FileConfig::default());
	};

	if !path.exists() && !explicit {
		return Ok(FileConfig::default());
	}

	let text = fs::read_to_string(&path)
		.with_context(|| format!("could not read config file {}", path.display()))?;
	toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))
}

/// Returns the normal per-user config path without creating directories.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
	dirs::config_dir().map(|directory| directory.join("export-codex").join("config.toml"))
}

fn validate_accent(color: &str) -> Result<()> {
	let valid = color.len() == 7
		&& color.starts_with('#')
		&& color[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
	if !valid {
		bail!("accent must use the #RRGGBB form");
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cli_flags_override_file_defaults() {
		let file = FileConfig {
			format: Some(OutputFormat::Markdown),
			theme: Some(ThemeName::Warm),
			progress: Some(true),
			..FileConfig::default()
		};
		let args = ExportArgs {
			format: Some(OutputFormat::Html),
			no_progress: true,
			..ExportArgs::default()
		};

		let effective = EffectiveConfig::resolve(&file, &args).expect("config should resolve");
		assert_eq!(effective.format, OutputFormat::Html);
		assert_eq!(effective.render.theme, ThemeName::Warm);
		assert!(!effective.render.include_progress);
	}

	#[test]
	fn invalid_accent_is_actionable() {
		let args = ExportArgs {
			accent: Some("blue".to_owned()),
			..ExportArgs::default()
		};
		let error = EffectiveConfig::resolve(&FileConfig::default(), &args)
			.expect_err("invalid colors should fail");
		assert!(error.to_string().contains("#RRGGBB"));
	}

	#[test]
	fn explicit_flags_can_reverse_saved_boolean_defaults() {
		let file = FileConfig {
			timestamps: Some(true),
			progress: Some(false),
			metadata: Some(false),
			..FileConfig::default()
		};
		let args = ExportArgs {
			no_timestamps: true,
			progress: true,
			metadata: true,
			..ExportArgs::default()
		};

		let effective = EffectiveConfig::resolve(&file, &args).expect("config should resolve");

		assert!(!effective.render.include_timestamps);
		assert!(effective.render.include_progress);
		assert!(effective.render.include_metadata);
	}

	#[test]
	fn pdf_export_does_not_read_configured_html_css() {
		let file = FileConfig {
			css: Some(PathBuf::from("missing-print-styles.css")),
			..FileConfig::default()
		};
		let args = ExportArgs {
			format: Some(OutputFormat::Pdf),
			..ExportArgs::default()
		};

		let effective = EffectiveConfig::resolve(&file, &args)
			.expect("PDF should ignore configured HTML styles");

		assert!(effective.render.custom_css.is_none());
	}

	#[test]
	fn explicit_css_rejects_non_html_exports() {
		let args = ExportArgs {
			format: Some(OutputFormat::Markdown),
			css: Some(PathBuf::from("print.css")),
			..ExportArgs::default()
		};

		let error = EffectiveConfig::resolve(&FileConfig::default(), &args)
			.expect_err("explicit CSS should not be silently ignored");

		assert!(error.to_string().contains("HTML exports"));
	}
}
