"""Run_Renderer: consume an AG-UI event stream into one live Slack message."""

from __future__ import annotations

import asyncio
import contextlib
import logging
import time
from collections.abc import AsyncIterator

from .mrkdwn import MAX_MESSAGE_CHARS, chunk, to_mrkdwn

log = logging.getLogger(__name__)

PLACEHOLDER = "_thinking…_"
NO_OUTPUT = "_(no output)_"
INTERRUPT = (
    "*The agent requested approval.* Approvals are not supported by this "
    "integration — check that the arlo container is running in serve mode."
)
INCOMPLETE = "*The stream ended without a result.*"

UPDATE_INTERVAL_SECONDS = 1.0
RUN_TIMEOUT_SECONDS = 300.0


class Renderer:
    """Owns the Slack message(s) for one run and the throttled edit loop."""

    def __init__(
        self,
        client,
        channel: str,
        thread_ts: str,
        *,
        interval: float = UPDATE_INTERVAL_SECONDS,
        max_chars: int = MAX_MESSAGE_CHARS,
    ) -> None:
        self.client = client
        self.channel = channel
        self.thread_ts = thread_ts
        self.interval = interval
        self.max_chars = max_chars
        self.text = ""
        # status and notice are written as mrkdwn already, so they are appended
        # after conversion — never through to_mrkdwn, which would eat their `*`.
        self.status = ""
        self.notice = ""
        self.ts: list[str] = []
        self.posted: list[str] = []
        self._last_flush = 0.0

    async def _call(self, method, **kwargs):
        """Slack call that honours Retry-After. The buffer is the source of truth,
        so a retried update never loses text."""
        for _ in range(5):
            try:
                return await method(**kwargs)
            except Exception as e:  # SlackApiError, but keep fakes in tests simple
                resp = getattr(e, "response", None)
                if getattr(resp, "status_code", None) != 429:
                    raise
                delay = float(resp.headers.get("Retry-After", 1))
                log.warning("slack rate limited, sleeping %.1fs", delay)
                await asyncio.sleep(delay)
        raise RuntimeError("slack rate limit: gave up after 5 retries")

    def _body(self) -> str:
        # Status and notice sit below the answer so the text does not jump
        # around as tools come and go.
        parts = [to_mrkdwn(self.text).rstrip(), self.notice, self.status]
        return "\n\n".join(p for p in parts if p) or PLACEHOLDER

    async def flush(self, force: bool = False) -> None:
        now = time.monotonic()
        if not force and now - self._last_flush < self.interval:
            return
        self._last_flush = now

        for i, part in enumerate(chunk(self._body(), self.max_chars)):
            if i < len(self.posted):
                # Earlier chunks stop changing once the text past them grows,
                # so this edits each chunk at most once more after it is full.
                if part != self.posted[i]:
                    await self._call(
                        self.client.chat_update,
                        channel=self.channel,
                        ts=self.ts[i],
                        text=part,
                    )
                    self.posted[i] = part
            else:
                resp = await self._call(
                    self.client.chat_postMessage,
                    channel=self.channel,
                    thread_ts=self.thread_ts,
                    text=part,
                )
                self.ts.append(resp["ts"])
                self.posted.append(part)


async def render(
    client,
    channel: str,
    thread_ts: str,
    events: AsyncIterator[dict],
    *,
    interval: float = UPDATE_INTERVAL_SECONDS,
    max_chars: int = MAX_MESSAGE_CHARS,
    timeout: float = RUN_TIMEOUT_SECONDS,
) -> int:
    """Post a placeholder, stream `events` into it, and return the event count."""
    r = Renderer(client, channel, thread_ts, interval=interval, max_chars=max_chars)
    await r.flush(force=True)

    count = 0
    error: str | None = None
    terminal = False

    try:
        async with asyncio.timeout(timeout):
            async with contextlib.aclosing(events) as stream:
                async for ev in stream:
                    count += 1
                    kind = ev.get("type")

                    if kind == "TEXT_MESSAGE_CONTENT":
                        r.text += ev.get("delta", "")
                        await r.flush()
                    elif kind == "TEXT_MESSAGE_END":
                        await r.flush()
                    elif kind == "TOOL_CALL_START":
                        r.status = f"_running {ev.get('toolCallName', 'tool')}…_"
                        await r.flush(force=True)
                    elif kind == "TOOL_CALL_END":
                        r.status = ""
                        await r.flush()
                    elif kind == "RUN_FINISHED":
                        terminal = True
                        if (ev.get("outcome") or {}).get("type") == "interrupt":
                            error = INTERRUPT
                        break
                    elif kind == "RUN_ERROR":
                        terminal = True
                        message = ev.get("message", "")
                        # ponytail: workaround for an unclosed-step bug in
                        # bridge.rs's EventMapper — a successful run terminated in
                        # RUN_ERROR. Fixed upstream; kept so the proxy also works
                        # against an older arlo. Delete when that is not a concern.
                        if not message.startswith("RUN_FINISHED with active steps"):
                            error = f"*Run failed:* {message}"
                        break
                    # RUN_STARTED, TEXT_MESSAGE_START, TOOL_CALL_ARGS, STEP_* — ignored
    except TimeoutError:
        error = f"*Run timed out* after {timeout:.0f}s."
    except Exception as e:
        log.exception("run stream failed")
        error = f"*Could not reach Arlo:* {type(e).__name__}"

    if not terminal and error is None:
        error = INCOMPLETE

    r.status = ""
    if error:
        r.notice = error  # partial text, if any, is preserved above it
    elif not r.text.strip():
        r.notice = NO_OUTPUT
    await r.flush(force=True)
    return count
