#!/usr/bin/env python3
"""Render the shipped Markdown documents to PDF for the release archive.

`README.md` was authored for print: it carries 17
`<div style="page-break-after: always;">` markers and 16 screenshots. Neither
survives a plain-text reader, and a user who opens the file in Notepad sees
`![Alt text](images/image-1.png)` where the screenshot of their session summary
should be. The PDF is what makes the manual readable to someone who does not
know what a `.md` file is; the Markdown ships alongside it because GitHub
renders it and because it is greppable.

WeasyPrint rather than pandoc/LaTeX: it honours the `page-break-after` CSS the
document already contains, needs no TeX distribution, and resolves the relative
`images/` paths against `base_url`.

Usage:  python3 .github/md2pdf.py README.md PROGRAM_OVERVIEW.md
Writes: README.pdf, PROGRAM_OVERVIEW.pdf  (beside their sources)
"""

import pathlib
import re
import sys

import markdown
from weasyprint import CSS, HTML


def github_slugify(value: str, separator: str) -> str:
    """Heading -> anchor, by GitHub's rule rather than python-markdown's.

    The documents' own links (`[Features](#features)`) are written for GitHub,
    which renders these files on the repository page. The PDF has to agree with
    them or its table of contents is 36 dead links.

    The two algorithms differ in exactly one place, and it bites here: GitHub
    lowercases, drops everything that is not alphanumeric/`-`/`_`/space, and
    turns spaces into hyphens -- leaving runs of hyphens intact.
    python-markdown's default additionally collapses `---` to `-`, so
    "2. Using the Diagnostic Test Mode (`--test`)" anchors as
    `...-mode-test` there and `...-mode---test` on GitHub. Only the second
    matches what the README links to.
    """
    value = value.lower()
    value = re.sub(r"[^\w\s-]", "", value)
    return re.sub(r"\s", separator, value.strip())

# Deliberately plain: this needs to read like a manual, not like a web page.
# `@page` gives WeasyPrint the sheet size and margins; everything the document
# itself specifies (the page-break divs) still wins.
STYLESHEET = """
@page { size: A4; margin: 18mm 16mm; }
body {
    font-family: "DejaVu Sans", "Helvetica", sans-serif;
    font-size: 10pt;
    line-height: 1.45;
    color: #111;
}
h1 { font-size: 20pt; margin: 0 0 0.6em; }
h2 { font-size: 15pt; margin: 1.4em 0 0.5em; border-bottom: 1px solid #ccc;
     padding-bottom: 0.2em; }
h3 { font-size: 12pt; margin: 1.2em 0 0.4em; }
h4 { font-size: 11pt; margin: 1em 0 0.3em; }
h1, h2, h3, h4 { page-break-after: avoid; }
code, pre { font-family: "DejaVu Sans Mono", monospace; font-size: 8.5pt; }
code { background: #f2f2f2; padding: 0.1em 0.3em; border-radius: 2px; }
pre { background: #f6f6f6; border: 1px solid #e0e0e0; border-radius: 3px;
      padding: 0.7em; white-space: pre-wrap; word-wrap: break-word; }
pre code { background: none; padding: 0; }
/* Screenshots are the point of the manual -- never let one overflow the
   sheet, and never split one across a page boundary. */
img { max-width: 100%; page-break-inside: avoid; }
table { border-collapse: collapse; width: 100%; font-size: 9pt;
        page-break-inside: avoid; }
th, td { border: 1px solid #ccc; padding: 0.35em 0.5em; text-align: left;
         vertical-align: top; }
th { background: #f2f2f2; }
blockquote { border-left: 3px solid #bbb; margin: 1em 0; padding: 0.2em 1em;
             background: #fafafa; }
a { color: #14507a; text-decoration: none; }
"""


def render(src: pathlib.Path) -> pathlib.Path:
    html_body = markdown.markdown(
        src.read_text(encoding="utf-8"),
        # `tables` for the argument/filter/archive tables, `fenced_code` for the
        # ``` blocks, `sane_lists` so the numbered Getting-started steps do not
        # restart, `attr_list` because the source uses raw HTML attributes, and
        # `toc` purely for the `id` it puts on every heading -- without it the
        # contents page renders as text that looks like links and goes nowhere.
        extensions=["tables", "fenced_code", "sane_lists", "attr_list", "toc"],
        extension_configs={"toc": {"slugify": github_slugify}},
    )
    out = src.with_suffix(".pdf")
    # base_url is what resolves `images/image-1.png` -- relative to the document,
    # not to the working directory the build happens to run in.
    HTML(string=html_body, base_url=str(src.parent.resolve())).write_pdf(
        out, stylesheets=[CSS(string=STYLESHEET)]
    )
    return out


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(f"usage: {argv[0]} <file.md> [file.md ...]", file=sys.stderr)
        return 2
    for name in argv[1:]:
        src = pathlib.Path(name)
        if not src.is_file():
            print(f"[ERROR] no such file: {src}", file=sys.stderr)
            return 1
        out = render(src)
        print(f"{src} -> {out} ({out.stat().st_size // 1024} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
