import re

from arlo_slack.mrkdwn import chunk, to_mrkdwn


def test_bold_does_not_degrade_to_italic():
    assert to_mrkdwn("**bold**") == "*bold*"
    assert to_mrkdwn("__bold__") == "*bold*"


def test_italic():
    assert to_mrkdwn("*i* and _j_") == "_i_ and _j_"


def test_mixed_bold_and_italic():
    assert to_mrkdwn("**b** then *i*") == "*b* then _i_"


def test_links():
    assert to_mrkdwn("[docs](https://x.dev)") == "<https://x.dev|docs>"


def test_headings():
    assert to_mrkdwn("# Title\nbody") == "*Title*\nbody"
    assert to_mrkdwn("### Sub") == "*Sub*"


def test_code_fence_untouched():
    src = "before **b**\n```python\nx = **not bold**\n# not a heading\n```\nafter *i*"
    out = to_mrkdwn(src)
    assert "```python\nx = **not bold**\n# not a heading\n```" in out
    assert "*b*" in out and "_i_" in out


def test_inline_code_untouched():
    assert to_mrkdwn("use `**kwargs` here") == "use `**kwargs` here"


def test_chunk_short_text_is_one_chunk():
    assert chunk("hello", 100) == ["hello"]


def test_chunk_respects_limit_and_word_boundaries():
    text = " ".join("word%d" % i for i in range(500))
    parts = chunk(text, 200)
    assert all(len(p) <= 200 for p in parts)
    for p in parts:
        for token in p.split():
            assert re.fullmatch(r"word\d+", token), token


def test_chunk_prefers_paragraph_breaks():
    text = ("a" * 50 + "\n\n") * 10
    parts = chunk(text, 120)
    assert all(p.endswith("\n\n") or p == parts[-1] for p in parts)


def test_chunk_rejoins_to_input_when_no_fences():
    text = "\n\n".join("para %d %s" % (i, "x" * 60) for i in range(20))
    assert "".join(chunk(text, 150)) == text


def test_chunk_reopens_code_fence_with_language():
    body = "\n".join("line %d" % i for i in range(200))
    text = f"intro\n\n```python\n{body}\n```\ntail"
    parts = chunk(text, 300)
    assert len(parts) > 1
    # Every chunk that leaves a fence open closes it, and the next reopens it.
    for i, p in enumerate(parts[:-1]):
        if p.count("```") % 2 == 1:
            assert p.endswith("```")
            assert parts[i + 1].startswith("```python\n")
    # Content survives once repair fences are removed.
    rejoined = "".join(parts).replace("\n```", "").replace("```python\n", "", 100)
    assert "line 199" in rejoined and "intro" in rejoined
