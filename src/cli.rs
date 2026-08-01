//! Command-line contract for interactive and non-interactive exports.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::domain::{MarginSize, OutputFormat, PaperSize, ThemeName};

/// Export local Codex chats as polished, portable documents.
#[derive(Debug, Parser)]
#[command(
	name = "export-codex",
	version,
	about,
	long_about = "Export a Codex Desktop chat by thread ID, codex:// deeplink, rollout path, or an interactive terminal browser. PDF output uses embedded text, so its contents remain searchable and copyable."
)]
pub struct Cli {
	/// Override the Codex data directory (defaults to CODEX_HOME, then ~/.codex).
	#[arg(long, global = true, env = "CODEX_HOME", value_name = "DIR")]
	pub codex_home: Option<PathBuf>,

	/// Load persistent defaults from this TOML file.
	#[arg(long, global = true, value_name = "FILE")]
	pub config: Option<PathBuf>,

	/// Suppress successful export messages.
	#[arg(long, global = true)]
	pub quiet: bool,

	/// Optional utility command; without one, the CLI exports directly.
	#[command(subcommand)]
	pub command: Option<Command>,

	/// Direct-export options used when no subcommand is supplied.
	#[command(flatten)]
	pub export: ExportArgs,
}

/// Utility commands that complement the direct export path.
#[derive(Debug, Subcommand)]
pub enum Command {
	/// Export a selected chat; equivalent to the top-level positional form.
	Export(ExportArgs),
	/// Open the full-screen task browser and export the selected chat.
	Browse(BrowseArgs),
	/// List locally stored tasks for scripts or quick inspection.
	List(ListArgs),
	/// Show the built-in visual themes.
	Themes,
	/// Check Codex storage and renderer availability.
	Doctor,
}

/// Options shared by direct and browser-driven exports.
#[derive(Debug, Clone, Default, Args)]
pub struct ExportArgs {
	/// Thread UUID, `codex://threads/<id>` deeplink, or rollout JSONL path.
	#[arg(value_name = "THREAD")]
	pub selector: Option<String>,

	/// Output kind; defaults to PDF unless configured otherwise.
	#[arg(short, long, value_enum)]
	pub format: Option<OutputFormat>,

	/// File path for one format, or directory when --format all is used.
	#[arg(short, long, value_name = "PATH")]
	pub output: Option<PathBuf>,

	/// Replace an existing output file instead of protecting it.
	#[arg(long)]
	pub force: bool,

	/// Replace the Codex task title in the exported document.
	#[arg(long, value_name = "TEXT")]
	pub title: Option<String>,

	/// Built-in visual theme.
	#[arg(long, value_enum)]
	pub theme: Option<ThemeName>,

	/// Accent color as #RRGGBB.
	#[arg(long, value_name = "COLOR")]
	pub accent: Option<String>,

	/// PDF and print page size.
	#[arg(long, value_enum)]
	pub paper: Option<PaperSize>,

	/// Page margin density.
	#[arg(long, value_enum)]
	pub margins: Option<MarginSize>,

	/// Base body size in points (8–20).
	#[arg(long, value_name = "POINTS")]
	pub font_size: Option<f32>,

	/// Show timestamps beside each transcript entry.
	#[arg(long, conflicts_with = "no_timestamps")]
	pub timestamps: bool,

	/// Hide timestamps even when enabled in the config file.
	#[arg(long, conflicts_with = "timestamps")]
	pub no_timestamps: bool,

	/// Include intermediate Codex progress updates.
	#[arg(long, conflicts_with = "no_progress")]
	pub progress: bool,

	/// Omit intermediate Codex progress updates.
	#[arg(long, conflicts_with = "progress")]
	pub no_progress: bool,

	/// Include the compact conversation metadata header.
	#[arg(long, conflicts_with = "no_metadata")]
	pub metadata: bool,

	/// Omit the compact conversation metadata header.
	#[arg(long, conflicts_with = "metadata")]
	pub no_metadata: bool,

	/// Append trusted CSS to self-contained HTML output.
	#[arg(long, value_name = "FILE")]
	pub css: Option<PathBuf>,
}

/// Browser-specific starting filters plus normal export options.
#[derive(Debug, Clone, Args)]
pub struct BrowseArgs {
	/// Start on archived tasks rather than active tasks.
	#[arg(long)]
	pub archived: bool,

	/// Include spawned agent tasks in the browser.
	#[arg(long)]
	pub agents: bool,

	/// Options applied after a task is selected.
	#[command(flatten)]
	pub export: ExportArgs,
}

/// Script-friendly catalog listing filters.
#[derive(Debug, Clone, Args)]
pub struct ListArgs {
	/// Show archived tasks instead of active tasks.
	#[arg(long)]
	pub archived: bool,

	/// Include spawned agent tasks.
	#[arg(long)]
	pub agents: bool,

	/// Emit a stable JSON array instead of a terminal table.
	#[arg(long)]
	pub json: bool,

	/// Restrict output to tasks matching this text.
	#[arg(short, long, value_name = "TEXT")]
	pub search: Option<String>,

	/// Maximum rows to print.
	#[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=1000))]
	pub limit: u16,
}

#[cfg(test)]
mod tests {
	use clap::Parser;

	use super::*;

	#[test]
	fn direct_deeplink_keeps_simple_export_ux() {
		let cli = Cli::try_parse_from([
			"export-codex",
			"codex://threads/019f874c-1e53-7d12-97db-fac3249ce327",
			"--format",
			"html",
		])
		.expect("direct export arguments should parse");

		assert_eq!(cli.export.format, Some(OutputFormat::Html));
		assert!(cli.export.selector.is_some());
	}

	#[test]
	fn explicit_export_command_uses_same_options() {
		let cli = Cli::try_parse_from([
			"export-codex",
			"export",
			"019f874c-1e53-7d12-97db-fac3249ce327",
			"--format",
			"all",
		])
		.expect("export subcommand should parse");

		let Some(Command::Export(args)) = cli.command else {
			panic!("expected the export subcommand");
		};
		assert_eq!(args.format, Some(OutputFormat::All));
	}
}
