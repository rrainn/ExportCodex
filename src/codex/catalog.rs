//! Read-only catalog discovery and filesystem reconciliation.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};
use walkdir::WalkDir;

use super::jsonl::{SessionMetadata, SnapshotRecord, sanitize_text, visit_snapshot_records};
use super::rollout::{is_rollout_file, rollout_ids_from_filename};
use super::selector::ThreadSelector;
use crate::domain::{MessageRole, ThreadId, ThreadSummary};

/// Selects which archive state appears in the interactive task list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArchiveFilter {
	/// Show the uncluttered set of tasks still active in Codex.
	#[default]
	Active,
	/// Show only tasks that Codex has archived.
	Archived,
	/// Show both active and archived tasks.
	All,
}

/// Filters applied to the local task catalog.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatalogFilter {
	/// Archive state to include.
	pub archive: ArchiveFilter,
	/// Whether spawned and system-owned agent tasks should be visible.
	pub include_agents: bool,
}

/// A read-only snapshot of locally available Codex tasks.
#[derive(Debug, Clone)]
pub struct Catalog {
	home: PathBuf,
	threads: Vec<ThreadSummary>,
	warnings: Vec<String>,
}

impl Catalog {
	/// Loads the newest root-level Codex state database and reconciles rollout files.
	///
	/// Codex has historically left stale databases in a `sqlite` subdirectory, so
	/// discovery deliberately examines only `state_*.sqlite` beside the session roots.
	///
	/// # Errors
	///
	/// Returns an error when the Codex home is missing or cannot be inspected. An
	/// unreadable or incompatible state database falls back to rollout discovery.
	pub fn load(home: impl Into<PathBuf>) -> Result<Self> {
		let home = home.into();
		if !home.is_dir() {
			bail!(
				"Codex home does not exist or is not a directory: {}",
				home.display()
			);
		}

		let mut warnings = Vec::new();
		let mut database_threads = Vec::new();
		match discover_state_database(&home)? {
			Some(database) => match load_database(&database, &home) {
				Ok((threads, database_warnings)) => {
					database_threads = threads;
					warnings.extend(database_warnings);
				}
				Err(error) => warnings.push(format!(
					"Could not read {} ({error:#}); discovered tasks from rollout files instead.",
					database.display()
				)),
			},
			None => warnings.push(
				"No root Codex state database was found; discovered tasks from rollout files."
					.to_owned(),
			),
		}

		let (locations, discovery_warnings) = discover_rollouts(&home);
		warnings.extend(discovery_warnings);
		let threads = reconcile_threads(&home, database_threads, locations, &mut warnings);

		Ok(Self {
			home,
			threads,
			warnings,
		})
	}

	/// Returns the resolved Codex home used by this snapshot.
	#[must_use]
	pub fn home(&self) -> &Path {
		&self.home
	}

	/// Returns non-fatal compatibility and discovery warnings.
	#[must_use]
	pub fn warnings(&self) -> &[String] {
		&self.warnings
	}

	/// Returns newest-first tasks matching the interactive browser filters.
	#[must_use]
	pub fn list(&self, filter: CatalogFilter) -> Vec<ThreadSummary> {
		self.threads
			.iter()
			.filter(|thread| match filter.archive {
				ArchiveFilter::Active => !thread.archived,
				ArchiveFilter::Archived => thread.archived,
				ArchiveFilter::All => true,
			})
			.filter(|thread| filter.include_agents || !thread.is_agent)
			.cloned()
			.collect()
	}

	/// Resolves a direct selector across active and archived storage.
	///
	/// Direct selection intentionally ignores browser filters because a UUID or
	/// deeplink is an explicit request, including when Codex archived that task.
	///
	/// # Errors
	///
	/// Returns an error when no matching rollout exists or its metadata is invalid.
	pub fn resolve(&self, selector: &ThreadSelector) -> Result<ThreadSummary> {
		match selector {
			ThreadSelector::Id(id) => {
				if let Some(thread) = self.threads.iter().find(|thread| thread.id == *id) {
					return Ok(thread.clone());
				}

				let (locations, _) = discover_rollouts(&self.home);
				let location =
					select_location(locations.iter().filter(|location| location.id == *id), None)
						.ok_or_else(|| anyhow!("no local Codex chat found for session {id}"))?;
				let (summary, _) = summarize_rollout(&self.home, &location.path, Some(*id))?;
				Ok(summary)
			}
			ThreadSelector::Path(path) => {
				let path = resolve_direct_path(&self.home, path);
				if !path.is_file() {
					bail!("Codex rollout does not exist: {}", path.display());
				}
				if !is_rollout_file(&path) {
					bail!("unsupported Codex rollout format: {}", path.display());
				}

				let filename_id = rollout_ids_from_filename(&path).map(|ids| ids.session);
				let (summary, _) = summarize_rollout(&self.home, &path, filename_id)?;
				Ok(summary)
			}
		}
	}
}

/// Chooses the highest state schema number without recursing into stale locations.
fn discover_state_database(home: &Path) -> Result<Option<PathBuf>> {
	let entries = fs::read_dir(home)
		.with_context(|| format!("could not inspect Codex home {}", home.display()))?;
	let mut candidates = Vec::new();
	for entry in entries {
		let entry = entry.with_context(|| format!("could not inspect {}", home.display()))?;
		let path = entry.path();
		if !path.is_file() {
			continue;
		}
		let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
			continue;
		};
		let Some(version) = state_database_version(name) else {
			continue;
		};
		let modified = entry
			.metadata()
			.and_then(|metadata| metadata.modified())
			.unwrap_or(SystemTime::UNIX_EPOCH);
		candidates.push((version, modified, path));
	}

	candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
	Ok(candidates.pop().map(|(_, _, path)| path))
}

fn state_database_version(name: &str) -> Option<u64> {
	name.strip_prefix("state_")?
		.strip_suffix(".sqlite")?
		.parse()
		.ok()
}

/// Reads only columns present in the installed Codex schema.
fn load_database(path: &Path, home: &Path) -> Result<(Vec<ThreadSummary>, Vec<String>)> {
	let connection = Connection::open_with_flags(
		path,
		OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
	)
	.with_context(|| format!("could not open Codex state database {}", path.display()))?;
	connection
		.busy_timeout(Duration::from_millis(500))
		.context("could not configure the Codex database read timeout")?;

	let columns = thread_columns(&connection)?;
	if !columns.contains("id") {
		bail!("the Codex threads table has no id column");
	}

	let selected = [
		"id",
		"rollout_path",
		"title",
		"preview",
		"first_user_message",
		"cwd",
		"created_at_ms",
		"created_at",
		"updated_at_ms",
		"updated_at",
		"recency_at_ms",
		"recency_at",
		"archived",
		"agent_path",
		"thread_source",
		"source",
		"history_mode",
		"model",
	];
	let expressions = selected
		.iter()
		.map(|column| {
			if columns.contains(*column) {
				format!("\"{column}\"")
			} else {
				"NULL".to_owned()
			}
		})
		.collect::<Vec<_>>()
		.join(", ");
	let query = format!("SELECT {expressions} FROM threads");
	let mut statement = connection
		.prepare(&query)
		.context("could not prepare the Codex thread catalog query")?;
	let mut rows = statement
		.query([])
		.context("could not query the Codex thread catalog")?;
	let mut threads = Vec::new();
	let mut warnings = Vec::new();
	while let Some(row) = rows.next().context("could not read a Codex catalog row")? {
		match summary_from_row(row, home) {
			Ok(summary) => threads.push(summary),
			Err(error) => warnings.push(format!("Ignored an invalid Codex catalog row: {error:#}")),
		}
	}

	Ok((threads, warnings))
}

fn thread_columns(connection: &Connection) -> Result<HashSet<String>> {
	let mut statement = connection
		.prepare("PRAGMA table_info(threads)")
		.context("could not inspect the Codex threads schema")?;
	let names = statement
		.query_map([], |row| row.get::<_, String>(1))
		.context("could not query the Codex threads schema")?
		.collect::<rusqlite::Result<HashSet<_>>>()
		.context("could not read the Codex threads schema")?;
	Ok(names)
}

/// Maps a positional dynamic query into the stable application domain.
fn summary_from_row(row: &Row<'_>, home: &Path) -> Result<ThreadSummary> {
	let id = row_text(row, 0)?
		.ok_or_else(|| anyhow!("missing thread id"))?
		.parse::<ThreadId>()
		.context("invalid thread id")?;
	let rollout_path = row_text(row, 1)?
		.map(|path| normalize_catalog_path(home, &sanitize_text(&path)))
		.unwrap_or_default();
	let title = first_nonempty([row_text(row, 2)?, row_text(row, 4)?])
		.map(|title| sanitize_text(&title))
		.unwrap_or_default();
	let preview = first_nonempty([row_text(row, 3)?, row_text(row, 4)?])
		.map(|preview| sanitize_text(&preview))
		.unwrap_or_default();
	let cwd = row_text(row, 5)?
		.map(|cwd| PathBuf::from(sanitize_text(&cwd)))
		.unwrap_or_default();
	let created_at = timestamp_from_parts(row_i64(row, 6)?, row_i64(row, 7)?);
	let recency_at = timestamp_from_parts(row_i64(row, 10)?, row_i64(row, 11)?);
	let catalog_updated_at = timestamp_from_parts(row_i64(row, 8)?, row_i64(row, 9)?);
	let updated_at = recency_at.or(catalog_updated_at);
	let archived = row_bool(row, 12)? || is_archived_path(home, &rollout_path);
	let agent_path = row_text(row, 13)?;
	let thread_source = row_text(row, 14)?;
	let source = row_text(row, 15)?;
	let is_agent = agent_path
		.as_deref()
		.is_some_and(|path| !path.trim().is_empty())
		|| thread_source.as_deref().is_some_and(|source| {
			!source.trim().is_empty() && !source.eq_ignore_ascii_case("user")
		}) || source.as_deref().is_some_and(text_mentions_agent);
	let history_mode =
		row_text(row, 16)?.map_or_else(|| "unknown".to_owned(), |mode| sanitize_text(&mode));
	let model = row_text(row, 17)?.map(|model| sanitize_text(&model));

	Ok(ThreadSummary {
		id,
		title,
		preview,
		cwd,
		rollout_path,
		created_at,
		updated_at,
		archived,
		is_agent,
		history_mode,
		model,
	})
}

fn row_text(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
	match row.get_ref(index)? {
		ValueRef::Null | ValueRef::Blob(_) => Ok(None),
		ValueRef::Text(value) => Ok(Some(String::from_utf8_lossy(value).into_owned())),
		ValueRef::Integer(value) => Ok(Some(value.to_string())),
		ValueRef::Real(value) => Ok(Some(value.to_string())),
	}
}

fn row_i64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<i64>> {
	match row.get_ref(index)? {
		ValueRef::Integer(value) => Ok(Some(value)),
		ValueRef::Real(value) => Ok(value.to_string().parse().ok()),
		ValueRef::Text(value) => Ok(String::from_utf8_lossy(value).parse().ok()),
		ValueRef::Null | ValueRef::Blob(_) => Ok(None),
	}
}

fn row_bool(row: &Row<'_>, index: usize) -> rusqlite::Result<bool> {
	match row.get_ref(index)? {
		ValueRef::Integer(value) => Ok(value != 0),
		ValueRef::Real(value) => Ok(value != 0.0),
		ValueRef::Text(value) => {
			let value = String::from_utf8_lossy(value);
			Ok(value == "1" || value.eq_ignore_ascii_case("true"))
		}
		ValueRef::Null | ValueRef::Blob(_) => Ok(false),
	}
}

fn first_nonempty<const N: usize>(values: [Option<String>; N]) -> Option<String> {
	values
		.into_iter()
		.flatten()
		.find(|value| !value.trim().is_empty())
}

fn timestamp_from_parts(milliseconds: Option<i64>, seconds: Option<i64>) -> Option<DateTime<Utc>> {
	milliseconds
		.and_then(DateTime::from_timestamp_millis)
		.or_else(|| {
			seconds.and_then(|value| {
				if value.unsigned_abs() > 10_000_000_000 {
					DateTime::from_timestamp_millis(value)
				} else {
					DateTime::from_timestamp(value, 0)
				}
			})
		})
}

fn normalize_catalog_path(home: &Path, value: &str) -> PathBuf {
	let path = PathBuf::from(value);
	if path.is_absolute() {
		path
	} else {
		home.join(path)
	}
}

fn text_mentions_agent(value: &str) -> bool {
	let value = value.to_ascii_lowercase();
	value.contains("subagent") || value == "agent"
}

#[derive(Debug, Clone)]
struct RolloutLocation {
	id: ThreadId,
	path: PathBuf,
	archived: bool,
	modified: SystemTime,
}

/// Walks only the two documented rollout roots and records recoverable errors.
fn discover_rollouts(home: &Path) -> (Vec<RolloutLocation>, Vec<String>) {
	let mut locations = Vec::new();
	let mut warnings = Vec::new();
	for (root, archived) in [
		(home.join("sessions"), false),
		(home.join("archived_sessions"), true),
	] {
		if !root.is_dir() {
			continue;
		}

		for entry in WalkDir::new(&root).follow_links(false) {
			let entry = match entry {
				Ok(entry) => entry,
				Err(error) => {
					warnings.push(format!(
						"Could not inspect part of {}: {error}",
						root.display()
					));
					continue;
				}
			};
			if !entry.file_type().is_file() || !is_rollout_file(entry.path()) {
				continue;
			}
			let Some(id) = rollout_ids_from_filename(entry.path()).map(|ids| ids.session) else {
				continue;
			};
			let modified = entry
				.metadata()
				.ok()
				.and_then(|metadata| metadata.modified().ok())
				.unwrap_or(SystemTime::UNIX_EPOCH);
			locations.push(RolloutLocation {
				id,
				path: entry.into_path(),
				archived,
				modified,
			});
		}
	}
	(locations, warnings)
}

/// Prefers the database's archive state, then the newest duplicate during moves.
fn select_location<'a>(
	locations: impl Iterator<Item = &'a RolloutLocation>,
	archived: Option<bool>,
) -> Option<&'a RolloutLocation> {
	locations.max_by(|left, right| {
		let left_preferred = archived.is_some_and(|archived| left.archived == archived);
		let right_preferred = archived.is_some_and(|archived| right.archived == archived);
		left_preferred
			.cmp(&right_preferred)
			.then_with(|| left.modified.cmp(&right.modified))
	})
}

/// Repairs moved paths and supplements catalog rows that `SQLite` has not seen yet.
fn reconcile_threads(
	home: &Path,
	database_threads: Vec<ThreadSummary>,
	locations: Vec<RolloutLocation>,
	warnings: &mut Vec<String>,
) -> Vec<ThreadSummary> {
	let mut by_id: HashMap<ThreadId, Vec<RolloutLocation>> = HashMap::new();
	for location in locations {
		by_id.entry(location.id).or_default().push(location);
	}

	let mut threads = Vec::new();
	let mut known = HashSet::new();
	for mut thread in database_threads {
		let candidates = by_id.get(&thread.id).map(Vec::as_slice).unwrap_or_default();
		let current_matches_state = thread.rollout_path.is_file()
			&& is_rollout_file(&thread.rollout_path)
			&& is_archived_path(home, &thread.rollout_path) == thread.archived;
		if !current_matches_state {
			if let Some(location) = select_location(candidates.iter(), Some(thread.archived)) {
				thread.rollout_path.clone_from(&location.path);
				thread.archived = location.archived;
			}
		}

		if thread.rollout_path.is_file() && is_rollout_file(&thread.rollout_path) {
			known.insert(thread.id);
			threads.push(thread);
		} else {
			let expected = if thread.rollout_path.as_os_str().is_empty() {
				"no recorded path".to_owned()
			} else {
				thread.rollout_path.display().to_string()
			};
			warnings.push(format!(
				"Catalog session {} points to a missing rollout ({expected}); the unavailable task was omitted.",
				thread.id,
			));
		}
	}

	for (id, candidates) in by_id {
		if known.contains(&id) {
			continue;
		}
		let Some(location) = select_location(candidates.iter(), None) else {
			continue;
		};
		match summarize_rollout(home, &location.path, Some(id)) {
			Ok((summary, rollout_warnings)) => {
				warnings.extend(
					rollout_warnings
						.into_iter()
						.map(|warning| format!("Session {}: {warning}", summary.id)),
				);
				threads.push(summary);
			}
			Err(error) => warnings.push(format!(
				"Skipped unreadable session {id} at {}: {error:#}",
				location.path.display()
			)),
		}
	}

	threads.sort_by(compare_threads);
	threads
}

fn compare_threads(left: &ThreadSummary, right: &ThreadSummary) -> Ordering {
	right
		.updated_at
		.cmp(&left.updated_at)
		.then_with(|| right.created_at.cmp(&left.created_at))
		.then_with(|| left.id.to_string().cmp(&right.id.to_string()))
}

/// Recovers enough metadata for direct paths and database-free installations.
fn summarize_rollout(
	home: &Path,
	path: &Path,
	id_hint: Option<ThreadId>,
) -> Result<(ThreadSummary, Vec<String>)> {
	let mut metadata: Option<SessionMetadata> = None;
	let mut first_user_message = None;
	let warnings = visit_snapshot_records(path, |record| {
		match record {
			SnapshotRecord::Session {
				metadata: session, ..
			} if metadata.is_none() => metadata = Some(session),
			SnapshotRecord::Message { message, .. }
				if message.role == MessageRole::User && first_user_message.is_none() =>
			{
				first_user_message = Some(message.text);
			}
			_ => {}
		}
		if metadata.is_some() && first_user_message.is_some() {
			ControlFlow::Break(())
		} else {
			ControlFlow::Continue(())
		}
	})?;

	if let (Some(hint), Some(recorded)) = (id_hint, metadata.as_ref().and_then(|meta| meta.id)) {
		if hint != recorded {
			bail!("rollout filename identifies {hint}, but session metadata identifies {recorded}");
		}
	}
	let id = id_hint
		.or_else(|| metadata.as_ref().and_then(|metadata| metadata.id))
		.ok_or_else(|| anyhow!("rollout has no recognizable session id"))?;
	let excerpt = first_user_message.as_deref().unwrap_or_default();
	let title = one_line_excerpt(excerpt, 100);
	let preview = one_line_excerpt(excerpt, 240);
	let modified = fs::metadata(path)
		.ok()
		.and_then(|metadata| metadata.modified().ok())
		.map(DateTime::<Utc>::from);

	Ok((
		ThreadSummary {
			id,
			title,
			preview,
			cwd: metadata
				.as_ref()
				.and_then(|metadata| metadata.cwd.as_deref())
				.map(PathBuf::from)
				.unwrap_or_default(),
			rollout_path: path.to_path_buf(),
			created_at: metadata.as_ref().and_then(|metadata| metadata.timestamp),
			updated_at: modified,
			archived: is_archived_path(home, path),
			is_agent: metadata.as_ref().is_some_and(|metadata| metadata.is_agent),
			history_mode: metadata
				.as_ref()
				.and_then(|metadata| metadata.history_mode.clone())
				.unwrap_or_else(|| "unknown".to_owned()),
			model: metadata.and_then(|metadata| metadata.model),
		},
		warnings,
	))
}

fn one_line_excerpt(value: &str, maximum_characters: usize) -> String {
	let mut excerpt = String::new();
	for word in sanitize_text(value).split_whitespace() {
		let separator = usize::from(!excerpt.is_empty());
		let remaining = maximum_characters.saturating_sub(excerpt.chars().count() + separator);
		if remaining == 0 {
			break;
		}
		if separator == 1 {
			excerpt.push(' ');
		}
		let mut characters = word.chars();
		excerpt.extend(characters.by_ref().take(remaining));
		if characters.next().is_some() {
			break;
		}
	}
	excerpt
}

fn resolve_direct_path(home: &Path, path: &Path) -> PathBuf {
	if path.is_absolute() || path.is_file() {
		path.to_path_buf()
	} else {
		let from_home = home.join(path);
		if from_home.is_file() {
			from_home
		} else {
			path.to_path_buf()
		}
	}
}

fn is_archived_path(home: &Path, path: &Path) -> bool {
	path.strip_prefix(home.join("archived_sessions")).is_ok()
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::path::Path;

	use rusqlite::{Connection, params};
	use tempfile::TempDir;

	use super::{ArchiveFilter, Catalog, CatalogFilter};
	use crate::codex::selector::ThreadSelector;
	use crate::domain::ThreadId;

	const ACTIVE_ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";
	const ARCHIVED_ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0c";
	const AGENT_ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0d";

	#[test]
	fn filters_active_archived_and_agent_tasks_from_dynamic_schema() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let active = rollout_path(temporary.path(), "sessions/2026/07/20", ACTIVE_ID);
		let archived = rollout_path(temporary.path(), "archived_sessions", ARCHIVED_ID);
		let agent = rollout_path(temporary.path(), "sessions/2026/07/20", AGENT_ID);
		write_rollout(&active, ACTIVE_ID, "Active prompt", false);
		write_rollout(&archived, ARCHIVED_ID, "Archived prompt", false);
		write_rollout(&agent, AGENT_ID, "Agent prompt", true);
		create_catalog(
			&temporary.path().join("state_5.sqlite"),
			[
				(
					active.as_path(),
					ACTIVE_ID,
					"Active\u{1b} title",
					false,
					false,
				),
				(archived.as_path(), ARCHIVED_ID, "Archived", true, false),
				(agent.as_path(), AGENT_ID, "Agent", false, true),
			],
		);

		// A legacy nested database must never override the live root catalog.
		let stale_directory = temporary.path().join("sqlite");
		fs::create_dir_all(&stale_directory).expect("stale directory should be created");
		create_catalog(&stale_directory.join("state_99.sqlite"), []);

		let catalog = Catalog::load(temporary.path()).expect("catalog should load");
		let default_threads = catalog.list(CatalogFilter::default());
		let archived_threads = catalog.list(CatalogFilter {
			archive: ArchiveFilter::Archived,
			include_agents: false,
		});
		let all_threads = catalog.list(CatalogFilter {
			archive: ArchiveFilter::All,
			include_agents: true,
		});

		assert_eq!(default_threads.len(), 1);
		assert_eq!(default_threads[0].title, "Active title");
		assert_eq!(archived_threads.len(), 1);
		assert_eq!(all_threads.len(), 3);
	}

	#[test]
	fn direct_uuid_resolution_includes_archived_tasks() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let archived = rollout_path(temporary.path(), "archived_sessions", ARCHIVED_ID);
		write_rollout(&archived, ARCHIVED_ID, "Archived prompt", false);

		let catalog = Catalog::load(temporary.path()).expect("filesystem fallback should load");
		let selector = ThreadSelector::Id(
			ARCHIVED_ID
				.parse::<ThreadId>()
				.expect("fixture ID should parse"),
		);
		let resolved = catalog
			.resolve(&selector)
			.expect("archived UUID should resolve");

		assert!(resolved.archived);
		assert_eq!(resolved.title, "Archived prompt");
	}

	#[test]
	fn filesystem_fallback_recovers_metadata_and_hides_agents_by_default() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let active = rollout_path(temporary.path(), "sessions/2026/07/20", ACTIVE_ID);
		let agent = rollout_path(temporary.path(), "sessions/2026/07/20", AGENT_ID);
		write_rollout(&active, ACTIVE_ID, "Recovered from disk", false);
		write_rollout(&agent, AGENT_ID, "Hidden agent", true);

		let catalog = Catalog::load(temporary.path()).expect("filesystem fallback should load");
		let default_threads = catalog.list(CatalogFilter::default());
		let with_agents = catalog.list(CatalogFilter {
			archive: ArchiveFilter::Active,
			include_agents: true,
		});

		assert_eq!(default_threads.len(), 1);
		assert_eq!(default_threads[0].title, "Recovered from disk");
		assert_eq!(with_agents.len(), 2);
	}

	#[test]
	fn compound_archive_filename_uses_the_primary_session_id() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let rollout_segment_id = "0198cabe-0dc5-7c82-a9a9-99740a332c0e";
		let archived = temporary.path().join("archived_sessions").join(format!(
			"rollout-2026-07-20T10-00-00-{ARCHIVED_ID}_{rollout_segment_id}.jsonl"
		));
		write_rollout(&archived, ARCHIVED_ID, "Archived copy", false);

		let catalog = Catalog::load(temporary.path()).expect("filesystem fallback should load");
		let archived_threads = catalog.list(CatalogFilter {
			archive: ArchiveFilter::Archived,
			include_agents: false,
		});

		assert_eq!(archived_threads.len(), 1);
		assert_eq!(archived_threads[0].id.to_string(), ARCHIVED_ID);
		assert!(
			catalog
				.warnings()
				.iter()
				.all(|warning| !warning.contains("filename identifies"))
		);
	}

	#[test]
	fn compound_archive_path_resolves_from_its_primary_session_metadata() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let rollout_segment_id = "0198cabe-0dc5-7c82-a9a9-99740a332c0e";
		let archived = temporary.path().join("archived_sessions").join(format!(
			"rollout-2026-07-20T10-00-00-{ARCHIVED_ID}_{rollout_segment_id}.jsonl"
		));
		write_rollout(&archived, ARCHIVED_ID, "Archived copy", false);

		let catalog = Catalog::load(temporary.path()).expect("filesystem fallback should load");
		let resolved = catalog
			.resolve(&ThreadSelector::Path(archived.clone()))
			.expect("compound archive path should resolve");

		assert_eq!(resolved.id.to_string(), ARCHIVED_ID);
		assert_eq!(resolved.rollout_path, archived);
		assert!(resolved.archived);
	}

	#[test]
	fn paginated_archive_segments_share_one_catalog_identity() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let archived_root = temporary.path().join("archived_sessions");
		for rollout_segment_id in [
			"0198cabe-0dc5-7c82-a9a9-99740a332c0e",
			"0198cabe-0dc5-7c82-a9a9-99740a332c0f",
		] {
			let archived = archived_root.join(format!(
				"rollout-2026-07-20T10-00-00-{ARCHIVED_ID}_{rollout_segment_id}.jsonl"
			));
			write_rollout(&archived, ARCHIVED_ID, "Archived copy", false);
		}

		let catalog = Catalog::load(temporary.path()).expect("filesystem fallback should load");
		let archived_threads = catalog.list(CatalogFilter {
			archive: ArchiveFilter::Archived,
			include_agents: false,
		});

		assert_eq!(archived_threads.len(), 1);
		assert_eq!(archived_threads[0].id.to_string(), ARCHIVED_ID);
	}

	#[test]
	fn conflicting_primary_filename_and_metadata_are_rejected() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let archived = rollout_path(temporary.path(), "archived_sessions", ARCHIVED_ID);
		write_rollout(&archived, ACTIVE_ID, "Conflicting identity", false);

		let catalog = Catalog::load(temporary.path()).expect("filesystem fallback should load");
		let error = catalog
			.resolve(&ThreadSelector::Path(archived))
			.expect_err("conflicting identity should be rejected");

		assert!(error.to_string().contains("session metadata identifies"));
	}

	#[test]
	fn missing_database_rollout_remains_an_actionable_doctor_diagnostic() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let missing = rollout_path(temporary.path(), "sessions/2026/07/20", ACTIVE_ID);
		create_catalog(
			&temporary.path().join("state_5.sqlite"),
			[(missing.as_path(), ACTIVE_ID, "Unavailable", false, false)],
		);

		let catalog = Catalog::load(temporary.path()).expect("catalog should load");

		assert!(catalog.list(CatalogFilter::default()).is_empty());
		assert!(catalog.warnings().iter().any(|warning| {
			warning.contains(ACTIVE_ID) && warning.contains(&missing.display().to_string())
		}));
	}

	fn rollout_path(home: &Path, directory: &str, id: &str) -> std::path::PathBuf {
		home.join(directory)
			.join(format!("rollout-2026-07-20T10-00-00-{id}.jsonl"))
	}

	fn write_rollout(path: &Path, id: &str, prompt: &str, is_agent: bool) {
		fs::create_dir_all(path.parent().expect("fixture path should have a parent"))
			.expect("rollout directory should be created");
		let source = if is_agent {
			"{\"subagent\":{\"parent\":\"test\"}}"
		} else {
			"\"cli\""
		};
		let content = format!(
			"{{\"timestamp\":\"2026-07-20T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/workspace\",\"source\":{source}}}}}\n{{\"timestamp\":\"2026-07-20T10:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":{}}}}}\n",
			serde_json::to_string(prompt).expect("fixture prompt should serialize"),
		);
		fs::write(path, content).expect("rollout fixture should be written");
	}

	fn create_catalog<'a>(
		path: &Path,
		rows: impl IntoIterator<Item = (&'a Path, &'a str, &'a str, bool, bool)>,
	) {
		let connection = Connection::open(path).expect("catalog fixture should open");
		connection
			.execute_batch(
				"CREATE TABLE threads (
					id TEXT PRIMARY KEY,
					rollout_path TEXT,
					title TEXT,
					preview TEXT,
					cwd TEXT,
					updated_at INTEGER,
					archived INTEGER,
					agent_path TEXT
				);",
			)
			.expect("catalog fixture schema should be created");
		for (rollout, id, title, archived, is_agent) in rows {
			connection
				.execute(
					"INSERT INTO threads
						(id, rollout_path, title, preview, cwd, updated_at, archived, agent_path)
					 VALUES (?1, ?2, ?3, ?3, '/workspace', 1, ?4, ?5)",
					params![
						id,
						rollout.to_string_lossy(),
						title,
						i64::from(archived),
						is_agent.then_some("0")
					],
				)
				.expect("catalog fixture row should be inserted");
		}
	}
}
