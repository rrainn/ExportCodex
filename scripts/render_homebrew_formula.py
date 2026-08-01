#!/usr/bin/env python3
"""Render a deterministic Homebrew formula from release artifact metadata."""

from __future__ import annotations

import argparse
import re
from pathlib import Path
from urllib.parse import urlparse


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_TEMPLATE = REPOSITORY_ROOT / "packaging" / "homebrew" / "export-codex.rb.template"
SEMVER_PATTERN = re.compile(
	r"^(0|[1-9][0-9]*)\."
	r"(0|[1-9][0-9]*)\."
	r"(0|[1-9][0-9]*)"
	r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
CHECKSUM_PATTERN = re.compile(r"^[0-9a-f]{64}$")
PLACEHOLDER_PATTERN = re.compile(r"\{\{[A-Z0-9_]+\}\}")

TARGETS = {
	"DARWIN_ARM64": "aarch64-apple-darwin",
	"DARWIN_X64": "x86_64-apple-darwin",
	"LINUX_ARM64": "aarch64-unknown-linux-musl",
	"LINUX_X64": "x86_64-unknown-linux-musl",
}


class FormulaRenderError(ValueError):
	"""Describe invalid release metadata without exposing implementation details."""


def validate_version(version: str) -> None:
	"""Require a Cargo-compatible release version without build metadata."""
	if not SEMVER_PATTERN.fullmatch(version):
		raise FormulaRenderError(
			f"version must be semantic versioning without build metadata: {version!r}"
		)


def validate_tag(tag: str, version: str) -> None:
	"""Keep archive URLs and the Cargo package version on the same release."""
	if tag != f"v{version}":
		raise FormulaRenderError(f"tag must be exactly v{version}: {tag!r}")


def validate_checksum(name: str, checksum: str) -> None:
	"""Reject malformed or uppercase values before they reach Homebrew."""
	if not CHECKSUM_PATTERN.fullmatch(checksum):
		raise FormulaRenderError(f"{name} must be a lowercase 64-character SHA256")


def validate_asset_url_prefix(prefix: str) -> None:
	"""Allow immutable HTTPS releases and local file URLs used by preflight tests."""
	parsed = urlparse(prefix)
	if parsed.scheme not in {"https", "file"}:
		raise FormulaRenderError("asset URL prefix must use https:// or file://")
	if parsed.scheme == "https" and not parsed.netloc:
		raise FormulaRenderError("HTTPS asset URL prefix must include a host")
	if any(character in prefix for character in {'"', "\\", "\n", "\r"}):
		raise FormulaRenderError("asset URL prefix contains an unsafe character")


def render_formula(
	*,
	version: str,
	tag: str,
	checksums: dict[str, str],
	asset_url_prefix: str | None = None,
	template_path: Path = DEFAULT_TEMPLATE,
) -> str:
	"""Return a validated formula with stable whitespace and ordering."""
	validate_version(version)
	validate_tag(tag, version)
	if set(checksums) != set(TARGETS):
		missing = sorted(set(TARGETS) - set(checksums))
		extra = sorted(set(checksums) - set(TARGETS))
		raise FormulaRenderError(f"checksum platforms differ; missing={missing}, extra={extra}")
	for platform, checksum in checksums.items():
		validate_checksum(platform, checksum)

	prefix = asset_url_prefix or f"https://github.com/rrainn/ExportCodex/releases/download/{tag}"
	prefix = prefix.rstrip("/")
	validate_asset_url_prefix(prefix)

	template = template_path.read_text(encoding="utf-8")
	replacements = {"{{VERSION}}": version}
	for platform, target in TARGETS.items():
		archive = f"export-codex-v{version}-{target}.tar.gz"
		replacements[f"{{{{{platform}_URL}}}}"] = f"{prefix}/{archive}"
		replacements[f"{{{{{platform}_SHA256}}}}"] = checksums[platform]

	for placeholder, replacement in replacements.items():
		# A count check prevents a template typo from silently omitting a platform.
		count = template.count(placeholder)
		if count != 1:
			raise FormulaRenderError(
				f"template must contain {placeholder} exactly once; found {count}"
			)
		template = template.replace(placeholder, replacement)

	unresolved = sorted(set(PLACEHOLDER_PATTERN.findall(template)))
	if unresolved:
		raise FormulaRenderError(f"template contains unresolved placeholders: {unresolved}")
	if len(re.findall(r'^\s*sha256 "[0-9a-f]{64}"$', template, re.MULTILINE)) != 4:
		raise FormulaRenderError("rendered formula must contain exactly four SHA256 entries")

	return template.rstrip() + "\n"


def parse_arguments() -> argparse.Namespace:
	"""Build the command-line contract shared by CI and local release checks."""
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--version", required=True)
	parser.add_argument("--tag", required=True)
	parser.add_argument("--darwin-arm64-sha256", required=True)
	parser.add_argument("--darwin-x64-sha256", required=True)
	parser.add_argument("--linux-arm64-sha256", required=True)
	parser.add_argument("--linux-x64-sha256", required=True)
	parser.add_argument(
		"--asset-url-prefix",
		help="Override the GitHub release URL with a local file URL for preflight tests.",
	)
	parser.add_argument("--template", type=Path, default=DEFAULT_TEMPLATE)
	parser.add_argument("--output", type=Path, required=True)
	return parser.parse_args()


def main() -> int:
	"""Render the formula atomically enough for a fresh CI workspace."""
	arguments = parse_arguments()
	checksums = {
		"DARWIN_ARM64": arguments.darwin_arm64_sha256,
		"DARWIN_X64": arguments.darwin_x64_sha256,
		"LINUX_ARM64": arguments.linux_arm64_sha256,
		"LINUX_X64": arguments.linux_x64_sha256,
	}
	try:
		formula = render_formula(
			version=arguments.version,
			tag=arguments.tag,
			checksums=checksums,
			asset_url_prefix=arguments.asset_url_prefix,
			template_path=arguments.template,
		)
	except (FormulaRenderError, OSError) as error:
		raise SystemExit(f"error: {error}") from error

	arguments.output.parent.mkdir(parents=True, exist_ok=True)
	# Explicit newlines keep the committed tap formula identical on every host.
	with arguments.output.open("w", encoding="utf-8", newline="\n") as output_file:
		output_file.write(formula)
	print(arguments.output)
	return 0


if __name__ == "__main__":
	raise SystemExit(main())

