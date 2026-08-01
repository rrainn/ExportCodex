//! Binary entry point for the ExportCodex command-line application.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use export_codex::cli::{BrowseArgs, Cli, Command, ExportArgs, ListArgs};
use export_codex::codex::{
	ArchiveFilter, Catalog, CatalogFilter, ThreadSelector, parse_chat, resolve_codex_home,
};
use export_codex::config::{self, EffectiveConfig, FileConfig};
use export_codex::domain::{
	Chat, ChatMessage, MessagePhase, MessageRole, RenderOptions, ThreadId, ThreadSummary,
};
use export_codex::output::{plan_paths, render_artifacts, write_artifacts};
use export_codex::tui::app::{BrowserOptions, select_thread};
use serde_json::json;

fn main() -> ExitCode {
	match run() {
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("error: {error:#}");
			ExitCode::FAILURE
		}
	}
}

fn run() -> Result<()> {
	let cli = Cli::parse();
	let home_override = cli.codex_home.clone();
	let config_path = cli.config.clone();
	let quiet = cli.quiet;

	match cli.command {
		Some(Command::Themes) => show_themes(),
		Some(Command::Doctor) => doctor(home_override.as_deref()),
		Some(Command::List(args)) => list_threads(home_override.as_deref(), &args),
		Some(Command::Browse(args)) => {
			let file = config::load(config_path.as_deref())?;
			browse_and_export(home_override.as_deref(), &file, &args, quiet)
		}
		Some(Command::Export(args)) => {
			let file = config::load(config_path.as_deref())?;
			export_or_browse(home_override.as_deref(), &file, &args, quiet)
		}
		None => {
			let file = config::load(config_path.as_deref())?;
			export_or_browse(home_override.as_deref(), &file, &cli.export, quiet)
		}
	}
}

fn export_or_browse(
	home_override: Option<&Path>,
	file: &FileConfig,
	args: &ExportArgs,
	quiet: bool,
) -> Result<()> {
	if let Some(selector) = &args.selector {
		return export_selected(home_override, file, args, selector, quiet);
	}

	if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
		bail!(
			"a thread ID, codex:// deeplink, or rollout path is required when no interactive terminal is available"
		);
	}

	browse_and_export(
		home_override,
		file,
		&BrowseArgs {
			archived: false,
			agents: false,
			export: args.clone(),
		},
		quiet,
	)
}

fn browse_and_export(
	home_override: Option<&Path>,
	file: &FileConfig,
	args: &BrowseArgs,
	quiet: bool,
) -> Result<()> {
	if let Some(selector) = &args.export.selector {
		return export_selected(home_override, file, &args.export, selector, quiet);
	}

	let home = resolve_codex_home(home_override)?;
	let catalog = Catalog::load(home)?;
	print_catalog_warnings(&catalog);
	let threads = catalog.list(CatalogFilter {
		archive: ArchiveFilter::All,
		include_agents: true,
	});
	if threads.is_empty() {
		bail!("no local Codex tasks were found");
	}

	let selected = select_thread(
		threads,
		BrowserOptions {
			archived: args.archived,
			include_agents: args.agents,
		},
	)?;
	let Some(thread) = selected else {
		return Ok(());
	};
	export_thread(thread, file, &args.export, quiet)
}

fn export_selected(
	home_override: Option<&Path>,
	file: &FileConfig,
	args: &ExportArgs,
	selector: &str,
	quiet: bool,
) -> Result<()> {
	let home = resolve_codex_home(home_override)?;
	let catalog = Catalog::load(home)?;
	print_catalog_warnings(&catalog);
	let selector = ThreadSelector::from_str(selector).context("invalid thread selector")?;
	let thread = catalog.resolve(&selector)?;
	export_thread(thread, file, args, quiet)
}

fn export_thread(
	thread: ThreadSummary,
	file: &FileConfig,
	args: &ExportArgs,
	quiet: bool,
) -> Result<()> {
	let effective = EffectiveConfig::resolve(file, args)?;
	let chat = parse_chat(thread)?;
	for warning in &chat.warnings {
		eprintln!("warning: {warning}");
	}

	let paths = plan_paths(
		&chat,
		effective.format,
		args.output.as_deref(),
		effective.output_directory.as_deref(),
		args.force,
	)?;
	let artifacts = render_artifacts(&chat, effective.format, &effective.render)?;
	let written = write_artifacts(artifacts, &paths, args.force)?;

	if !quiet {
		for path in written {
			if path != PathBuf::from("-") {
				eprintln!("Exported {}", path.display());
			}
		}
	}
	Ok(())
}

fn list_threads(home_override: Option<&Path>, args: &ListArgs) -> Result<()> {
	let home = resolve_codex_home(home_override)?;
	let catalog = Catalog::load(home)?;
	print_catalog_warnings(&catalog);
	let archive = if args.archived {
		ArchiveFilter::Archived
	} else {
		ArchiveFilter::Active
	};
	let query = args.search.as_deref().map(str::to_lowercase);
	let threads = catalog
		.list(CatalogFilter {
			archive,
			include_agents: args.agents,
		})
		.into_iter()
		.filter(|thread| {
			query.as_ref().is_none_or(|query| {
				[
					thread.display_title().to_owned(),
					thread.preview.clone(),
					thread.cwd.display().to_string(),
					thread.id.to_string(),
				]
				.into_iter()
				.any(|value| value.to_lowercase().contains(query))
			})
		})
		.take(usize::from(args.limit))
		.collect::<Vec<_>>();

	if args.json {
		let rows = threads
			.iter()
			.map(|thread| {
				json!({
					"id": thread.id.to_string(),
					"title": thread.display_title(),
					"preview": thread.preview,
					"cwd": thread.cwd,
					"created_at": thread.created_at,
					"updated_at": thread.updated_at,
					"archived": thread.archived,
					"agent_task": thread.is_agent,
					"model": thread.model,
				})
			})
			.collect::<Vec<_>>();
		println!("{}", serde_json::to_string_pretty(&rows)?);
		return Ok(());
	}

	if threads.is_empty() {
		println!("No matching Codex tasks.");
		return Ok(());
	}
	for thread in threads {
		let date = thread
			.updated_at
			.map(|value| value.format("%Y-%m-%d").to_string())
			.unwrap_or_else(|| "----------".to_owned());
		println!("{date}  {}  {}", thread.id, thread.display_title());
	}
	Ok(())
}

fn show_themes() -> Result<()> {
	println!("paper  Warm white, charcoal text, and a restrained blue accent (default)");
	println!("warm   Ivory paper, deep green text, and a terracotta accent");
	println!("mono   Neutral grayscale for dense technical transcripts");
	Ok(())
}

fn doctor(home_override: Option<&Path>) -> Result<()> {
	let home = resolve_codex_home(home_override)?;
	let catalog = Catalog::load(home.clone())?;
	verify_pdf_renderer()?;
	let active = catalog
		.list(CatalogFilter {
			archive: ArchiveFilter::Active,
			include_agents: false,
		})
		.len();
	let archived = catalog
		.list(CatalogFilter {
			archive: ArchiveFilter::Archived,
			include_agents: false,
		})
		.len();

	println!("Codex home    {}", home.display());
	println!("Active tasks  {active}");
	println!("Archived      {archived}");
	println!("PDF renderer  ready (pure Rust, selectable text)");
	if catalog.warnings().is_empty() {
		println!("Catalog       healthy");
	} else {
		println!(
			"Catalog       ready with {} warning(s)",
			catalog.warnings().len()
		);
		print_catalog_warnings(&catalog);
	}
	Ok(())
}

fn verify_pdf_renderer() -> Result<()> {
	// Rendering a real in-memory page catches font and packaging failures.
	let id = ThreadId::from_str("00000000-0000-4000-8000-000000000001")
		.context("invalid built-in renderer probe ID")?;
	let chat = Chat {
		thread: ThreadSummary {
			id,
			title: "Renderer check".to_owned(),
			preview: String::new(),
			cwd: PathBuf::from("doctor"),
			rollout_path: PathBuf::new(),
			created_at: None,
			updated_at: None,
			archived: false,
			is_agent: false,
			history_mode: "probe".to_owned(),
			model: None,
		},
		messages: vec![ChatMessage {
			role: MessageRole::Assistant,
			phase: MessagePhase::FinalAnswer,
			text: "Selectable PDF text.".to_owned(),
			timestamp: None,
		}],
		warnings: Vec::new(),
	};
	let bytes = export_codex::render::pdf::render(&chat, &RenderOptions::default())
		.context("PDF renderer self-test failed")?;
	if !bytes.starts_with(b"%PDF-") {
		bail!("PDF renderer self-test returned an invalid document");
	}
	Ok(())
}

fn print_catalog_warnings(catalog: &Catalog) {
	for warning in catalog.warnings() {
		eprintln!("warning: {warning}");
	}
}
