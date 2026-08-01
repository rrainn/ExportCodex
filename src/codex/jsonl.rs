//! Safe, streaming parsing of Codex rollout files.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::ops::ControlFlow;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{Chat, ChatMessage, MessagePhase, MessageRole, ThreadId, ThreadSummary};

// A cap keeps hidden tool records and compressed inputs from exhausting memory.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Parses only messages that Codex marks as user-visible transcript events.
///
/// The rollout length is captured before reading so an active Codex writer cannot
/// make one export nondeterministically consume later turns.
///
/// # Errors
///
/// Returns an error when the rollout cannot be opened, decompressed, or streamed.
pub fn parse_chat(thread: ThreadSummary) -> Result<Chat> {
	let rollout_path = thread.rollout_path.clone();
	let mut messages = Vec::new();
	let warnings = visit_snapshot_records(&rollout_path, |record| {
		if let SnapshotRecord::Message(message) = record {
			messages.push(message);
		}
		ControlFlow::Continue(())
	})?;

	Ok(Chat {
		thread,
		messages,
		warnings,
	})
}

/// Metadata needed to recover a catalog entry when `SQLite` is unavailable.
#[derive(Debug, Clone)]
pub(crate) struct SessionMetadata {
	pub(crate) id: Option<ThreadId>,
	pub(crate) cwd: Option<String>,
	pub(crate) timestamp: Option<DateTime<Utc>>,
	pub(crate) is_agent: bool,
	pub(crate) history_mode: Option<String>,
	pub(crate) model: Option<String>,
}

/// Relevant rollout records are normalized before other storage code sees them.
#[derive(Debug, Clone)]
pub(crate) enum SnapshotRecord {
	Session(SessionMetadata),
	Message(ChatMessage),
}

/// Streams relevant records while discarding hidden response items and new types.
pub(crate) fn visit_snapshot_records<F>(path: &Path, mut visitor: F) -> Result<Vec<String>>
where
	F: FnMut(SnapshotRecord) -> ControlFlow<()>,
{
	let file = File::open(path)
		.with_context(|| format!("could not open Codex rollout {}", path.display()))?;
	let snapshot_len = file
		.metadata()
		.with_context(|| format!("could not inspect Codex rollout {}", path.display()))?
		.len();
	let limited = file.take(snapshot_len);
	let mut reader: Box<dyn BufRead> = if is_zstd_path(path) {
		let decoder = zstd::stream::read::Decoder::new(limited)
			.with_context(|| format!("could not decompress Codex rollout {}", path.display()))?;
		Box::new(BufReader::new(decoder))
	} else {
		Box::new(BufReader::new(limited))
	};

	let mut warnings = Vec::new();
	let mut line = Vec::new();
	let mut line_number = 0_u64;
	loop {
		line.clear();
		let read = match read_bounded_line(reader.as_mut(), &mut line) {
			Ok(read) => read,
			Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
				warnings.push(
					"Ignored an incomplete compressed record at the end of the rollout.".to_owned(),
				);
				break;
			}
			Err(error) => {
				return Err(error)
					.with_context(|| format!("could not read Codex rollout {}", path.display()));
			}
		};
		if !read.read_any {
			break;
		}

		line_number += 1;
		if read.oversized {
			warnings.push(format!(
				"Ignored an oversized JSON record on line {line_number} (records are limited to 8 MiB)."
			));
			continue;
		}
		if line.last() == Some(&b'\r') {
			line.pop();
		}
		if line.iter().all(u8::is_ascii_whitespace) {
			continue;
		}

		match parse_relevant_record(&line) {
			Ok(Some(record)) => {
				if visitor(record).is_break() {
					break;
				}
			}
			Ok(None) => {}
			Err(error) if !read.terminated && error.is_eof() => {
				warnings.push(format!(
					"Ignored an incomplete trailing JSON record on line {line_number}."
				));
			}
			Err(_) => warnings.push(format!(
				"Ignored a malformed JSON record on line {line_number}."
			)),
		}
	}

	Ok(warnings)
}

#[derive(Debug, Clone, Copy)]
struct BoundedLineRead {
	read_any: bool,
	terminated: bool,
	oversized: bool,
}

/// Reads through one physical line while retaining only a bounded prefix.
fn read_bounded_line(
	reader: &mut dyn BufRead,
	output: &mut Vec<u8>,
) -> io::Result<BoundedLineRead> {
	let mut read_any = false;
	let mut terminated = false;
	let mut oversized = false;

	loop {
		let buffer = reader.fill_buf()?;
		if buffer.is_empty() {
			break;
		}

		read_any = true;
		let newline = buffer.iter().position(|byte| *byte == b'\n');
		let consumed = newline.map_or(buffer.len(), |index| index + 1);
		let content_len = newline.unwrap_or(consumed);
		let retained = (MAX_RECORD_BYTES - output.len().min(MAX_RECORD_BYTES)).min(content_len);
		output.extend_from_slice(&buffer[..retained]);
		oversized |= retained < content_len;
		reader.consume(consumed);

		if newline.is_some() {
			terminated = true;
			break;
		}
	}

	Ok(BoundedLineRead {
		read_any,
		terminated,
		oversized,
	})
}

/// Performs a cheap top-level parse before allocating payloads for relevant types.
fn parse_relevant_record(line: &[u8]) -> serde_json::Result<Option<SnapshotRecord>> {
	let header = serde_json::from_slice::<RecordHeader>(line)?;
	match header.kind.as_deref() {
		Some("event_msg") => parse_event_record(line),
		Some("session_meta") => {
			parse_session_record(line).map(|record| Some(SnapshotRecord::Session(record)))
		}
		// `response_item` contains hidden context and duplicates, so it is never rendered.
		_ => Ok(None),
	}
}

/// Converts the two explicitly visible event variants into renderer-safe messages.
fn parse_event_record(line: &[u8]) -> serde_json::Result<Option<SnapshotRecord>> {
	let envelope = serde_json::from_slice::<EventEnvelope>(line)?;
	let had_message = envelope.payload.message.is_some();
	let attachment_count = value_count(envelope.payload.images.as_ref())
		+ value_count(envelope.payload.local_images.as_ref());
	if !had_message && attachment_count == 0 {
		return Ok(None);
	}
	let timestamp = envelope.timestamp.as_deref().and_then(parse_timestamp);
	let mut text = envelope
		.payload
		.message
		.as_deref()
		.map(sanitize_text)
		.unwrap_or_default();
	append_attachment_note(&mut text, attachment_count);

	let normalized = match envelope.payload.kind.as_str() {
		"user_message" => ChatMessage {
			role: MessageRole::User,
			phase: MessagePhase::User,
			text,
			timestamp,
		},
		"agent_message" => ChatMessage {
			role: MessageRole::Assistant,
			phase: match envelope.payload.phase.as_deref() {
				Some("commentary") => MessagePhase::Commentary,
				Some("final_answer") => MessagePhase::FinalAnswer,
				_ => MessagePhase::Legacy,
			},
			text,
			timestamp,
		},
		_ => return Ok(None),
	};

	Ok(Some(SnapshotRecord::Message(normalized)))
}

fn value_count(value: Option<&Value>) -> usize {
	match value {
		None | Some(Value::Null) => 0,
		Some(Value::Array(values)) => values.len(),
		Some(_) => 1,
	}
}

fn append_attachment_note(text: &mut String, count: usize) {
	if count == 0 {
		return;
	}
	if !text.trim().is_empty() {
		text.push_str("\n\n");
	}
	let (noun, verb) = if count == 1 {
		("attachment", "is")
	} else {
		("attachments", "are")
	};
	text.push_str(&format!(
		"[{count} image {noun} from the original prompt {verb} not embedded in this export.]"
	));
}

/// Extracts only durable metadata and treats arbitrary `source` shapes defensively.
fn parse_session_record(line: &[u8]) -> serde_json::Result<SessionMetadata> {
	let envelope = serde_json::from_slice::<SessionEnvelope>(line)?;
	let source_is_agent = envelope
		.payload
		.source
		.as_ref()
		.is_some_and(value_mentions_agent);
	let is_agent = envelope
		.payload
		.agent_path
		.as_deref()
		.is_some_and(|path| !path.trim().is_empty())
		|| source_is_agent;

	Ok(SessionMetadata {
		id: envelope
			.payload
			.id
			.as_deref()
			.and_then(|id| id.parse::<ThreadId>().ok()),
		cwd: envelope.payload.cwd.map(|cwd| sanitize_text(&cwd)),
		timestamp: envelope.timestamp.as_deref().and_then(parse_timestamp),
		is_agent,
		history_mode: envelope
			.payload
			.history_mode
			.map(|mode| sanitize_text(&mode)),
		model: envelope.payload.model.map(|model| sanitize_text(&model)),
	})
}

/// Finds explicit agent markers without depending on Codex's evolving source schema.
fn value_mentions_agent(value: &Value) -> bool {
	match value {
		Value::String(value) => {
			let value = value.to_ascii_lowercase();
			value.contains("subagent") || value == "agent"
		}
		Value::Array(values) => values.iter().any(value_mentions_agent),
		Value::Object(values) => values.iter().any(|(key, value)| {
			let key = key.to_ascii_lowercase();
			key.contains("subagent") || key == "agent_path" || value_mentions_agent(value)
		}),
		_ => false,
	}
}

/// Removes terminal and document control characters while preserving layout.
#[must_use]
pub(crate) fn sanitize_text(value: &str) -> String {
	value
		.chars()
		.filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
		.collect()
}

/// Parses Codex's RFC 3339 timestamps while remaining tolerant of legacy records.
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
	DateTime::parse_from_rfc3339(value)
		.ok()
		.map(|timestamp| timestamp.with_timezone(&Utc))
}

fn is_zstd_path(path: &Path) -> bool {
	path.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| extension.eq_ignore_ascii_case("zst"))
		&& path
			.file_stem()
			.map(Path::new)
			.and_then(Path::extension)
			.and_then(|extension| extension.to_str())
			.is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
}

#[derive(Debug, Deserialize)]
struct RecordHeader {
	#[serde(rename = "type")]
	kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventEnvelope {
	timestamp: Option<String>,
	payload: EventPayload,
}

#[derive(Debug, Deserialize)]
struct EventPayload {
	#[serde(rename = "type")]
	kind: String,
	message: Option<String>,
	phase: Option<String>,
	images: Option<Value>,
	local_images: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SessionEnvelope {
	timestamp: Option<String>,
	payload: SessionPayload,
}

#[derive(Debug, Deserialize)]
struct SessionPayload {
	id: Option<String>,
	cwd: Option<String>,
	source: Option<Value>,
	agent_path: Option<String>,
	history_mode: Option<String>,
	model: Option<String>,
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::path::{Path, PathBuf};

	use chrono::{TimeZone, Utc};
	use tempfile::TempDir;

	use super::{MAX_RECORD_BYTES, parse_chat, sanitize_text};
	use crate::domain::{MessagePhase, MessageRole, ThreadId, ThreadSummary};

	const ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";

	#[test]
	fn renders_only_visible_events_with_phase_and_timestamp() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl");
		let records = concat!(
			"{\"timestamp\":\"2026-07-20T10:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Hello\"}}\n",
			"{\"timestamp\":\"2026-07-20T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":[{\"text\":\"hidden duplicate\"}]}}\n",
			"{\"timestamp\":\"2026-07-20T10:00:02Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Working\",\"phase\":\"commentary\"}}\n",
			"{\"timestamp\":\"2026-07-20T10:00:03Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Done\",\"phase\":\"final_answer\"}}\n",
			"{\"type\":\"future_record\",\"payload\":{\"anything\":true}}\n",
		);
		fs::write(&path, records).expect("rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("rollout should parse");

		assert_eq!(chat.messages.len(), 3);
		assert_eq!(chat.messages[0].role, MessageRole::User);
		assert_eq!(chat.messages[1].phase, MessagePhase::Commentary);
		assert_eq!(chat.messages[2].phase, MessagePhase::FinalAnswer);
		assert_eq!(
			chat.messages[2].timestamp,
			Utc.with_ymd_and_hms(2026, 7, 20, 10, 0, 3).single(),
		);
		assert!(chat.warnings.is_empty());
	}

	#[test]
	fn incomplete_trailing_record_is_non_fatal() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl");
		fs::write(
			&path,
			concat!(
				"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Safe\"}}\n",
				"{\"type\":\"event_msg\",\"payload\":",
			),
		)
		.expect("rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("incomplete tail should not fail the export");

		assert_eq!(chat.messages.len(), 1);
		assert!(
			chat.warnings
				.iter()
				.any(|warning| warning.contains("incomplete trailing"))
		);
	}

	#[test]
	fn compressed_rollout_uses_the_same_visible_event_rules() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl.zst");
		let records = concat!(
			"{\"type\":\"response_item\",\"payload\":{\"text\":\"hidden\"}}\n",
			"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Visible\"}}\n",
		);
		let compressed = zstd::stream::encode_all(records.as_bytes(), 0)
			.expect("rollout fixture should compress");
		fs::write(&path, compressed).expect("compressed rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("compressed rollout should parse");

		assert_eq!(chat.messages.len(), 1);
		assert_eq!(chat.messages[0].text, "Visible");
		assert_eq!(chat.messages[0].phase, MessagePhase::Legacy);
	}

	#[test]
	fn oversized_hidden_records_do_not_block_later_visible_messages() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl");
		let oversized = "x".repeat(MAX_RECORD_BYTES + 1);
		let records = format!(
			"{{\"type\":\"response_item\",\"payload\":{{\"text\":\"{oversized}\"}}}}\n\
			 {{\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"Still visible\"}}}}\n"
		);
		fs::write(&path, records).expect("oversized rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("later records should remain readable");

		assert_eq!(chat.messages.len(), 1);
		assert_eq!(chat.messages[0].text, "Still visible");
		assert!(
			chat.warnings
				.iter()
				.any(|warning| warning.contains("oversized"))
		);
	}

	#[test]
	fn image_only_prompts_remain_visible_as_attachment_markers() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl");
		fs::write(
			&path,
			"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"images\":[\"data:image/png;base64,AA\"],\"local_images\":[\"/private/image.png\",\"/private/other.png\"]}}\n",
		)
		.expect("attachment rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("image-only prompt should parse");

		assert_eq!(chat.messages.len(), 1);
		assert_eq!(chat.messages[0].role, MessageRole::User);
		assert!(chat.messages[0].text.contains("3 image attachments"));
		assert!(!chat.messages[0].text.contains("/private/image.png"));
	}

	#[test]
	fn control_characters_cannot_reach_output_documents() {
		assert_eq!(
			sanitize_text("safe\u{1b}[31m\0\r\nnext\tvalue"),
			"safe[31m\nnext\tvalue"
		);
	}

	fn summary(path: PathBuf) -> ThreadSummary {
		ThreadSummary {
			id: ID.parse::<ThreadId>().expect("fixture ID should parse"),
			title: "Fixture".to_owned(),
			preview: String::new(),
			cwd: Path::new("/workspace").to_path_buf(),
			rollout_path: path,
			created_at: None,
			updated_at: None,
			archived: false,
			is_agent: false,
			history_mode: "full".to_owned(),
			model: None,
		}
	}
}
