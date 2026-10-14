//! End-to-end CLI behavior against a synthetic, privacy-sensitive Codex home.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use rusqlite::{Connection, params};
use tempfile::TempDir;

const THREAD_ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";
const SEGMENTED_THREAD_ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c10";

fn fixture_home() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
		.join("codex-home")
}

#[test]
fn direct_export_contains_visible_chat_without_hidden_context() {
	let temporary = TempDir::new().expect("temporary output directory should exist");
	let output = temporary.path().join("chat.md");

	cargo_bin_cmd!("export-codex")
		.args([
			"--codex-home",
			fixture_home()
				.to_str()
				.expect("fixture path should be UTF-8"),
			THREAD_ID,
			"--format",
			"markdown",
			"--output",
			output.to_str().expect("output path should be UTF-8"),
		])
		.assert()
		.success()
		.stderr(predicate::str::contains("Exported"))
		.stderr(predicate::str::contains("No root Codex state database").not());

	let markdown = fs::read_to_string(output).expect("Markdown export should be readable");
	assert!(markdown.contains("Please export **this chat**"));
	assert!(markdown.contains("selectable café → text"));
	assert!(markdown.contains("Codex - progress"));
	assert!(!markdown.contains("DO_NOT_EXPORT_HIDDEN_CONTEXT"));
	assert!(!markdown.contains("DO_NOT_EXPORT_TOOL_OUTPUT"));
	assert!(!markdown.contains("DO_NOT_EXPORT_DUPLICATE"));
}

#[test]
fn doctor_reports_catalog_diagnostics_without_polluting_normal_commands() {
	cargo_bin_cmd!("export-codex")
		.args([
			"--codex-home",
			fixture_home()
				.to_str()
				.expect("fixture path should be UTF-8"),
			"doctor",
		])
		.assert()
		.success()
		.stdout(predicate::str::contains("Catalog       ready with"))
		.stderr(predicate::str::contains("No root Codex state database"));
}

#[test]
fn no_progress_and_custom_title_change_observable_document_content() {
	let temporary = TempDir::new().expect("temporary output directory should exist");
	let output = temporary.path().join("chat.html");

	cargo_bin_cmd!("export-codex")
		.args([
			"--codex-home",
			fixture_home()
				.to_str()
				.expect("fixture path should be UTF-8"),
			THREAD_ID,
			"--format",
			"html",
			"--title",
			"Shareable notes",
			"--no-progress",
			"--output",
			output.to_str().expect("output path should be UTF-8"),
		])
		.assert()
		.success();

	let html = fs::read_to_string(output).expect("HTML export should be readable");
	assert!(html.contains("<title>Shareable notes</title>"));
	assert!(!html.contains("checking the archive"));
	assert!(html.contains("&lt;script&gt;not executable&lt;/script&gt;"));
	assert!(!html.contains("<script>"));
}

#[test]
fn existing_outputs_are_protected_without_force() {
	let temporary = TempDir::new().expect("temporary output directory should exist");
	let output = temporary.path().join("existing.md");
	fs::write(&output, "keep me").expect("collision fixture should be written");

	cargo_bin_cmd!("export-codex")
		.args([
			"--codex-home",
			fixture_home()
				.to_str()
				.expect("fixture path should be UTF-8"),
			THREAD_ID,
			"--format",
			"markdown",
			"--output",
			output.to_str().expect("output path should be UTF-8"),
		])
		.assert()
		.failure()
		.stderr(predicate::str::contains("refusing to replace"));

	assert_eq!(
		fs::read_to_string(output).expect("existing output should remain readable"),
		"keep me"
	);
}

#[test]
fn list_json_is_stable_and_script_friendly() {
	cargo_bin_cmd!("export-codex")
		.args([
			"--codex-home",
			fixture_home()
				.to_str()
				.expect("fixture path should be UTF-8"),
			"list",
			"--json",
		])
		.assert()
		.success()
		.stdout(predicate::str::contains(THREAD_ID))
		.stdout(predicate::str::contains("Please export"));
}

#[test]
fn cli_reconstructs_current_schema_continuations_through_the_catalog_boundary() {
	let home = segmented_fixture_home();
	let output_directory = TempDir::new().expect("temporary output directory should exist");
	let output = output_directory.path().join("segmented.md");

	cargo_bin_cmd!("export-codex")
		.args([
			"--codex-home",
			home.path().to_str().expect("fixture path should be UTF-8"),
			SEGMENTED_THREAD_ID,
			"--format",
			"markdown",
			"--output",
			output.to_str().expect("output path should be UTF-8"),
		])
		.assert()
		.success()
		.stderr(predicate::str::contains("warning:").not());

	let markdown = fs::read_to_string(output).expect("Markdown export should be readable");
	assert!(markdown.contains("Base prompt"));
	assert!(markdown.contains("Current answer"));
	assert!(!markdown.contains("Stale overlap"));
	assert!(!markdown.contains("Abandoned root"));
	assert!(!markdown.contains("HIDDEN_RESPONSE_DUPLICATE"));
}

fn segmented_fixture_home() -> TempDir {
	let home = TempDir::new().expect("temporary Codex home should exist");
	let archive = home.path().join("archived_sessions");
	fs::create_dir_all(&archive).expect("archive fixture directory should be created");
	let base_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c11";
	let head_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c12";
	let abandoned = archive.join(format!(
		"rollout-2026-09-18T09-00-00-{SEGMENTED_THREAD_ID}.jsonl"
	));
	let base = archive.join(format!(
		"rollout-2026-09-18T10-00-00-{SEGMENTED_THREAD_ID}_{base_segment}.jsonl"
	));
	let head = archive.join(format!(
		"rollout-2026-09-18T11-00-00-{SEGMENTED_THREAD_ID}_{head_segment}.jsonl"
	));
	fs::write(
		&abandoned,
		format!(
			"{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SEGMENTED_THREAD_ID}\"}}}}\n\
			 {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Abandoned root\"}}}}\n"
		),
	)
	.expect("abandoned root should be written");
	fs::write(
		&base,
		format!(
			"{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SEGMENTED_THREAD_ID}\"}}}}\n\
			 {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"item\":{{\"type\":\"UserMessage\",\"content\":[{{\"type\":\"text\",\"text\":\"Base prompt\"}}]}}}}}}\n\
			 {{\"ordinal\":2,\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"Stale overlap\"}}}}\n"
		),
	)
	.expect("base segment should be written");
	fs::write(
		&head,
		format!(
			"{{\"ordinal\":2,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SEGMENTED_THREAD_ID}\",\"history_base\":{{\"thread_id\":\"{base_segment}\",\"end_ordinal_exclusive\":2}}}}}}\n\
			 {{\"ordinal\":3,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"content\":[{{\"text\":\"HIDDEN_RESPONSE_DUPLICATE\"}}]}}}}\n\
			 {{\"ordinal\":4,\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"item\":{{\"type\":\"AgentMessage\",\"phase\":\"final_answer\",\"content\":[{{\"type\":\"Text\",\"text\":\"Current answer\"}}]}}}}}}\n"
		),
	)
	.expect("head segment should be written");

	// The database selects the live head; sibling roots must not override that choice.
	let connection =
		Connection::open(home.path().join("state_5.sqlite")).expect("catalog fixture should open");
	connection
		.execute_batch(
			"CREATE TABLE threads (
				id TEXT PRIMARY KEY,
				rollout_path TEXT,
				title TEXT,
				preview TEXT,
				cwd TEXT,
				updated_at INTEGER,
				archived INTEGER
			);",
		)
		.expect("catalog fixture schema should be created");
	connection
		.execute(
			"INSERT INTO threads
				(id, rollout_path, title, preview, cwd, updated_at, archived)
			 VALUES (?1, ?2, 'Segmented task', 'Segmented task', '/workspace', 1, 1)",
			params![SEGMENTED_THREAD_ID, head.to_string_lossy()],
		)
		.expect("catalog fixture row should be inserted");
	drop(connection);
	home
}
