"""History_Builder: rebuild the AG-UI `messages` array from Slack on every turn.

Slack is the conversation store. Nothing is persisted here — every turn re-reads
the thread (and, for a fresh top-level mention, recent channel context).
"""

from __future__ import annotations

import logging
import os
import re

log = logging.getLogger(__name__)

THREAD_HISTORY_LIMIT = int(os.getenv("THREAD_HISTORY_LIMIT") or 100)
CHANNEL_HISTORY_LIMIT = int(os.getenv("CHANNEL_HISTORY_LIMIT") or 20)
HISTORY_CHAR_BUDGET = int(os.getenv("HISTORY_CHAR_BUDGET") or 12000)

SYSTEM_PROMPT = (
    "You are Arlo, answering in a Slack thread in channel {channel}. Your reply is "
    "rendered as a single Slack message: be brief, prefer short paragraphs over "
    "headings and tables, and put code in fenced blocks. Earlier messages from "
    "humans are prefixed with the speaker's name; do not prefix your own reply."
)

_MENTION = re.compile(r"<@[A-Z0-9]+>")

# Display names for the lifetime of the process. Slack user ids are stable and a
# rename mid-run does not matter.
_names: dict[str, str] = {}


async def display_name(client, user_id: str) -> str:
    if user_id not in _names:
        try:
            info = (await client.users_info(user=user_id))["user"]
            _names[user_id] = (
                info.get("profile", {}).get("display_name")
                or info.get("real_name")
                or user_id
            )
        except Exception:
            log.warning("users.info failed for %s, falling back to id", user_id)
            _names[user_id] = user_id
    return _names[user_id]


async def thread_replies(client, channel: str, thread_ts: str, limit: int) -> list[dict]:
    """Thread messages, oldest first. Empty list if Slack refuses — callers degrade."""
    try:
        resp = await client.conversations_replies(
            channel=channel, ts=thread_ts, limit=limit
        )
        return list(resp.get("messages") or [])
    except Exception:
        log.warning("conversations.replies failed for %s:%s", channel, thread_ts)
        return []


async def _channel_history(client, channel: str, limit: int) -> list[dict]:
    try:
        resp = await client.conversations_history(channel=channel, limit=limit)
        # conversations.history returns newest first.
        return list(reversed(resp.get("messages") or []))
    except Exception:
        log.warning("conversations.history failed for %s", channel)
        return []


def _is_bot(msg: dict, bot_user_id: str) -> bool:
    return bool(msg.get("bot_id")) or msg.get("user") == bot_user_id


def clean(text: str) -> str:
    """Strip mention tokens (including the bot's own) and surrounding whitespace."""
    return _MENTION.sub("", text or "").strip()


async def build(
    client,
    event: dict,
    bot_user_id: str,
    *,
    replies: list[dict] | None = None,
    thread_limit: int = THREAD_HISTORY_LIMIT,
    channel_limit: int = CHANNEL_HISTORY_LIMIT,
    budget: int = HISTORY_CHAR_BUDGET,
) -> list[dict]:
    """Build the AG-UI `messages` array for this turn."""
    channel = event["channel"]
    trigger_ts = event["ts"]
    thread_ts = event.get("thread_ts")

    if thread_ts:
        prior = replies if replies is not None else await thread_replies(
            client, channel, thread_ts, thread_limit
        )
    else:
        prior = await _channel_history(client, channel, channel_limit)

    history: list[dict] = []
    for msg in prior:
        if msg.get("ts") == trigger_ts or msg.get("subtype"):
            continue
        text = clean(msg.get("text", ""))
        if not text:
            continue
        if _is_bot(msg, bot_user_id):
            history.append({"role": "assistant", "content": text})
        else:
            name = await display_name(client, msg.get("user", "unknown"))
            history.append({"role": "user", "content": f"{name}: {text}"})

    trigger_name = await display_name(client, event.get("user", "unknown"))
    trigger = {
        "role": "user",
        "content": f"{trigger_name}: {clean(event.get('text', ''))}",
    }

    # Oldest-first trim. The trigger is pinned: it is never a candidate.
    total = sum(len(m["content"]) for m in history) + len(trigger["content"])
    while history and total > budget:
        total -= len(history.pop(0)["content"])

    system = {"role": "system", "content": SYSTEM_PROMPT.format(channel=channel)}
    return [
        {"id": f"m{i}", **m} for i, m in enumerate([system, *history, trigger])
    ]
