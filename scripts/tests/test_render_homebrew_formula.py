"""Behavior tests for the release formula renderer."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts.render_homebrew_formula import (
	DEFAULT_TEMPLATE,
	FormulaRenderError,
	TARGETS,
	render_formula,
)


CHECKSUMS = {
	"DARWIN_ARM64": "0" * 64,
	"DARWIN_X64": "1" * 64,
	"LINUX_ARM64": "2" * 64,
	"LINUX_X64": "3" * 64,
}


class RenderFormulaTests(unittest.TestCase):
	"""Verify release metadata becomes a complete, reproducible formula."""

	def test_renders_all_platforms_deterministically(self) -> None:
		"""The same immutable inputs should always produce the same tap file."""
		first = render_formula(version="1.2.3", tag="v1.2.3", checksums=CHECKSUMS)
		second = render_formula(version="1.2.3", tag="v1.2.3", checksums=CHECKSUMS)

		self.assertEqual(first, second)
		self.assertTrue(first.endswith("\n"))
		self.assertNotIn("{{", first)
		self.assertEqual(first.count("sha256 \""), 4)
		self.assertIn('"type":"session_meta"', first)
		self.assertIn("Homebrew visible agent response", first)
		for target in TARGETS.values():
			self.assertIn(f"export-codex-v1.2.3-{target}.tar.gz", first)

	def test_local_asset_prefix_supports_homebrew_preflight(self) -> None:
		"""Release checks must install archives before GitHub publishes them."""
		formula = render_formula(
			version="1.2.3-rc.1",
			tag="v1.2.3-rc.1",
			checksums=CHECKSUMS,
			asset_url_prefix="file:///tmp/export-codex-assets/",
		)

		self.assertIn("file:///tmp/export-codex-assets/export-codex-v1.2.3-rc.1", formula)

	def test_rejects_tag_that_does_not_match_version(self) -> None:
		"""A mismatched tag would otherwise publish permanently broken URLs."""
		with self.assertRaisesRegex(FormulaRenderError, "tag must be exactly"):
			render_formula(version="1.2.3", tag="v1.2.4", checksums=CHECKSUMS)

	def test_rejects_malformed_checksum(self) -> None:
		"""Homebrew should fail during rendering instead of after publication."""
		checksums = dict(CHECKSUMS)
		checksums["LINUX_X64"] = "ABC123"

		with self.assertRaisesRegex(FormulaRenderError, "lowercase 64-character"):
			render_formula(version="1.2.3", tag="v1.2.3", checksums=checksums)

	def test_rejects_incomplete_platform_set(self) -> None:
		"""Every supported Homebrew platform needs an independently verified asset."""
		checksums = dict(CHECKSUMS)
		del checksums["LINUX_ARM64"]

		with self.assertRaisesRegex(FormulaRenderError, "checksum platforms differ"):
			render_formula(version="1.2.3", tag="v1.2.3", checksums=checksums)

	def test_rejects_insecure_asset_url(self) -> None:
		"""Published formulas must not download release binaries over plaintext HTTP."""
		with self.assertRaisesRegex(FormulaRenderError, "https:// or file://"):
			render_formula(
				version="1.2.3",
				tag="v1.2.3",
				checksums=CHECKSUMS,
				asset_url_prefix="http://example.com/assets",
			)

	def test_rejects_unresolved_template_placeholder(self) -> None:
		"""New template fields must be wired into the renderer intentionally."""
		with tempfile.TemporaryDirectory() as temporary_directory:
			template_path = Path(temporary_directory) / "formula.rb.template"
			template_path.write_text(
				DEFAULT_TEMPLATE.read_text(encoding="utf-8") + "{{UNKNOWN}}\n",
				encoding="utf-8",
			)
			with self.assertRaisesRegex(FormulaRenderError, "unresolved placeholders"):
				render_formula(
					version="1.2.3",
					tag="v1.2.3",
					checksums=CHECKSUMS,
					template_path=template_path,
				)


if __name__ == "__main__":
	unittest.main()
