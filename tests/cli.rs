//! End-to-end CLI behavior against a synthetic, privacy-sensitive Codex home.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

const THREAD_ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";

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
		.stderr(predicate::str::contains("Exported"));

	let markdown = fs::read_to_string(output).expect("Markdown export should be readable");
	assert!(markdown.contains("Please export **this chat**"));
	assert!(markdown.contains("selectable café → text"));
	assert!(markdown.contains("Codex - progress"));
	assert!(!markdown.contains("DO_NOT_EXPORT_HIDDEN_CONTEXT"));
	assert!(!markdown.contains("DO_NOT_EXPORT_TOOL_OUTPUT"));
	assert!(!markdown.contains("DO_NOT_EXPORT_DUPLICATE"));
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
