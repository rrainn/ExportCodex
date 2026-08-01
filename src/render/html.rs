//! Self-contained semantic HTML rendering with print-focused styling.

use std::fmt::Write;

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

use crate::domain::{Chat, ChatMessage, MessagePhase, MessageRole, RenderOptions, ThemeName};

use super::markdown::{format_timestamp, included};
use super::theme::{
	accent, font_size, is_safe_link_destination, margin_mm, palette, paper_css, title,
};

/// Renders a chat as a self-contained HTML document with no scripts or remote assets.
#[must_use]
pub fn render(chat: &Chat, options: &RenderOptions) -> String {
	let mut output = String::with_capacity(HTML_BASE_STYLE.len() + chat.messages.len() * 512);
	let title = escape_html(&title(chat, options));
	let colors = palette(options.theme);
	let theme_class = match options.theme {
		ThemeName::Paper => "theme-paper",
		ThemeName::Warm => "theme-warm",
		ThemeName::Mono => "theme-mono",
	};

	output.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
	output.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
	writeln!(output, "<title>{title}</title>").expect("writing to a string cannot fail");
	output.push_str("<style>\n:root {\n");
	writeln!(output, "\t--page-background: {};", colors.background)
		.expect("writing to a string cannot fail");
	writeln!(output, "\t--surface: {};", colors.surface).expect("writing to a string cannot fail");
	writeln!(output, "\t--text: {};", colors.text).expect("writing to a string cannot fail");
	writeln!(output, "\t--muted: {};", colors.muted).expect("writing to a string cannot fail");
	writeln!(output, "\t--border: {};", colors.border).expect("writing to a string cannot fail");
	writeln!(output, "\t--user-surface: {};", colors.user_surface)
		.expect("writing to a string cannot fail");
	writeln!(output, "\t--code-surface: {};", colors.code_surface)
		.expect("writing to a string cannot fail");
	writeln!(output, "\t--accent: {};", accent(options)).expect("writing to a string cannot fail");
	writeln!(output, "\t--body-size: {:.2}pt;", font_size(options))
		.expect("writing to a string cannot fail");
	output.push_str("}\n");
	writeln!(
		output,
		"@page {{ size: {}; margin: {:.1}mm; @bottom-left {{ content: \"Exported from Codex\"; }} @bottom-right {{ content: counter(page); }} }}",
		paper_css(options.paper_size),
		margin_mm(options.margins),
	)
	.expect("writing to a string cannot fail");
	output.push_str(HTML_BASE_STYLE);
	if let Some(custom_css) = options
		.custom_css
		.as_deref()
		.filter(|css| !css.trim().is_empty())
	{
		output.push_str("\n/* User customization */\n");
		output.push_str(&safe_style_text(custom_css));
		output.push('\n');
	}
	output.push_str("</style>\n</head>\n");
	writeln!(output, "<body class=\"{theme_class}\">").expect("writing to a string cannot fail");
	output.push_str("<main class=\"sheet\">\n<header class=\"document-header\">\n");
	output.push_str("<p class=\"eyebrow\">Codex transcript</p>\n");
	writeln!(output, "<h1>{title}</h1>").expect("writing to a string cannot fail");
	if options.include_metadata {
		render_metadata(&mut output, chat);
	}
	output.push_str("</header>\n<div class=\"conversation\">\n");

	let mut rendered_messages = 0;
	for message in chat
		.messages
		.iter()
		.filter(|message| included(message, options))
	{
		render_message(&mut output, message, options, rendered_messages);
		rendered_messages += 1;
	}
	if rendered_messages == 0 {
		output.push_str("<p class=\"empty-state\">No visible messages matched this export.</p>\n");
	}

	output.push_str("</div>\n<footer class=\"document-footer\">Exported from Codex</footer>\n");
	output.push_str("</main>\n</body>\n</html>\n");
	output
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

	output.push_str("<dl class=\"metadata\">\n");
	metadata_item(output, "Session", &chat.thread.id.to_string(), true);
	metadata_item(output, "Workspace", workspace, false);
	metadata_item(output, "Status", status, false);
	if let Some(updated_at) = chat.thread.updated_at.as_ref() {
		metadata_item(output, "Updated", &format_timestamp(updated_at), false);
	}
	if let Some(model) = chat
		.thread
		.model
		.as_deref()
		.filter(|model| !model.trim().is_empty())
	{
		metadata_item(output, "Model", model, false);
	}
	output.push_str("</dl>\n");
}

fn metadata_item(output: &mut String, label: &str, value: &str, code: bool) {
	let label = escape_html(label);
	let value = escape_html(value);
	output.push_str("<div class=\"metadata__item\">");
	write!(output, "<dt>{label}</dt><dd>").expect("writing to a string cannot fail");
	if code {
		write!(output, "<code>{value}</code>").expect("writing to a string cannot fail");
	} else {
		output.push_str(&value);
	}
	output.push_str("</dd></div>\n");
}

fn render_message(
	output: &mut String,
	message: &ChatMessage,
	options: &RenderOptions,
	index: usize,
) {
	let (role_class, phase_class, label) = match (message.role, message.phase) {
		(MessageRole::User, _) => ("role-user", "phase-user", "You"),
		(MessageRole::Assistant, MessagePhase::Commentary) => {
			("role-assistant", "phase-commentary", "Codex - progress")
		}
		(MessageRole::Assistant, MessagePhase::FinalAnswer) => {
			("role-assistant", "phase-final", "Codex")
		}
		(MessageRole::Assistant, MessagePhase::Legacy) => {
			("role-assistant", "phase-legacy", "Codex")
		}
		(MessageRole::Assistant, MessagePhase::User) => ("role-assistant", "phase-legacy", "Codex"),
	};

	writeln!(
		output,
		"<section class=\"message {role_class} {phase_class}\" aria-labelledby=\"message-{index}-label\">"
	)
	.expect("writing to a string cannot fail");
	output.push_str("<header class=\"message__header\">");
	write!(output, "<h2 id=\"message-{index}-label\">{label}</h2>")
		.expect("writing to a string cannot fail");
	if options.include_timestamps
		&& let Some(timestamp) = message.timestamp.as_ref()
	{
		write!(
			output,
			"<time datetime=\"{}\">{}</time>",
			escape_attribute(&timestamp.to_rfc3339()),
			escape_html(&format_timestamp(timestamp)),
		)
		.expect("writing to a string cannot fail");
	}
	output.push_str("</header>\n<div class=\"message__body\">");
	if message.text.trim().is_empty() {
		output.push_str("<p class=\"empty\">No text content.</p>");
	} else {
		output.push_str(&render_markdown_fragment(&message.text));
	}
	output.push_str("</div>\n</section>\n");
}

fn render_markdown_fragment(markdown: &str) -> String {
	let mut parser_options = Options::empty();
	parser_options.insert(Options::ENABLE_TABLES);
	parser_options.insert(Options::ENABLE_FOOTNOTES);
	parser_options.insert(Options::ENABLE_STRIKETHROUGH);
	parser_options.insert(Options::ENABLE_TASKLISTS);

	let parser = Parser::new_ext(markdown, parser_options);
	let mut safe_events = Vec::new();
	let mut link_stack = Vec::new();
	let mut image_stack = Vec::new();

	for event in parser {
		match event {
			Event::Html(raw_html) | Event::InlineHtml(raw_html) => {
				safe_events.push(Event::Text(raw_html));
			}
			Event::Start(tag) => match &tag {
				Tag::Link { dest_url, .. } => {
					let safe = is_safe_link_destination(dest_url);
					link_stack.push(safe);
					if safe {
						safe_events.push(Event::Start(tag));
					}
				}
				Tag::Image { dest_url, .. } => {
					let safe = is_safe_embedded_image(dest_url);
					image_stack.push(safe);
					if safe {
						safe_events.push(Event::Start(tag));
					}
				}
				_ => safe_events.push(Event::Start(tag)),
			},
			Event::End(end @ TagEnd::Link) => {
				if link_stack.pop().unwrap_or(false) {
					safe_events.push(Event::End(end));
				}
			}
			Event::End(end @ TagEnd::Image) => {
				if image_stack.pop().unwrap_or(false) {
					safe_events.push(Event::End(end));
				}
			}
			other => safe_events.push(other),
		}
	}

	let mut output = String::new();
	html::push_html(&mut output, safe_events.into_iter());
	output
}

fn is_safe_embedded_image(destination: &CowStr<'_>) -> bool {
	let value = destination.as_ref();
	if value.len() > 8 * 1024 * 1024 || value.chars().any(char::is_control) {
		return false;
	}
	let lower = value.to_ascii_lowercase();
	["png", "jpeg", "gif", "webp"]
		.iter()
		.any(|format| lower.starts_with(&format!("data:image/{format};base64,")))
}

fn escape_html(value: &str) -> String {
	let mut escaped = String::with_capacity(value.len());
	for character in value.chars() {
		match character {
			'&' => escaped.push_str("&amp;"),
			'<' => escaped.push_str("&lt;"),
			'>' => escaped.push_str("&gt;"),
			'"' => escaped.push_str("&quot;"),
			'\'' => escaped.push_str("&#39;"),
			_ => escaped.push(character),
		}
	}
	escaped
}

fn escape_attribute(value: &str) -> String {
	escape_html(value)
}

fn safe_style_text(css: &str) -> String {
	// HTML tokenization recognizes a closing style tag even inside CSS strings.
	css.replace('<', "\\3C ")
}

const HTML_BASE_STYLE: &str = r#"
* { box-sizing: border-box; }
html { color-scheme: light; background: var(--page-background); }
body {
	margin: 0;
	background: var(--page-background);
	color: var(--text);
	font-family: "Iowan Old Style", "Palatino Linotype", Palatino, Georgia, serif;
	font-size: var(--body-size);
	line-height: 1.62;
	-webkit-font-smoothing: antialiased;
	text-rendering: optimizeLegibility;
}
body.theme-warm { font-family: Charter, "Bitstream Charter", Georgia, serif; }
body.theme-mono {
	font-family: ui-monospace, "SFMono-Regular", Consolas, "Liberation Mono", monospace;
	line-height: 1.58;
}
.sheet {
	width: min(76ch, calc(100% - 2rem));
	margin: 2.5rem auto 4rem;
	padding: clamp(1.6rem, 5vw, 4.5rem);
	background: var(--surface);
	box-shadow: 0 24px 70px rgb(49 42 32 / 10%);
}
.document-header { padding-bottom: 2rem; border-bottom: 1px solid var(--border); }
.eyebrow {
	margin: 0 0 .65rem;
	color: var(--accent);
	font: 650 .72rem/1.2 ui-sans-serif, system-ui, sans-serif;
	letter-spacing: .15em;
	text-transform: uppercase;
}
h1 {
	max-width: 22ch;
	margin: 0;
	font-size: clamp(2rem, 6vw, 3.65rem);
	font-weight: 560;
	letter-spacing: -.035em;
	line-height: 1.04;
}
.metadata {
	display: flex;
	flex-wrap: wrap;
	gap: .8rem 1.5rem;
	margin: 1.5rem 0 0;
	font: .78rem/1.4 ui-sans-serif, system-ui, sans-serif;
}
.metadata__item { min-width: 7rem; }
.metadata dt { color: var(--muted); font-size: .68rem; letter-spacing: .08em; text-transform: uppercase; }
.metadata dd { margin: .15rem 0 0; color: var(--text); }
.metadata code { padding: 0; background: none; font-size: .92em; }
.conversation { margin-top: .5rem; }
.message {
	position: relative;
	margin: 0;
	padding: 2rem 0 2rem 1.4rem;
	border-bottom: 1px solid var(--border);
}
.message::before {
	position: absolute;
	top: 2rem;
	bottom: 2rem;
	left: 0;
	width: 2px;
	background: var(--accent);
	content: "";
}
.message.role-user { margin-block: .35rem; background: linear-gradient(90deg, var(--user-surface), transparent 72%); }
.message.phase-commentary { color: var(--muted); }
.message.phase-commentary::before { background: var(--border); }
.message__header {
	display: flex;
	align-items: baseline;
	justify-content: space-between;
	gap: 1rem;
	margin-bottom: .8rem;
	font-family: ui-sans-serif, system-ui, sans-serif;
}
.message__header h2 {
	margin: 0;
	color: var(--accent);
	font-size: .75rem;
	font-weight: 700;
	letter-spacing: .1em;
	text-transform: uppercase;
}
.phase-commentary .message__header h2 { color: var(--muted); }
.message__header time { color: var(--muted); font-size: .7rem; white-space: nowrap; }
.message__body > :first-child { margin-top: 0; }
.message__body > :last-child { margin-bottom: 0; }
.message__body p { margin: 0 0 .9em; }
.message__body h1, .message__body h2, .message__body h3, .message__body h4 {
	margin: 1.5em 0 .55em;
	font-family: ui-sans-serif, system-ui, sans-serif;
	font-weight: 650;
	letter-spacing: -.015em;
	line-height: 1.22;
}
.message__body h1 { font-size: 1.55em; }
.message__body h2 { font-size: 1.35em; }
.message__body h3 { font-size: 1.16em; }
a { color: var(--accent); text-decoration-thickness: .08em; text-underline-offset: .18em; overflow-wrap: anywhere; }
code, pre { font-family: ui-monospace, "SFMono-Regular", Consolas, "Liberation Mono", monospace; }
code { padding: .13em .34em; border-radius: .28em; background: var(--code-surface); font-size: .88em; }
pre {
	max-width: 100%;
	margin: 1.2em 0;
	padding: 1rem 1.1rem;
	overflow-x: auto;
	border: 1px solid var(--border);
	border-radius: .45rem;
	background: var(--code-surface);
	font-size: .82em;
	line-height: 1.55;
	tab-size: 4;
}
pre code { padding: 0; background: none; font-size: inherit; }
blockquote {
	margin: 1.2em 0;
	padding: .05em 0 .05em 1.1em;
	border-left: 2px solid var(--border);
	color: var(--muted);
}
ul, ol { padding-left: 1.45em; }
li + li { margin-top: .28em; }
table { width: 100%; margin: 1.25em 0; border-collapse: collapse; font-size: .9em; }
th, td { padding: .55em .65em; border: 1px solid var(--border); text-align: left; vertical-align: top; }
th { background: var(--user-surface); font-family: ui-sans-serif, system-ui, sans-serif; font-weight: 650; }
hr { margin: 1.7em 0; border: 0; border-top: 1px solid var(--border); }
.empty { color: var(--muted); font-style: italic; }
.empty-state {
	margin: 2rem 0;
	padding: 1rem 1.2rem;
	border: 1px solid var(--border);
	border-radius: .45rem;
	color: var(--muted);
	background: var(--surface);
	font-style: italic;
}
.document-footer {
	padding-top: 1.2rem;
	color: var(--muted);
	font: .7rem/1.4 ui-sans-serif, system-ui, sans-serif;
	letter-spacing: .08em;
	text-align: right;
	text-transform: uppercase;
}
@media (max-width: 640px) {
	.sheet { width: 100%; margin: 0; padding: 1.5rem 1.1rem 3rem; box-shadow: none; }
	h1 { font-size: 2.25rem; }
	.message { padding-left: 1rem; }
	.message__header { display: block; }
	.message__header time { display: block; margin-top: .25rem; }
}
@media print {
	html, body { background: #FFF; }
	body { font-size: var(--body-size); }
	.sheet { width: auto; max-width: none; margin: 0; padding: 0; background: #FFF; box-shadow: none; }
	.document-header { break-after: avoid-page; }
	.message { break-inside: auto; }
	.message__header, pre, blockquote, table, tr, img { break-inside: avoid; }
	h1, h2, h3, h4 { break-after: avoid-page; }
	p, li { orphans: 3; widows: 3; }
	pre { overflow: visible; white-space: pre-wrap; overflow-wrap: anywhere; }
	a { color: inherit; }
	.document-footer { display: none; }
}
"#;

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use std::str::FromStr;

	use crate::domain::{ThreadId, ThreadSummary};

	use super::*;

	fn chat_with(text: &str) -> Chat {
		Chat {
			thread: ThreadSummary {
				id: ThreadId::from_str("019f874c-1e53-7d12-97db-fac3249ce327").unwrap(),
				title: "Safety <check>".to_owned(),
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
			messages: vec![ChatMessage {
				role: MessageRole::User,
				phase: MessagePhase::User,
				text: text.to_owned(),
				timestamp: None,
			}],
			warnings: Vec::new(),
		}
	}

	#[test]
	fn raw_html_and_hidden_looking_tags_render_as_text() {
		let chat = chat_with(
			"<environment_context>keep this visible</environment_context>\n\n<script>alert('no')</script>",
		);

		let output = render(&chat, &RenderOptions::default());
		assert!(
			output.contains(
				"&lt;environment_context&gt;keep this visible&lt;/environment_context&gt;"
			)
		);
		assert!(output.contains("&lt;script&gt;alert('no')&lt;/script&gt;"));
		assert!(!output.contains("<script>"));
		assert!(output.contains("<title>Safety</title>"));
	}

	#[test]
	fn unsafe_links_and_remote_images_are_not_emitted() {
		let chat = chat_with(
			"[safe](https://example.com) [unsafe](javascript:alert(1)) ![remote](https://example.com/a.png)",
		);

		let output = render(&chat, &RenderOptions::default());
		assert!(output.contains("href=\"https://example.com\""));
		assert!(!output.contains("javascript:"));
		assert!(!output.contains("<img"));
		assert!(output.contains("unsafe"));
		assert!(output.contains("remote"));
	}

	#[test]
	fn custom_css_cannot_close_the_style_element() {
		let options = RenderOptions {
			custom_css: Some("</style><script>alert(1)</script>".to_owned()),
			..RenderOptions::default()
		};

		let output = render(&chat_with("Hello"), &options);
		assert_eq!(output.matches("</style>").count(), 1);
		assert!(!output.contains("<script>"));
		assert!(output.contains("\\3C /style>"));
	}

	#[test]
	fn empty_transcripts_have_a_clear_empty_state() {
		let mut chat = chat_with("");
		chat.messages.clear();

		let output = render(&chat, &RenderOptions::default());

		assert!(output.contains("No visible messages matched this export."));
	}
}
