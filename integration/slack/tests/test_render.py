import asyncio

from arlo_slack import render


class FakeSlack:
    def __init__(self):
        self.posts: list[str] = []
        self.updates: list[tuple[str, str]] = []
        self._n = 0

    async def chat_postMessage(self, channel, thread_ts, text):
        self._n += 1
        self.posts.append(text)
        return {"ts": f"ts{self._n}"}

    async def chat_update(self, channel, ts, text):
        self.updates.append((ts, text))
        return {"ok": True}

    @property
    def final(self) -> str:
        return self.updates[-1][1] if self.updates else self.posts[-1]


async def feed(events):
    for e in events:
        yield e


def text(delta):
    return {"type": "TEXT_MESSAGE_CONTENT", "delta": delta}


async def run(events, **kw):
    slack = FakeSlack()
    kw.setdefault("interval", 0)
    count = await render.render(slack, "C1", "1.0", feed(events), **kw)
    return slack, count


async def test_placeholder_posted_before_any_event():
    slack, _ = await run([])
    assert slack.posts[0] == render.PLACEHOLDER


async def test_successful_run_renders_final_answer():
    slack, count = await run(
        [
            {"type": "RUN_STARTED"},
            {"type": "STEP_STARTED", "stepName": "turn-1"},
            text("Hello "),
            text("**world**"),
            {"type": "TEXT_MESSAGE_END"},
            {"type": "RUN_FINISHED", "outcome": {"type": "success"}},
        ]
    )
    assert slack.final == "Hello *world*"
    assert count == 6


async def test_unclosed_step_run_error_is_treated_as_success():
    slack, _ = await run(
        [
            text("answer"),
            {
                "type": "RUN_ERROR",
                "message": 'RUN_FINISHED with active steps: {"turn-1"}',
            },
        ]
    )
    assert slack.final == "answer"


async def test_run_error_preserves_partial_text():
    slack, _ = await run(
        [text("partial "), {"type": "RUN_ERROR", "message": "boom"}]
    )
    assert "partial" in slack.final
    assert "*Run failed:* boom" in slack.final


async def test_missing_terminal_event_is_reported():
    slack, _ = await run([text("partial")])
    assert "partial" in slack.final
    assert render.INCOMPLETE in slack.final


async def test_interrupt_outcome_reports_unsupported_approvals():
    slack, _ = await run(
        [{"type": "RUN_FINISHED", "outcome": {"type": "interrupt", "interrupts": []}}]
    )
    assert slack.final == render.INTERRUPT


async def test_empty_output_gets_a_notice():
    slack, _ = await run([{"type": "RUN_FINISHED", "outcome": {"type": "success"}}])
    assert slack.final == render.NO_OUTPUT


async def test_tool_status_line_shown_then_cleared():
    slack, _ = await run(
        [
            text("thinking"),
            {"type": "TOOL_CALL_START", "toolCallName": "web_search"},
            {"type": "TOOL_CALL_ARGS", "delta": "{}"},
        ]
    )
    assert any("running web_search" in t for _, t in slack.updates)
    assert "running web_search" not in slack.final


async def test_throttle_limits_updates():
    slack = FakeSlack()
    await render.render(
        slack, "C1", "1.0", feed([text("x") for _ in range(50)]), interval=60
    )
    # Placeholder post, then only the forced terminal flush.
    assert len(slack.posts) == 1
    assert len(slack.updates) == 1


async def test_timeout_reports_and_keeps_partial_text():
    async def slow():
        yield text("partial")
        await asyncio.sleep(10)

    slack = FakeSlack()
    await render.render(slack, "C1", "1.0", slow(), interval=0, timeout=0.05)
    assert "partial" in slack.final
    assert "timed out" in slack.final


async def test_transport_failure_is_reported():
    async def broken():
        raise ConnectionError("no route to arlo")
        yield  # pragma: no cover

    slack = FakeSlack()
    await render.render(slack, "C1", "1.0", broken(), interval=0)
    assert "Could not reach Arlo" in slack.final


async def test_long_output_spans_chunks_and_earlier_chunks_stop_changing():
    words = " ".join(f"w{i}" for i in range(400))
    slack, _ = await run(
        [text(words), {"type": "RUN_FINISHED", "outcome": {"type": "success"}}],
        max_chars=200,
    )
    assert len(slack.posts) > 1
    first_chunk_edits = [t for ts, t in slack.updates if ts == "ts1"]
    assert len(first_chunk_edits) <= 1


async def test_rate_limit_is_retried_without_losing_text():
    class Limited(FakeSlack):
        def __init__(self):
            super().__init__()
            self.tries = 0

        async def chat_update(self, channel, ts, text):
            self.tries += 1
            if self.tries == 1:
                err = RuntimeError("ratelimited")
                err.response = type(
                    "R", (), {"status_code": 429, "headers": {"Retry-After": "0"}}
                )()
                raise err
            return await super().chat_update(channel=channel, ts=ts, text=text)

    slack = Limited()
    await render.render(
        slack,
        "C1",
        "1.0",
        feed([text("kept"), {"type": "RUN_FINISHED", "outcome": {"type": "success"}}]),
        interval=0,
    )
    assert slack.final == "kept"
