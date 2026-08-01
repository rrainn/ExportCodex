//! Parsing of direct task selectors accepted by the CLI.

use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;
use url::Url;

use crate::domain::ThreadId;

/// A stable thread identifier or a direct rollout file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadSelector {
	/// A Codex thread selected by UUID or deeplink.
	Id(ThreadId),
	/// A transcript selected by its local rollout path.
	Path(PathBuf),
}

/// A user-facing selector validation error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SelectorError {
	/// No selector was supplied.
	#[error("the chat selector is empty")]
	Empty,
	/// A Codex link did not use the canonical task route.
	#[error("invalid Codex deeplink; expected codex://threads/<session-id>")]
	InvalidDeeplink,
	/// The value was neither a supported identifier nor rollout path.
	#[error(
		"invalid chat selector; enter a session UUID, codex://threads/<session-id>, or .jsonl path"
	)]
	InvalidSelector,
}

impl FromStr for ThreadSelector {
	type Err = SelectorError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let value = value.trim();
		if value.is_empty() {
			return Err(SelectorError::Empty);
		}

		if let Ok(id) = value.parse::<ThreadId>() {
			return Ok(Self::Id(id));
		}

		if value.starts_with("codex:") {
			return parse_deeplink(value).map(Self::Id);
		}

		if is_rollout_path(value) {
			return Ok(Self::Path(PathBuf::from(value)));
		}

		Err(SelectorError::InvalidSelector)
	}
}

/// Validates the documented desktop deeplink without accepting ambiguous routes.
fn parse_deeplink(value: &str) -> Result<ThreadId, SelectorError> {
	let url = Url::parse(value).map_err(|_| SelectorError::InvalidDeeplink)?;
	if url.scheme() != "codex"
		|| url.host_str() != Some("threads")
		|| !url.username().is_empty()
		|| url.password().is_some()
		|| url.port().is_some()
		|| url.query().is_some()
		|| url.fragment().is_some()
	{
		return Err(SelectorError::InvalidDeeplink);
	}

	let mut segments = url
		.path_segments()
		.ok_or(SelectorError::InvalidDeeplink)?
		.filter(|segment| !segment.is_empty());
	let id = segments
		.next()
		.ok_or(SelectorError::InvalidDeeplink)?
		.parse::<ThreadId>()
		.map_err(|_| SelectorError::InvalidDeeplink)?;
	if segments.next().is_some() {
		return Err(SelectorError::InvalidDeeplink);
	}

	Ok(id)
}

/// Restricts paths to rollout formats that the parser can actually consume.
fn is_rollout_path(value: &str) -> bool {
	is_extension(value, "jsonl")
		|| (is_extension(value, "zst")
			&& PathBuf::from(value)
				.file_stem()
				.and_then(|stem| stem.to_str())
				.is_some_and(|stem| is_extension(stem, "jsonl")))
}

fn is_extension(value: &str, expected: &str) -> bool {
	PathBuf::from(value)
		.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use std::str::FromStr;

	use super::{SelectorError, ThreadSelector};

	const ID: &str = "0198cabe-0dc5-7c82-a9a9-99740a332c0b";

	#[test]
	fn uuid_and_deeplink_select_the_same_thread() {
		let id = ThreadSelector::from_str(ID).expect("UUID should be accepted");
		let link = ThreadSelector::from_str(&format!("codex://threads/{ID}"))
			.expect("canonical deeplink should be accepted");

		assert_eq!(id, link);
	}

	#[test]
	fn compressed_and_plain_rollout_paths_are_supported() {
		assert_eq!(
			ThreadSelector::from_str("./rollout.jsonl").expect("JSONL path should parse"),
			ThreadSelector::Path(PathBuf::from("./rollout.jsonl")),
		);
		assert_eq!(
			ThreadSelector::from_str("./rollout.jsonl.zst")
				.expect("compressed JSONL path should parse"),
			ThreadSelector::Path(PathBuf::from("./rollout.jsonl.zst")),
		);
	}

	#[test]
	fn deeplinks_reject_queries_and_extra_segments() {
		for value in [
			format!("codex://threads/{ID}?source=test"),
			format!("codex://threads/{ID}/extra"),
			format!("codex://thread/{ID}"),
		] {
			assert_eq!(
				ThreadSelector::from_str(&value),
				Err(SelectorError::InvalidDeeplink),
			);
		}
	}

	#[test]
	fn unrelated_text_is_not_mistaken_for_a_path() {
		assert_eq!(
			ThreadSelector::from_str("find my latest chat"),
			Err(SelectorError::InvalidSelector),
		);
	}
}
