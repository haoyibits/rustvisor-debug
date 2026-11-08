import copy
from pathlib import Path
import tempfile
import unittest

from links import check_html, destinations, preprocess


class GuideLinks(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.context = {"root": str(self.root), "config": {"book": {"src": "."}}}

    def page(self, path, content, output=None):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(content, encoding="utf-8")
        return {"Chapter": {
            "name": path, "path": output or path, "source_path": path,
            "content": content, "sub_items": [],
        }}

    def test_only_real_links_count_including_table_and_reference(self):
        content = (
            "[one][ref]\n\n[ref]: real.md#part\n\n"
            "`[fake](inline.md)`\n\n```md\n[fake](fenced.md)\n```\n\n"
            "| topic |\n| --- |\n| [two](table.md) |\n"
        )
        self.assertEqual(list(destinations(content)), ["real.md#part", "table.md"])

    def test_nested_index_aliases_and_deduplicated_backlinks(self):
        home = self.page("README.md", "# Home\n[child](topic/README.md)\n", "index.md")
        child = self.page("topic/README.md", "# Topic\n[home](../README.md) [again](../index.md)\n", "topic/index.md")
        home["Chapter"]["sub_items"] = [child]
        book = {"sections": [home]}
        original = copy.deepcopy(book)
        result = preprocess(self.context, book)
        content = result["sections"][0]["Chapter"]["content"]
        self.assertEqual(content.count('href="topic/index.md"'), 1)
        self.assertIn('href="../index.md"', child["Chapter"]["content"])
        self.assertEqual((self.root / "README.md").read_text(), original["sections"][0]["Chapter"]["content"])

    def test_self_link_does_not_create_a_backlink(self):
        page = self.page("a.md", "# A\n[self](a.md) [heading](#a)\n")
        result = preprocess(self.context, {"sections": [page]})
        self.assertNotIn("引用此页", result["sections"][0]["Chapter"]["content"])

    def test_english_backlinks_and_content_language(self):
        home = self.page("README.md", "[English](en/README.md)", "index.md")
        english = self.page("en/README.md", "# Guide", "en/index.md")
        preprocess(self.context, {"sections": [home, english]})
        content = english["Chapter"]["content"]
        self.assertIn('aria-label="Referenced by"', content)
        self.assertIn('<div lang="en">', content)
        self.assertIn('href="../index.md"', content)

    def test_missing_link_fails(self):
        page = self.page("a.md", "[missing](gone.md)")
        with self.assertRaisesRegex(ValueError, "missing local link"):
            preprocess(self.context, {"sections": [page]})

    def test_unlisted_chapter_fails(self):
        page = self.page("a.md", "# A")
        self.page("forgotten.md", "# Forgotten")
        with self.assertRaisesRegex(ValueError, "missing from SUMMARY"):
            preprocess(self.context, {"sections": [page]})

    def test_duplicate_chapter_fails(self):
        page = self.page("a.md", "# A")
        with self.assertRaisesRegex(ValueError, "included twice"):
            preprocess(self.context, {"sections": [page, copy.deepcopy(page)]})

    def test_missing_asset_fails(self):
        page = self.page("a.md", "![diagram](missing.svg)")
        with self.assertRaisesRegex(ValueError, "missing local link"):
            preprocess(self.context, {"sections": [page]})

    def test_local_source_link_outside_book_fails(self):
        page = self.page("a.md", "[source](../src/main.rs)")
        with self.assertRaisesRegex(ValueError, "leaves the book"):
            preprocess(self.context, {"sections": [page]})

    def test_rendered_unicode_anchor_and_missing_fragment(self):
        (self.root / "index.html").write_text('<a href="b.html#%E4%B8%AD%E6%96%87">B</a>')
        (self.root / "b.html").write_text('<h1 id="中文">B</h1>')
        self.assertEqual(check_html(self.root), (2, 1))
        (self.root / "b.html").write_text('<h1 id="other">B</h1>')
        with self.assertRaisesRegex(ValueError, "missing rendered anchor"):
            check_html(self.root)

    def test_rendered_missing_page_fails(self):
        (self.root / "index.html").write_text('<a href="missing.html">B</a>')
        with self.assertRaisesRegex(ValueError, "missing rendered link"):
            check_html(self.root)

    def test_rendered_site_root_link_resolves_inside_output(self):
        (self.root / "index.html").write_text('<h1 id="home">Home</h1>')
        (self.root / "404.html").write_text('<a href="/#home">Home</a>')
        self.assertEqual(check_html(self.root), (2, 1))


if __name__ == "__main__":
    unittest.main()
