"""Message_Formatter: Markdown -> Slack mrkdwn, and length chunking.

Two pure functions, no I/O. This is the only module with real algorithmic risk,
so it carries the bulk of the test suite.
"""

from __future__ import annotations

import re

MAX_MESSAGE_CHARS = 3800

# Fenced code blocks are extracted before any conversion and re-inserted
# untouched. An unterminated fence swallows the rest of the text, which is what
# a Slack reader sees anyway.
_FENCE = re.compile(r"(```.*?```|```.*\Z)", re.S)

# One alternation, one pass: converted output is never rescanned, so `**x**`
# cannot degrade into `_*x*_`.
_INLINE = re.compile(
    r"\[([^\]\n]+)\]\(([^)\s]+)\)"  # 1,2 link
    r"|\*\*(.+?)\*\*"  # 3   **bold**
    r"|__(.+?)__"  # 4   __bold__
    r"|(?<!\*)\*([^*\n]+)\*(?!\*)"  # 5   *italic*
    r"|(?<![\w_])_([^_\n]+)_(?![\w_])",  # 6   _italic_
    re.S,
)

_HEADING = re.compile(r"^#{1,6}[ \t]+(.+?)[ \t]*$", re.M)

_INLINE_CODE = re.compile(r"`[^`\n]+`")


def _sub_inline(m: re.Match[str]) -> str:
    if m.group(1) is not None:
        return f"<{m.group(2)}|{m.group(1)}>"
    if m.group(3) is not None:
        return f"*{m.group(3)}*"
    if m.group(4) is not None:
        return f"*{m.group(4)}*"
    if m.group(5) is not None:
        return f"_{m.group(5)}_"
    return f"_{m.group(6)}_"


def _convert(segment: str) -> str:
    # Inline code spans are literal too, on the same extract/restore trick as
    # fences but cheap enough to inline here.
    spans: list[str] = []

    def stash(m: re.Match[str]) -> str:
        spans.append(m.group(0))
        return f"\x00{len(spans) - 1}\x00"

    segment = _INLINE_CODE.sub(stash, segment)
    segment = _INLINE.sub(_sub_inline, segment)
    segment = _HEADING.sub(r"*\1*", segment)
    return re.sub(r"\x00(\d+)\x00", lambda m: spans[int(m.group(1))], segment)


def to_mrkdwn(text: str) -> str:
    """Convert model Markdown to Slack mrkdwn, leaving code fences byte-for-byte."""
    return "".join(
        part if i % 2 else _convert(part) for i, part in enumerate(_FENCE.split(text))
    )


_FENCE_LINE = re.compile(r"^```(\S*)", re.M)


def _fence_state(s: str) -> str | None:
    """Fence state (None = closed, str = open with that language tag) at the end of `s`.

    `s` is always a complete chunk — a chunk continuing a fence carries its own
    reopening fence — so the scan starts from "closed" every time.
    """
    open_lang: str | None = None
    for m in _FENCE_LINE.finditer(s):
        open_lang = None if open_lang is not None else m.group(1)
    return open_lang


def chunk(text: str, limit: int = MAX_MESSAGE_CHARS) -> list[str]:
    """Split `text` into Slack-sized pieces, reopening code fences across breaks."""
    if len(text) <= limit:
        return [text] if text else [""]

    out: list[str] = []
    open_lang: str | None = None
    rest = text

    while rest:
        prefix = "" if open_lang is None else f"```{open_lang}\n"
        # Reserve room for the prefix and a possible closing fence, so a chunk
        # never exceeds the limit even when a fence has to be repaired.
        budget = max(1, limit - len(prefix) - 4)
        if len(rest) <= budget:
            piece, rest = rest, ""
        else:
            cut = budget
            for sep in ("\n\n", "\n", " "):
                idx = rest.rfind(sep, 0, budget)
                if idx > 0:
                    cut = idx + len(sep)
                    break
            piece, rest = rest[:cut], rest[cut:]

        body = prefix + piece
        open_lang = _fence_state(body)
        if open_lang is not None and rest:
            body += "\n```"
        elif open_lang is not None:
            # Trailing unterminated fence in the input: leave it as the model
            # wrote it rather than inventing a close.
            open_lang = None
        out.append(body)

    return out
