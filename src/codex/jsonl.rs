//! Safe, streaming parsing of Codex rollout files.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use walkdir::WalkDir;

use super::rollout::{is_rollout_file, rollout_ids_from_filename};
use crate::domain::{Chat, ChatMessage, MessagePhase, MessageRole, ThreadId, ThreadSummary};

// A cap keeps hidden tool records and compressed inputs from exhausting memory.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
// A corrupt history chain must not cause unbounded directory traversal or reads.
const MAX_HISTORY_SEGMENTS: usize = 256;

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
	let (segments, mut warnings) = resolve_rollout_chain(&rollout_path, thread.id)?;
	let segmented = segments.len() > 1;
	for segment in segments {
		let mut missing_ordinal = false;
		let segment_warnings = visit_snapshot_records(&segment.path, |record| {
			if let SnapshotRecord::Message { ordinal, message } = record {
				if segment
					.end_ordinal_exclusive
					.is_some_and(|end| ordinal.is_some_and(|ordinal| ordinal >= end))
				{
					return ControlFlow::Continue(());
				}
				missing_ordinal |= segment.end_ordinal_exclusive.is_some() && ordinal.is_none();
				messages.push(message);
			}
			ControlFlow::Continue(())
		})?;
		if missing_ordinal {
			warnings.push(format!(
				"{} contains a message without an ordinal before a continuation boundary; it was retained to avoid data loss.",
				segment_label(&segment.path)
			));
		}
		warnings.extend(segment_warnings.into_iter().map(|warning| {
			if segmented {
				format!("{}: {warning}", segment_label(&segment.path))
			} else {
				warning
			}
		}));
	}

	Ok(Chat {
		thread,
		messages,
		warnings,
	})
}

#[derive(Debug)]
struct RolloutSegment {
	path: PathBuf,
	end_ordinal_exclusive: Option<u64>,
}

/// Follows explicit history links so abandoned roots and stale overlap stay excluded.
fn resolve_rollout_chain(
	head: &Path,
	session_id: ThreadId,
) -> Result<(Vec<RolloutSegment>, Vec<String>)> {
	let mut reverse_chain = vec![RolloutSegment {
		path: head.to_path_buf(),
		end_ordinal_exclusive: None,
	}];
	let mut current = head.to_path_buf();
	let mut visited = HashSet::from([current.clone()]);
	let mut current_metadata = read_session_metadata(&current)?;
	if current_metadata
		.as_ref()
		.and_then(|metadata| metadata.history_base)
		.is_none()
	{
		return Ok((reverse_chain, Vec::new()));
	}
	let (available, ambiguous, mut warnings) = index_available_rollouts(head);
	let mut exhausted_limit = true;

	for _ in 0..MAX_HISTORY_SEGMENTS {
		let Some(metadata) = current_metadata else {
			exhausted_limit = false;
			break;
		};
		let expected_session =
			rollout_ids_from_filename(&current).map_or(session_id, |ids| ids.session);
		if let Some(recorded) = metadata.id {
			if recorded != expected_session {
				warnings.push(format!(
					"Stopped restoring transcript history because {} belongs to session {} instead of {}.",
					segment_label(&current),
					recorded,
					expected_session
				));
				exhausted_limit = false;
				break;
			}
		}
		let Some(base) = metadata.history_base else {
			exhausted_limit = false;
			break;
		};
		let Some(base_path) = available.get(&base.thread_id).cloned() else {
			warnings.push(format!(
				"Could not restore earlier transcript segment {}; exported the available continuation only.",
				base.thread_id
			));
			exhausted_limit = false;
			break;
		};
		if ambiguous.contains(&base.thread_id) {
			warnings.push(format!(
				"Multiple rollout files identify referenced segment {}; the newest candidate was used.",
				base.thread_id
			));
		}
		if !visited.insert(base_path.clone()) {
			warnings.push(format!(
				"Stopped restoring transcript history after detecting a segment cycle at {}.",
				segment_label(&base_path)
			));
			exhausted_limit = false;
			break;
		}
		let base_metadata = read_session_metadata(&base_path)?;
		if let (Some(metadata), Some(ids)) = (
			base_metadata.as_ref(),
			rollout_ids_from_filename(&base_path),
		) {
			if metadata.id.is_some_and(|recorded| recorded != ids.session) {
				warnings.push(format!(
					"Ignored transcript history segment {} because its filename and metadata identify different sessions.",
					segment_label(&base_path)
				));
				exhausted_limit = false;
				break;
			}
		}
		reverse_chain.push(RolloutSegment {
			path: base_path.clone(),
			end_ordinal_exclusive: Some(base.end_ordinal_exclusive),
		});
		current = base_path;
		current_metadata = base_metadata;
	}

	if exhausted_limit {
		warnings.push(format!(
			"Stopped restoring transcript history after {MAX_HISTORY_SEGMENTS} segments."
		));
	}
	reverse_chain.reverse();
	Ok((reverse_chain, warnings))
}

/// Indexes physical segments in the local Codex roots; explicit links choose the branch.
fn index_available_rollouts(
	head: &Path,
) -> (HashMap<ThreadId, PathBuf>, HashSet<ThreadId>, Vec<String>) {
	let mut by_segment: HashMap<ThreadId, (SystemTime, PathBuf)> = HashMap::new();
	let mut ambiguous = HashSet::new();
	let mut warnings = Vec::new();
	for entry in rollout_search_entries(head) {
		let entry = match entry {
			Ok(entry) => entry,
			Err(error) => {
				warnings.push(format!(
					"Could not inspect one potential transcript segment: {error}"
				));
				continue;
			}
		};
		if !entry.file_type().is_file() || !is_rollout_file(entry.path()) {
			continue;
		}
		let Some(ids) = rollout_ids_from_filename(entry.path()) else {
			continue;
		};
		let modified = entry
			.metadata()
			.ok()
			.and_then(|metadata| metadata.modified().ok())
			.unwrap_or(SystemTime::UNIX_EPOCH);
		let candidate = (modified, entry.into_path());
		match by_segment.get_mut(&ids.segment) {
			Some(existing) => {
				ambiguous.insert(ids.segment);
				if existing.0 < candidate.0
					|| (existing.0 == candidate.0 && existing.1 < candidate.1)
				{
					*existing = candidate;
				}
			}
			None => {
				by_segment.insert(ids.segment, candidate);
			}
		}
	}

	(
		by_segment
			.into_iter()
			.map(|(segment, (_, path))| (segment, path))
			.collect(),
		ambiguous,
		warnings,
	)
}

fn rollout_search_entries(
	head: &Path,
) -> Box<dyn Iterator<Item = walkdir::Result<walkdir::DirEntry>>> {
	if let Some(home) = codex_home_from_rollout(head) {
		let entries = [home.join("sessions"), home.join("archived_sessions")]
			.into_iter()
			.filter(|root| root.is_dir())
			.flat_map(|root| WalkDir::new(root).follow_links(false).into_iter());
		Box::new(entries)
	} else if let Some(parent) = head.parent() {
		Box::new(
			WalkDir::new(parent)
				.max_depth(1)
				.follow_links(false)
				.into_iter(),
		)
	} else {
		Box::new(std::iter::empty())
	}
}

fn codex_home_from_rollout(path: &Path) -> Option<&Path> {
	path.ancestors().find_map(|ancestor| {
		let name = ancestor.file_name()?.to_str()?;
		matches!(name, "sessions" | "archived_sessions").then(|| ancestor.parent())?
	})
}

fn read_session_metadata(path: &Path) -> Result<Option<SessionMetadata>> {
	let mut metadata = None;
	visit_snapshot_records(path, |record| {
		if let SnapshotRecord::Session {
			metadata: session, ..
		} = record
		{
			metadata = Some(session);
			ControlFlow::Break(())
		} else {
			ControlFlow::Continue(())
		}
	})?;
	Ok(metadata)
}

fn segment_label(path: &Path) -> String {
	path.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or("rollout")
		.to_owned()
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
	pub(crate) history_base: Option<HistoryBase>,
}

/// Link from one physical continuation segment to the preceding rollout prefix.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HistoryBase {
	pub(crate) thread_id: ThreadId,
	pub(crate) end_ordinal_exclusive: u64,
}

/// Relevant rollout records are normalized before other storage code sees them.
#[derive(Debug, Clone)]
pub(crate) enum SnapshotRecord {
	Session {
		metadata: SessionMetadata,
	},
	Message {
		ordinal: Option<u64>,
		message: ChatMessage,
	},
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
		Some("event_msg") => parse_event_record(line).map(|record| {
			record.map(|message| SnapshotRecord::Message {
				ordinal: header.ordinal,
				message,
			})
		}),
		Some("session_meta") => {
			parse_session_record(line).map(|metadata| Some(SnapshotRecord::Session { metadata }))
		}
		// `response_item` contains hidden context and duplicates, so it is never rendered.
		_ => Ok(None),
	}
}

/// Converts the two explicitly visible event variants into renderer-safe messages.
fn parse_event_record(line: &[u8]) -> serde_json::Result<Option<ChatMessage>> {
	let envelope = serde_json::from_slice::<EventEnvelope>(line)?;
	let timestamp = envelope.timestamp.as_deref().and_then(parse_timestamp);
	let normalized = match envelope.payload.kind.as_str() {
		"user_message" | "agent_message" => normalize_legacy_event(&envelope.payload, timestamp),
		"item_completed" => envelope
			.payload
			.item
			.as_ref()
			.and_then(|item| normalize_completed_item(item, timestamp)),
		_ => None,
	};

	Ok(normalized)
}

/// Normalizes the original flat event shape used by earlier Codex releases.
fn normalize_legacy_event(
	payload: &EventPayload,
	timestamp: Option<DateTime<Utc>>,
) -> Option<ChatMessage> {
	let had_message = payload.message.is_some();
	let attachment_count =
		value_count(payload.images.as_ref()) + value_count(payload.local_images.as_ref());
	if !had_message && attachment_count == 0 {
		return None;
	}

	let mut text = payload
		.message
		.as_deref()
		.map(sanitize_text)
		.unwrap_or_default();
	append_attachment_note(&mut text, attachment_count);
	match payload.kind.as_str() {
		"user_message" => Some(ChatMessage {
			role: MessageRole::User,
			phase: MessagePhase::User,
			text,
			timestamp,
		}),
		"agent_message" => Some(ChatMessage {
			role: MessageRole::Assistant,
			phase: message_phase(payload.phase.as_deref()),
			text,
			timestamp,
		}),
		_ => None,
	}
}

/// Normalizes the completed-item event shape emitted by current Codex clients.
fn normalize_completed_item(
	item: &CompletedItem,
	timestamp: Option<DateTime<Utc>>,
) -> Option<ChatMessage> {
	let mut text = item
		.content
		.iter()
		.filter(|content| content.kind.eq_ignore_ascii_case("text"))
		.filter_map(|content| content.text.as_deref())
		.map(sanitize_text)
		.filter(|text| !text.is_empty())
		.collect::<Vec<_>>()
		.join("\n\n");
	let attachment_count = item
		.content
		.iter()
		.filter(|content| {
			content.kind.eq_ignore_ascii_case("image")
				|| content.kind.eq_ignore_ascii_case("local_image")
		})
		.count();
	append_attachment_note(&mut text, attachment_count);
	if text.is_empty() {
		return None;
	}

	match item.kind.as_str() {
		"UserMessage" => Some(ChatMessage {
			role: MessageRole::User,
			phase: MessagePhase::User,
			text,
			timestamp,
		}),
		"AgentMessage" => Some(ChatMessage {
			role: MessageRole::Assistant,
			phase: message_phase(item.phase.as_deref()),
			text,
			timestamp,
		}),
		_ => None,
	}
}

fn message_phase(phase: Option<&str>) -> MessagePhase {
	match phase {
		Some("commentary") => MessagePhase::Commentary,
		Some("final_answer") => MessagePhase::FinalAnswer,
		_ => MessagePhase::Legacy,
	}
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
			.or(envelope.payload.session_id.as_deref())
			.and_then(|id| id.parse::<ThreadId>().ok()),
		cwd: envelope.payload.cwd.map(|cwd| sanitize_text(&cwd)),
		timestamp: envelope.timestamp.as_deref().and_then(parse_timestamp),
		is_agent,
		history_mode: envelope
			.payload
			.history_mode
			.map(|mode| sanitize_text(&mode)),
		model: envelope.payload.model.map(|model| sanitize_text(&model)),
		history_base: envelope.payload.history_base.map(|base| HistoryBase {
			thread_id: base.thread_id,
			end_ordinal_exclusive: base.end_ordinal_exclusive,
		}),
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
	ordinal: Option<u64>,
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
	item: Option<CompletedItem>,
}

#[derive(Debug, Deserialize)]
struct CompletedItem {
	#[serde(rename = "type")]
	kind: String,
	#[serde(default)]
	content: Vec<CompletedContent>,
	phase: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletedContent {
	#[serde(rename = "type")]
	#[serde(default)]
	kind: String,
	text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionEnvelope {
	timestamp: Option<String>,
	payload: SessionPayload,
}

#[derive(Debug, Deserialize)]
struct SessionPayload {
	id: Option<String>,
	session_id: Option<String>,
	cwd: Option<String>,
	source: Option<Value>,
	agent_path: Option<String>,
	history_mode: Option<String>,
	model: Option<String>,
	history_base: Option<HistoryBasePayload>,
}

#[derive(Debug, Deserialize)]
struct HistoryBasePayload {
	thread_id: ThreadId,
	end_ordinal_exclusive: u64,
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
	fn renders_current_completed_items_without_exposing_hidden_response_records() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl");
		let records = concat!(
			"{\"ordinal\":1,\"timestamp\":\"2026-09-18T10:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"UserMessage\",\"content\":[{\"type\":\"text\",\"text\":\"Current prompt\"},{\"type\":\"image\",\"image_url\":\"data:image/png;base64,PRIVATE\"}]}}}\n",
			"{\"ordinal\":2,\"timestamp\":\"2026-09-18T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":[{\"text\":\"hidden duplicate\"}]}}\n",
			"{\"ordinal\":2,\"timestamp\":\"2026-09-18T10:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"ToolCall\",\"content\":[{\"type\":\"text\",\"text\":\"hidden tool arguments\"}]}}}\n",
			"{\"ordinal\":2,\"timestamp\":\"2026-09-18T10:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"Reasoning\",\"content\":[{\"type\":\"text\",\"text\":\"hidden reasoning\"}]}}}\n",
			"{\"ordinal\":3,\"timestamp\":\"2026-09-18T10:00:02Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"AgentMessage\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"Text\",\"text\":\"Current answer\"}]}}}\n",
		);
		fs::write(&path, records).expect("rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("current rollout should parse");

		assert_eq!(chat.messages.len(), 2);
		assert_eq!(chat.messages[0].role, MessageRole::User);
		assert!(chat.messages[0].text.contains("Current prompt"));
		assert!(chat.messages[0].text.contains("1 image attachment"));
		assert!(!chat.messages[0].text.contains("PRIVATE"));
		assert_eq!(chat.messages[1].text, "Current answer");
		assert_eq!(chat.messages[1].phase, MessagePhase::FinalAnswer);
		assert!(
			chat.messages
				.iter()
				.all(|message| !message.text.contains("hidden tool arguments")
					&& !message.text.contains("hidden reasoning"))
		);
	}

	#[test]
	fn continuation_segments_restore_history_without_replaying_overlap() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let archived = temporary.path().join("archived_sessions");
		fs::create_dir_all(&archived).expect("archive directory should be created");
		let base_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c0c";
		let current_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c0d";
		let base = archived.join(format!(
			"rollout-2026-09-18T10-00-00-{ID}_{base_segment}.jsonl"
		));
		let current = archived.join(format!(
			"rollout-2026-09-18T11-00-00-{ID}_{current_segment}.jsonl"
		));
		let abandoned = archived.join(format!("rollout-2026-09-18T09-00-00-{ID}.jsonl"));
		fs::write(
			&abandoned,
			format!(
				"{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\"}}}}\n\
				 {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Abandoned root\"}}}}\n"
			),
		)
		.expect("abandoned rollout should be written");
		fs::write(
			&base,
			format!(
				"{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\"}}}}\n\
				 {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"From the base\"}}}}\n\
				 {{\"ordinal\":2,\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"Discarded overlap\"}}}}\n"
			),
		)
		.expect("base rollout should be written");
		fs::write(
			&current,
			format!(
				"{{\"ordinal\":2,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"history_base\":{{\"thread_id\":\"{base_segment}\",\"end_ordinal_exclusive\":2}}}}}}\n\
				 {{\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"From the continuation\",\"phase\":\"final_answer\"}}}}\n"
			),
		)
		.expect("continuation rollout should be written");

		let chat = parse_chat(summary(current)).expect("segmented rollout should parse");

		assert_eq!(
			chat.messages
				.iter()
				.map(|message| message.text.as_str())
				.collect::<Vec<_>>(),
			["From the base", "From the continuation"]
		);
		assert!(
			chat.messages
				.iter()
				.all(|message| message.text != "Abandoned root")
		);
	}

	#[test]
	fn history_links_cross_storage_roots_and_canonical_session_ids() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let predecessor_id = "0198cabe-0dc5-7c82-a9a9-99740a332c20";
		let active = temporary.path().join("sessions/2026/09/18");
		let archived = temporary.path().join("archived_sessions");
		fs::create_dir_all(&active).expect("active directory should be created");
		fs::create_dir_all(&archived).expect("archive directory should be created");
		let predecessor = active.join(format!(
			"rollout-2026-09-18T10-00-00-{predecessor_id}.jsonl.zst"
		));
		let head = archived.join(format!("rollout-2026-09-18T11-00-00-{ID}.jsonl"));
		let predecessor_records = format!(
			"{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{predecessor_id}\"}}}}\n\
			 {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"From another session root\"}}}}\n"
		);
		let compressed = zstd::stream::encode_all(predecessor_records.as_bytes(), 0)
			.expect("predecessor rollout should compress");
		fs::write(&predecessor, compressed).expect("predecessor rollout should be written");
		fs::write(
			&head,
			format!(
				"{{\"ordinal\":2,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"history_base\":{{\"thread_id\":\"{predecessor_id}\",\"end_ordinal_exclusive\":2}}}}}}\n\
				 {{\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"From the archived head\"}}}}\n"
			),
		)
		.expect("archived head should be written");

		let chat = parse_chat(summary(head)).expect("cross-root history should parse");

		assert_eq!(
			chat.messages
				.iter()
				.map(|message| message.text.as_str())
				.collect::<Vec<_>>(),
			["From another session root", "From the archived head"]
		);
	}

	#[test]
	fn missing_history_segment_is_reported_without_losing_the_available_head() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let missing_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c30";
		let path = temporary
			.path()
			.join(format!("rollout-2026-09-18T11-00-00-{ID}.jsonl"));
		fs::write(
			&path,
			format!(
				"{{\"ordinal\":2,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"history_base\":{{\"thread_id\":\"{missing_segment}\",\"end_ordinal_exclusive\":2}}}}}}\n\
				 {{\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"Available head\"}}}}\n"
			),
		)
		.expect("head rollout should be written");

		let chat = parse_chat(summary(path)).expect("available head should still parse");

		assert_eq!(chat.messages[0].text, "Available head");
		assert!(
			chat.warnings
				.iter()
				.any(|warning| warning.contains(missing_segment))
		);
	}

	#[test]
	fn cyclic_history_links_stop_without_replaying_segments() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let base_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c40";
		let head_segment = "0198cabe-0dc5-7c82-a9a9-99740a332c41";
		let base = temporary.path().join(format!(
			"rollout-2026-09-18T10-00-00-{ID}_{base_segment}.jsonl"
		));
		let head = temporary.path().join(format!(
			"rollout-2026-09-18T11-00-00-{ID}_{head_segment}.jsonl"
		));
		fs::write(
			&base,
			format!(
				"{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"history_base\":{{\"thread_id\":\"{head_segment}\",\"end_ordinal_exclusive\":4}}}}}}\n\
				 {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"From the base\"}}}}\n"
			),
		)
		.expect("base rollout should be written");
		fs::write(
			&head,
			format!(
				"{{\"ordinal\":2,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"history_base\":{{\"thread_id\":\"{base_segment}\",\"end_ordinal_exclusive\":2}}}}}}\n\
				 {{\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"From the head\"}}}}\n"
			),
		)
		.expect("head rollout should be written");

		let chat = parse_chat(summary(head)).expect("cyclic history should stop safely");

		assert_eq!(
			chat.messages
				.iter()
				.map(|message| message.text.as_str())
				.collect::<Vec<_>>(),
			["From the base", "From the head"]
		);
		assert!(
			chat.warnings
				.iter()
				.any(|warning| warning.contains("segment cycle"))
		);
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
	fn current_local_image_only_prompts_keep_a_private_attachment_marker() {
		let temporary = TempDir::new().expect("temporary directory should be created");
		let path = temporary.path().join("rollout.jsonl");
		fs::write(
			&path,
			"{\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"UserMessage\",\"content\":[{\"type\":\"local_image\",\"path\":\"/private/current-image.png\"}]}}}\n",
		)
		.expect("current attachment rollout fixture should be written");

		let chat = parse_chat(summary(path)).expect("current image-only prompt should parse");

		assert_eq!(chat.messages.len(), 1);
		assert_eq!(chat.messages[0].role, MessageRole::User);
		assert!(chat.messages[0].text.contains("1 image attachment"));
		assert!(!chat.messages[0].text.contains("/private/current-image.png"));
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
