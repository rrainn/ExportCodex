//! Shared safety boundaries for untrusted transcript Markdown.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};

use super::theme::is_safe_link_destination;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ReplacementKind {
	RawHtml,
	UnsafeDestination,
	LiteralMath,
}

/// Makes one message safe to concatenate with the rest of a Markdown document.
pub(crate) fn portable_fragment(markdown: &str) -> String {
	let sanitized = replace_unsafe_constructs(markdown, false);
	close_unterminated_fence(&sanitized)
}

/// Keeps PDF input non-executable and ensures every authored character is selectable.
pub(crate) fn pdf_document(markdown: &str) -> String {
	replace_unsafe_constructs(markdown, true)
}

fn replace_unsafe_constructs(markdown: &str, literal_math: bool) -> String {
	let mut parser_options = Options::empty();
	parser_options.insert(Options::ENABLE_TABLES);
	parser_options.insert(Options::ENABLE_FOOTNOTES);
	parser_options.insert(Options::ENABLE_STRIKETHROUGH);
	parser_options.insert(Options::ENABLE_TASKLISTS);
	if literal_math {
		parser_options.insert(Options::ENABLE_MATH);
	}

	let mut replacements = Parser::new_ext(markdown, parser_options)
		.into_offset_iter()
		.filter_map(|(event, range)| match event {
			Event::Html(_) | Event::InlineHtml(_) => Some((range, ReplacementKind::RawHtml)),
			Event::Start(Tag::Image { .. }) => Some((range, ReplacementKind::UnsafeDestination)),
			Event::Start(Tag::Link { dest_url, .. })
				if !is_safe_link_destination(dest_url.as_ref()) =>
			{
				Some((range, ReplacementKind::UnsafeDestination))
			}
			// The PDF renderer's math glyphs are vector outlines rather than selectable text.
			Event::InlineMath(_) | Event::DisplayMath(_) if literal_math => {
				Some((range, ReplacementKind::LiteralMath))
			}
			_ => None,
		})
		.collect::<Vec<_>>();
	replacements.sort_by_key(|(range, kind)| (range.start, std::cmp::Reverse(range.end), *kind));
	apply_replacements(markdown, replacements)
}

fn apply_replacements(
	markdown: &str,
	replacements: Vec<(Range<usize>, ReplacementKind)>,
) -> String {
	let mut output = String::with_capacity(markdown.len());
	let mut cursor = 0;
	for (range, kind) in replacements {
		if range.start < cursor || range.end > markdown.len() {
			continue;
		}
		output.push_str(&markdown[cursor..range.start]);
		match kind {
			ReplacementKind::RawHtml => {
				output.push_str(&escape_html_as_markdown(&markdown[range.clone()]));
			}
			ReplacementKind::UnsafeDestination => {
				output.push_str(&escape_markdown_literal(&markdown[range.clone()]));
			}
			ReplacementKind::LiteralMath => {
				output.push_str(&markdown[range.clone()].replace('$', "\\$"));
			}
		}
		cursor = range.end;
	}
	output.push_str(&markdown[cursor..]);
	output
}

fn close_unterminated_fence(markdown: &str) -> String {
	let missing = Parser::new(markdown)
		.into_offset_iter()
		.find_map(|(event, range)| {
			let Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) = event else {
				return None;
			};
			let source = markdown.get(range.clone())?;
			let first_line = source.lines().next()?;
			let marker = first_line.chars().next()?;
			if !matches!(marker, '`' | '~') {
				return None;
			}
			let marker_count = first_line
				.chars()
				.take_while(|value| *value == marker)
				.count();
			if has_closing_fence(source, marker, marker_count) {
				return None;
			}

			let line_start = markdown[..range.start]
				.rfind('\n')
				.map_or(0, |index| index + 1);
			let continuation_prefix = markdown[line_start..range.start]
				.chars()
				.map(|character| if character == '>' { '>' } else { ' ' })
				.collect::<String>();
			Some((marker, marker_count, continuation_prefix))
		});

	let Some((marker, marker_count, continuation_prefix)) = missing else {
		return markdown.to_owned();
	};
	let mut output = String::with_capacity(markdown.len() + marker_count + 4);
	output.push_str(markdown);
	if !output.ends_with('\n') {
		output.push('\n');
	}
	output.push_str(&continuation_prefix);
	output.extend(std::iter::repeat_n(marker, marker_count));
	output.push('\n');
	output
}

fn has_closing_fence(source: &str, marker: char, minimum_count: usize) -> bool {
	let Some((_, body)) = source.split_once('\n') else {
		return false;
	};
	let Some(last_line) = body.trim_end_matches(['\r', '\n']).rsplit('\n').next() else {
		return false;
	};
	let mut candidate = last_line.trim_start_matches([' ', '\t']);
	while let Some(remainder) = candidate.strip_prefix('>') {
		candidate = remainder.trim_start_matches([' ', '\t']);
	}
	let marker_count = candidate
		.chars()
		.take_while(|character| *character == marker)
		.count();
	marker_count >= minimum_count
		&& candidate
			.chars()
			.skip(marker_count)
			.all(char::is_whitespace)
}

fn escape_html_as_markdown(value: &str) -> String {
	value
		.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

fn escape_markdown_literal(value: &str) -> String {
	let mut escaped = String::with_capacity(value.len() * 2);
	for character in value.chars() {
		if matches!(
			character,
			'\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '(' | ')'
		) {
			escaped.push('\\');
		}
		escaped.push(character);
	}
	escaped
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn closed_fences_are_not_modified() {
		let markdown = "```rust\nlet answer = 42;\n```";
		assert_eq!(portable_fragment(markdown), markdown);
	}

	#[test]
	fn inline_code_dollars_are_not_treated_as_math() {
		let markdown = "Use `$PATH`, then calculate $x^2$.";
		let sanitized = pdf_document(markdown);
		assert!(sanitized.contains("`$PATH`"));
		assert!(sanitized.contains("\\$x^2\\$"));
	}

	#[test]
	fn markdown_images_cannot_trigger_later_asset_loading() {
		let sanitized = portable_fragment("![private](file:///private/image.png)");
		let has_image =
			Parser::new(&sanitized).any(|event| matches!(event, Event::Start(Tag::Image { .. })));

		assert!(!has_image);
		assert!(sanitized.contains("private"));
	}
}
