"""Slack Socket Mode proxy: Slack events in, AG-UI runs out.

Holds no state that outlives a run. Slack is the conversation store; the only
in-process maps are the duplicate-event window and the per-thread run locks.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import os
import signal
import sys
import time
import uuid
from collections import Counter, OrderedDict

from slack_bolt.adapter.socket_mode.aiohttp import AsyncSocketModeHandler
from slack_bolt.app.async_app import AsyncApp

from . import arlo, history, render

log = logging.getLogger("arlo_slack")

MAX_CONCURRENT_RUNS = int(os.getenv("MAX_CONCURRENT_RUNS") or 4)
SHUTDOWN_GRACE_SECONDS = float(os.getenv("SHUTDOWN_GRACE_SECONDS") or 30)
UPDATE_INTERVAL_SECONDS = float(os.getenv("UPDATE_INTERVAL_SECONDS") or 1.0)
MAX_MESSAGE_CHARS = int(os.getenv("MAX_MESSAGE_CHARS") or 3800)
RUN_TIMEOUT_SECONDS = float(os.getenv("RUN_TIMEOUT_SECONDS") or 300)

QUEUED_REACTION = "hourglass_flowing_sand"


def _id_set(name: str) -> set[str]:
    return {s.strip() for s in (os.getenv(name) or "").split(",") if s.strip()}


ALLOWED_CHANNELS = _id_set("SLACK_ALLOWED_CHANNELS")
ALLOWED_USERS = _id_set("SLACK_ALLOWED_USERS")

_bot_id = ""
_seen: OrderedDict[tuple[str, str], None] = OrderedDict()
_locks: dict[str, asyncio.Lock] = {}
_waiting: Counter[str] = Counter()
_sem = asyncio.Semaphore(MAX_CONCURRENT_RUNS)
_tasks: set[asyncio.Task] = set()
_accepting = True


# --- Access_Filter ---------------------------------------------------------


def mentions_bot(event: dict, bot_id: str) -> bool:
    return f"<@{bot_id}>" in (event.get("text") or "")


def why_ignore(event: dict, bot_id: str, mention: bool) -> str | None:
    """Reason this event is not ours, or None if it should run.

    Pure, and it returns the reason rather than a bool so every drop can say
    why — a silently discarded event is the hardest thing to debug here.
    """
    if event.get("bot_id"):
        return "bot-authored"
    if not event.get("user"):
        return "no user id"
    if event.get("user") == bot_id:
        return "self-authored"
    if event.get("subtype"):  # message_changed, channel_join, …
        return f"subtype {event['subtype']}"
    if not (event.get("text") or "").strip():
        return "empty text"
    # A non-mention channel message is only ours if it is threaded (2.4). DMs
    # need no thread and no mention (2.2).
    if not mention and event.get("channel_type") != "im" and not event.get("thread_ts"):
        return "channel message, not threaded, no mention"
    if ALLOWED_CHANNELS and event.get("channel") not in ALLOWED_CHANNELS:
        return "channel not in SLACK_ALLOWED_CHANNELS"
    if ALLOWED_USERS and event.get("user") not in ALLOWED_USERS:
        return "user not in SLACK_ALLOWED_USERS"
    return None


def should_handle(event: dict, bot_id: str, mention: bool) -> bool:
    return why_ignore(event, bot_id, mention) is None


def _first_time(key: tuple[str, str]) -> bool:
    """Slack redelivers; process each (channel, event_ts) at most once."""
    if key in _seen:
        return False
    _seen[key] = None
    while len(_seen) > 1000:
        _seen.popitem(last=False)
    return True


def thread_key(event: dict) -> str:
    return f"{event['channel']}:{event.get('thread_ts') or event['ts']}"


# --- run lifecycle ---------------------------------------------------------


async def _react(client, channel: str, ts: str, add: bool) -> None:
    call = client.reactions_add if add else client.reactions_remove
    with contextlib.suppress(Exception):  # already_reacted / no_reaction
        await call(channel=channel, timestamp=ts, name=QUEUED_REACTION)


async def _turn(client, event: dict, mention: bool) -> None:
    channel, ts = event["channel"], event["ts"]
    thread_ts = event.get("thread_ts") or ts
    key = thread_key(event)
    run_id = str(uuid.uuid4())
    replies = None

    try:
        # Thread follow-up in a channel: ours only if the bot has posted here.
        # The same fetch feeds the History_Builder, so the check costs nothing.
        if not mention and event.get("thread_ts") and event.get("channel_type") != "im":
            replies = await history.thread_replies(
                client, channel, thread_ts, history.THREAD_HISTORY_LIMIT
            )
            if not any(history._is_bot(m, _bot_id) for m in replies):
                log.info("filtered: bot has not posted in thread %s", key)
                return

        _waiting[key] += 1
        lock = _locks.setdefault(key, asyncio.Lock())
        queued = lock.locked() or _sem.locked()
        if queued:
            await _react(client, channel, ts, add=True)

        async with lock, _sem:
            if queued:
                await _react(client, channel, ts, add=False)
            started = time.monotonic()
            log.info("run started thread=%s run=%s url=%s", key, run_id, arlo.ARLO_URL)
            messages = await history.build(client, event, _bot_id, replies=replies)
            count = await render.render(
                client,
                channel,
                thread_ts,
                arlo.stream(messages, key, run_id),
                interval=UPDATE_INTERVAL_SECONDS,
                max_chars=MAX_MESSAGE_CHARS,
                timeout=RUN_TIMEOUT_SECONDS,
            )
            log.info(
                "run finished thread=%s run=%s messages=%d events=%d duration=%.1fs",
                key,
                run_id,
                len(messages),
                count,
                time.monotonic() - started,
            )
    except Exception:
        log.exception("unhandled error thread=%s run=%s", key, run_id)
        with contextlib.suppress(Exception):
            await client.chat_postMessage(
                channel=channel,
                thread_ts=thread_ts,
                text="_Something went wrong handling that message._",
            )
    finally:
        _waiting[key] -= 1
        if _waiting[key] <= 0:
            del _waiting[key]
            _locks.pop(key, None)


async def _dispatch(client, event: dict, mention: bool) -> None:
    """Runs inside the 3-second ack budget: filter, then hand off to a task."""
    # An @-mention arrives twice: as app_mention and as message.channels. Both
    # deliveries must reach the same verdict, or whichever lands first claims
    # the dedup key and the other is discarded as a duplicate.
    mention = mention or mentions_bot(event, _bot_id)

    log.debug(
        "event type=%s channel=%s ts=%s thread_ts=%s channel_type=%s mention=%s",
        event.get("type"),
        event.get("channel"),
        event.get("ts"),
        event.get("thread_ts"),
        event.get("channel_type"),
        mention,
    )

    if not _accepting:
        return
    reason = why_ignore(event, _bot_id, mention)
    if reason:
        # Access-control drops are an operator concern (10.5); the rest is noise
        # in any busy channel, so it only shows at DEBUG.
        level = logging.INFO if "ALLOWED" in reason else logging.DEBUG
        log.log(level, "ignored (%s) channel=%s ts=%s", reason, event.get("channel"), event.get("ts"))
        return
    if not _first_time((event.get("channel", ""), event.get("ts", ""))):
        log.debug("ignored (duplicate delivery) ts=%s", event.get("ts"))
        return
    task = asyncio.create_task(_turn(client, event, mention))
    _tasks.add(task)
    task.add_done_callback(_tasks.discard)


def build_app(token: str) -> AsyncApp:
    app = AsyncApp(token=token, logger=log)

    @app.event("app_mention")
    async def _on_mention(event):
        await _dispatch(app.client, event, mention=True)

    @app.event("message")
    async def _on_message(event):
        await _dispatch(app.client, event, mention=False)

    return app


# --- entrypoint ------------------------------------------------------------


def _require(name: str, prefix: str) -> str:
    value = os.getenv(name, "")
    if not value.startswith(prefix):
        log.error("%s is missing or malformed (expected a %s… token)", name, prefix)
        sys.exit(1)
    return value


async def _serve() -> None:
    global _bot_id, _accepting

    bot_token = _require("SLACK_BOT_TOKEN", "xoxb-")
    app_token = _require("SLACK_APP_TOKEN", "xapp-")

    app = build_app(bot_token)
    handler = AsyncSocketModeHandler(app, app_token)

    try:
        _bot_id = (await app.client.auth_test())["user_id"]
    except Exception as e:
        log.error("slack auth_test failed: %s", type(e).__name__)
        sys.exit(1)

    async def _on_close(_message) -> None:
        log.warning("socket mode disconnected, reconnecting")

    handler.client.on_close_listeners.append(_on_close)

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stop.set)

    await handler.connect_async()
    log.info("connected to slack as %s, arlo at %s", _bot_id, arlo.ARLO_URL)
    await stop.wait()

    _accepting = False
    log.info("shutdown signal: draining %d run(s)", len(_tasks))
    await handler.close_async()
    if _tasks:
        await asyncio.wait(set(_tasks), timeout=SHUTDOWN_GRACE_SECONDS)


def main() -> None:
    logging.basicConfig(
        level=os.getenv("LOG_LEVEL", "INFO").upper(),
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )
    asyncio.run(_serve())


if __name__ == "__main__":
    main()
