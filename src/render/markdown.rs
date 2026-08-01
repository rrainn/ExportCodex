//! Portable Markdown rendering for normalized chats.

use std::fmt::Write;

use chrono::{DateTime, Utc};

use crate::domain::{Chat, ChatMessage, MessagePhase, MessageRole, RenderOptions};

use super::theme::title;

/// Renders a chat as readable, portable Markdown.
#[must_use]
pub fn render(chat: &Chat, options: &RenderOptions) -> String {
	let mut output = String::new();
	let title = escape_inline(&title(chat, options));
	writeln!(output, "# {title}\n").expect("writing to a string cannot fail");
	let mut messages = chat
		.messages
		.iter()
		.filter(|message| included(message, options))
		.peekable();
	let has_messages = messages.peek().is_some();

	if options.include_metadata {
		render_metadata(&mut output, chat);
		if has_messages {
			output.push_str("---\n\n");
		}
	}
	if !has_messages {
		output
			.push_str("## No visible messages\n\nNo user-visible messages matched this export.\n");
	}

	while let Some(message) = messages.next() {
		render_message(&mut output, message, options);
		if messages.peek().is_some() {
			output.push_str("---\n\n");
		}
	}

	output.trim_end().to_owned() + "\n"
}

/// Determines whether a visible message belongs in the requested export.
pub(crate) const fn included(message: &ChatMessage, options: &RenderOptions) -> bool {
	options.include_progress || !matches!(message.phase, MessagePhase::Commentary)
}

fn render_metadata(output: &mut String, chat: &Chat) {
	let workspace = chat
		.thread
		.cwd
		.file_name()
		.and_then(|name| name.to_str())
		.filter(|name| !name.trim().is_empty())
		.unwrap_or("Unknown workspace");
	let status = if chat.thread.archived {
		"Archived"
	} else {
		"Active"
	};

	// Plain labels remain clean in strict Markdown and PDF renderers alike.
	writeln!(output, "> Codex chat  ").expect("writing to a string cannot fail");
	writeln!(output, "> Session: {}  ", chat.thread.id).expect("writing to a string cannot fail");
	writeln!(output, "> Workspace: {}  ", escape_inline(workspace))
		.expect("writing to a string cannot fail");
	writeln!(output, "> Status: {status}  ").expect("writing to a string cannot fail");
	if let Some(updated_at) = chat.thread.updated_at.as_ref() {
		writeln!(output, "> Updated: {}", format_timestamp(updated_at))
			.expect("writing to a string cannot fail");
	}
	output.push('\n');
}

fn render_message(output: &mut String, message: &ChatMessage, options: &RenderOptions) {
	let label = match (message.role, message.phase) {
		(MessageRole::User, _) => "You",
		(MessageRole::Assistant, MessagePhase::Commentary) => "Codex - progress",
		(MessageRole::Assistant, _) => "Codex",
	};
	writeln!(output, "## {label}\n").expect("writing to a string cannot fail");

	if options.include_timestamps
		&& let Some(timestamp) = message.timestamp.as_ref()
	{
		writeln!(output, "*{}*\n", format_timestamp(timestamp))
			.expect("writing to a string cannot fail");
	}

	let text = message.text.trim();
	if text.is_empty() {
		output.push_str("*No text content.*\n");
	} else {
		output.push_str(&super::safety::portable_fragment(text));
		output.push('\n');
	}
	output.push('\n');
}

pub(crate) fn format_timestamp(timestamp: &DateTime<Utc>) -> String {
	timestamp.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn escape_inline(value: &str) -> String {
	let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
	let mut escaped = String::with_capacity(single_line.len());
	for character in single_line.chars() {
		if matches!(
			character,
			'\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '#'
		) {
			escaped.push('\\');
		}
		escaped.push(character);
	}
	escaped
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use std::str::FromStr;

	use pulldown_cmark::{HeadingLevel, Parser, Tag};

	use crate::domain::{ThreadId, ThreadSummary};

	use super::*;

	fn chat_with(messages: Vec<ChatMessage>) -> Chat {
		Chat {
			thread: ThreadSummary {
				id: ThreadId::from_str("019f874c-1e53-7d12-97db-fac3249ce327").unwrap(),
				title: "A *careful* export".to_owned(),
				preview: String::new(),
				cwd: PathBuf::from("/tmp/ExportCodex"),
				rollout_path: PathBuf::from("session.jsonl"),
				created_at: None,
				updated_at: None,
				archived: false,
				is_agent: false,
				history_mode: "legacy".to_owned(),
				model: None,
			},
			messages,
			warnings: Vec::new(),
		}
	}

	#[test]
	fn progress_can_be_filtered_without_hiding_final_answers() {
		let chat = chat_with(vec![
			ChatMessage {
				role: MessageRole::Assistant,
				phase: MessagePhase::Commentary,
				text: "Checking the project.".to_owned(),
				timestamp: None,
			},
			ChatMessage {
				role: MessageRole::Assistant,
				phase: MessagePhase::FinalAnswer,
				text: "The export is ready.".to_owned(),
				timestamp: None,
			},
		]);
		let options = RenderOptions {
			include_progress: false,
			..RenderOptions::default()
		};

		let output = render(&chat, &options);
		assert!(!output.contains("Checking the project."));
		assert!(output.contains("The export is ready."));
	}

	#[test]
	fn message_markdown_is_preserved() {
		let chat = chat_with(vec![ChatMessage {
			role: MessageRole::User,
			phase: MessagePhase::User,
			text: "Use **bold** and `code`.".to_owned(),
			timestamp: None,
		}]);

		let output = render(&chat, &RenderOptions::default());
		assert!(output.contains("Use **bold** and `code`."));
		assert!(output.starts_with("# A careful export"));
	}

	#[test]
	fn custom_title_replaces_the_catalog_title() {
		let chat = chat_with(Vec::new());
		let options = RenderOptions {
			title: Some("Printable notes".to_owned()),
			..RenderOptions::default()
		};

		let output = render(&chat, &options);
		assert!(output.starts_with("# Printable notes\n"));
		assert!(!output.contains("A careful export"));
	}

	#[test]
	fn transcript_does_not_end_with_an_orphan_separator() {
		let chat = chat_with(vec![ChatMessage {
			role: MessageRole::Assistant,
			phase: MessagePhase::FinalAnswer,
			text: "The final message.".to_owned(),
			timestamp: None,
		}]);

		let output = render(&chat, &RenderOptions::default());

		assert!(!output.trim_end().ends_with("---"));
	}

	#[test]
	fn an_unclosed_fence_cannot_swallow_the_next_message() {
		let chat = chat_with(vec![
			ChatMessage {
				role: MessageRole::User,
				phase: MessagePhase::User,
				text: "```rust\nlet answer = 42;".to_owned(),
				timestamp: None,
			},
			ChatMessage {
				role: MessageRole::Assistant,
				phase: MessagePhase::FinalAnswer,
				text: "This must remain a separate message.".to_owned(),
				timestamp: None,
			},
		]);

		let output = render(&chat, &RenderOptions::default());
		let role_headings = Parser::new(&output)
			.filter(|event| {
				matches!(
					event,
					pulldown_cmark::Event::Start(Tag::Heading {
						level: HeadingLevel::H2,
						..
					})
				)
			})
			.count();

		assert_eq!(role_headings, 2);
	}

	#[test]
	fn raw_html_is_literal_in_portable_markdown() {
		let chat = chat_with(vec![ChatMessage {
			role: MessageRole::User,
			phase: MessagePhase::User,
			text: "<script>alert('no')</script>".to_owned(),
			timestamp: None,
		}]);

		let output = render(&chat, &RenderOptions::default());

		assert!(!output.contains("<script>"));
		assert!(output.contains("&lt;script&gt;alert('no')&lt;/script&gt;"));
	}

	#[test]
	fn empty_transcripts_explain_why_no_messages_are_shown() {
		let output = render(&chat_with(Vec::new()), &RenderOptions::default());

		assert!(output.contains("No visible messages"));
	}
}
