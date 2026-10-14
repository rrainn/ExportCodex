//! Shared recognition of Codex rollout paths and their stable identities.

use std::path::Path;

use crate::domain::ThreadId;

/// The stable chat ID and physical segment ID encoded in a rollout filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RolloutIds {
	pub(crate) session: ThreadId,
	pub(crate) segment: ThreadId,
}

/// Returns whether a path uses a rollout format the streaming parser supports.
pub(crate) fn is_rollout_file(path: &Path) -> bool {
	has_extension(path, "jsonl")
		|| (has_extension(path, "zst")
			&& path
				.file_stem()
				.map(Path::new)
				.is_some_and(|stem| has_extension(stem, "jsonl")))
}

/// Parses `...-<session>[_<segment>].jsonl[.zst]` without treating a segment as a chat.
pub(crate) fn rollout_ids_from_filename(path: &Path) -> Option<RolloutIds> {
	let name = path.file_name()?.to_str()?;
	let suffix_length = if has_extension(path, "jsonl") {
		".jsonl".len()
	} else if has_extension(path, "zst")
		&& path
			.file_stem()
			.map(Path::new)
			.is_some_and(|stem| has_extension(stem, "jsonl"))
	{
		".jsonl.zst".len()
	} else {
		return None;
	};
	let stem = name.get(..name.len().checked_sub(suffix_length)?)?;
	let mut encoded_ids = stem.split('_').filter_map(trailing_thread_id);
	let session = encoded_ids.next()?;
	let segment = encoded_ids.next().unwrap_or(session);
	Some(RolloutIds { session, segment })
}

fn trailing_thread_id(value: &str) -> Option<ThreadId> {
	value.get(value.len().checked_sub(36)?..)?.parse().ok()
}

fn has_extension(path: &Path, expected: &str) -> bool {
	path.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::rollout_ids_from_filename;

	#[test]
	fn plain_and_compressed_segments_keep_session_and_segment_ids_distinct() {
		let session = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";
		let segment = "0198cabe-0dc5-7c82-a9a9-99740a332c0c";
		for extension in ["jsonl", "jsonl.zst"] {
			let filename = format!("rollout-2026-09-18T10-00-00-{session}_{segment}.{extension}");
			let ids = rollout_ids_from_filename(Path::new(&filename))
				.expect("segmented rollout name should parse");
			assert_eq!(ids.session.to_string(), session);
			assert_eq!(ids.segment.to_string(), segment);
		}
	}

	#[test]
	fn unsegmented_rollout_uses_the_session_as_its_physical_id() {
		let session = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";
		let filename = format!("rollout-2026-09-18T10-00-00-{session}.jsonl");
		let ids = rollout_ids_from_filename(Path::new(&filename))
			.expect("unsegmented rollout name should parse");
		assert_eq!(ids.session, ids.segment);
	}
}
