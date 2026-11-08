#!/usr/bin/env python3
"""Validate mdBook chapters, append backlinks, or check rendered local links."""

import html
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import sys
from urllib.parse import unquote, urlsplit

from markdown_it import MarkdownIt


MARKDOWN = MarkdownIt("commonmark").enable("table")


def destinations(content):
    """Read actual Markdown links, including references, excluding code."""
    for block in MARKDOWN.parse(content):
        for token in block.children or []:
            if token.type == "link_open":
                yield token.attrGet("href")
            elif token.type == "image":
                yield token.attrGet("src")


def chapters(items):
    for item in items:
        if "Chapter" in item:
            chapter = item["Chapter"]
            if chapter.get("path") is not None:
                yield chapter
            yield from chapters(chapter.get("sub_items", []))


def local_target(base, destination):
    url = urlsplit(destination)
    if url.scheme or url.netloc:
        return None
    if url.path.startswith("/"):
        raise ValueError(f"use a relative local link: {destination}")
    return (base / unquote(url.path)).resolve() if url.path else None


def preprocess(context, book):
    root = Path(context["root"]).resolve()
    source = (root / context["config"]["book"].get("src", "src")).resolve()
    # mdBook 0.5 uses sections; 0.4 used items.
    items = book["sections"] if "sections" in book else book["items"]
    pages = list(chapters(items))
    aliases = {}
    sources = set()
    for index, chapter in enumerate(pages):
        path = chapter.get("source_path") or chapter["path"]
        file = (source / path).resolve()
        if file in sources:
            raise ValueError(f"chapter included twice: {path}")
        if not file.is_file():
            raise ValueError(f"missing chapter: {path}")
        sources.add(file)
        for alias in (path, chapter["path"]):
            resolved = (source / alias).resolve()
            if resolved in aliases and aliases[resolved] != index:
                raise ValueError(f"chapter output path collision: {alias}")
            aliases[resolved] = index

    unlisted = set(source.rglob("*.md")) - sources - {source / "SUMMARY.md"}
    if unlisted:
        names = ", ".join(str(path.relative_to(source)) for path in sorted(unlisted))
        raise ValueError(f"chapters missing from SUMMARY.md: {names}")

    incoming = [set() for _ in pages]
    for index, chapter in enumerate(pages):
        path = chapter.get("source_path") or chapter["path"]
        base = (source / path).parent
        for destination in destinations(chapter["content"]):
            target = local_target(base, destination)
            if target is None:
                continue
            if target in aliases:
                other = aliases[target]
                if other != index:
                    incoming[other].add(index)
            elif not target.is_relative_to(source):
                raise ValueError(f"{path}: local link leaves the book: {destination}")
            elif not target.is_file():
                raise ValueError(f"{path}: missing local link: {destination}")
            elif target.suffix == ".md":
                raise ValueError(f"{path}: link to an unlisted chapter: {destination}")

    for index, chapter in enumerate(pages):
        if not incoming[index]:
            continue
        entries = []
        for other in sorted(incoming[index]):
            page = pages[other]
            relative = os.path.relpath(page["path"], Path(chapter["path"]).parent)
            # HTML links avoid Markdown escaping of arbitrary chapter names.
            label = html.escape(page["name"])
            href = html.escape(Path(relative).as_posix(), quote=True)
            entries.append(f'<li><a href="{href}">{label}</a></li>')
        english = Path(chapter.get("source_path") or chapter["path"]).parts[0] == "en"
        heading = "Referenced by" if english else "引用此页"
        chapter["content"] += (
            f'\n\n<hr>\n<aside aria-label="{heading}">\n'
            f'<h2 id="referenced-by">{heading}</h2>\n<ul>\n'
            + "\n".join(entries)
            + "\n</ul>\n</aside>\n"
        )
    for chapter in pages:
        if Path(chapter.get("source_path") or chapter["path"]).parts[0] == "en":
            chapter["content"] = '<div lang="en">\n\n' + chapter["content"] + '\n\n</div>\n'
    return book


class Page(HTMLParser):
    def __init__(self, content):
        super().__init__()
        self.ids = set()
        self.links = []
        self.feed(content)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if attrs.get("id"):
            self.ids.add(attrs["id"])
        for name in ("href", "src"):
            if attrs.get(name):
                self.links.append(attrs[name])


def check_html(root):
    root = Path(root).resolve()
    pages = {path: Page(path.read_text(encoding="utf-8")) for path in root.rglob("*.html")}
    if not pages:
        raise ValueError(f"no HTML pages in {root}")
    count = 0
    for path, page in pages.items():
        for destination in page.links:
            url = urlsplit(destination)
            if url.scheme or url.netloc:
                continue
            if url.path.startswith("/"):
                target = (root / unquote(url.path).lstrip("/")).resolve()
            else:
                target = local_target(path.parent, destination) or path
            if target.is_dir():
                target /= "index.html"
            if not target.is_relative_to(root) or not target.is_file():
                raise ValueError(f"{path.name}: missing rendered link: {destination}")
            if url.fragment and target in pages:
                if unquote(url.fragment) not in pages[target].ids:
                    raise ValueError(f"{path.name}: missing rendered anchor: {destination}")
            count += 1
    return len(pages), count


def main():
    if sys.argv[1:2] == ["supports"]:
        return 0 if sys.argv[2:] == ["html"] else 1
    if sys.argv[1:2] == ["--check-html"] and len(sys.argv) == 3:
        pages, links = check_html(sys.argv[2])
        print(f"checked {pages} HTML pages and {links} local links/anchors")
        return 0
    if len(sys.argv) != 1:
        raise ValueError("usage: links.py [supports html | --check-html DIRECTORY]")
    context, book = json.load(sys.stdin)
    json.dump(preprocess(context, book), sys.stdout, ensure_ascii=False)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError) as error:
        print(f"guide links: {error}", file=sys.stderr)
        sys.exit(1)
