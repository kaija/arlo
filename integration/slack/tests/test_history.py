import pytest

from arlo_slack import history

BOT = "UBOT"


class FakeClient:
    """Canned Slack payloads. `fail` makes every history call raise."""

    def __init__(self, replies=None, channel=None, fail=False):
        self._replies = replies or []
        self._channel = channel or []
        self.fail = fail

    async def conversations_replies(self, **kw):
        if self.fail:
            raise RuntimeError("slack down")
        return {"messages": self._replies}

    async def conversations_history(self, **kw):
        if self.fail:
            raise RuntimeError("slack down")
        return {"messages": list(reversed(self._channel))}

    async def users_info(self, user):
        return {"user": {"profile": {"display_name": user.lower()}}}


@pytest.fixture(autouse=True)
def _clear_name_cache():
    history._names.clear()


def msg(user, text, ts, **extra):
    return {"user": user, "text": text, "ts": ts, **extra}


async def test_roles_and_name_prefix():
    client = FakeClient(
        replies=[
            msg("UALICE", "hello", "1.0"),
            msg(BOT, "hi there", "2.0", bot_id="B1"),
            msg("UBOB", "<@UBOT> what now?", "3.0"),
        ]
    )
    event = msg("UBOB", "<@UBOT> what now?", "3.0", channel="C1", thread_ts="1.0")

    out = await history.build(client, event, BOT)

    assert [m["role"] for m in out] == ["system", "user", "assistant", "user"]
    assert out[1]["content"] == "ualice: hello"
    assert out[2]["content"] == "hi there"
    # Trigger message: mention stripped, name prefixed, not duplicated from history
    assert out[3]["content"] == "ubob: what now?"
    assert all(m["id"] for m in out)


async def test_top_level_mention_reads_channel_history_oldest_first():
    client = FakeClient(
        channel=[msg("UALICE", "older", "1.0"), msg("UALICE", "newer", "2.0")]
    )
    event = msg("UBOB", "<@UBOT> hi", "3.0", channel="C1")

    out = await history.build(client, event, BOT)

    assert [m["content"] for m in out[1:]] == [
        "ualice: older",
        "ualice: newer",
        "ubob: hi",
    ]


async def test_subtypes_and_empty_messages_dropped():
    client = FakeClient(
        replies=[
            msg("UALICE", "joined", "1.0", subtype="channel_join"),
            msg("UALICE", "   ", "1.5"),
            msg("UALICE", "real", "2.0"),
        ]
    )
    event = msg("UBOB", "hi", "3.0", channel="C1", thread_ts="1.0")

    out = await history.build(client, event, BOT)

    assert [m["content"] for m in out[1:]] == ["ualice: real", "ubob: hi"]


async def test_trim_is_oldest_first_and_pins_the_trigger():
    client = FakeClient(
        replies=[msg("UALICE", "x" * 100, str(i)) for i in range(1, 10)]
    )
    event = msg("UBOB", "trigger", "99", channel="C1", thread_ts="1")

    out = await history.build(client, event, BOT, budget=250)

    body = out[1:]
    assert body[-1]["content"] == "ubob: trigger"
    assert sum(len(m["content"]) for m in body) <= 250
    assert len(body) < 10  # oldest dropped


async def test_api_failure_degrades_to_trigger_only():
    client = FakeClient(fail=True)
    event = msg("UBOB", "<@UBOT> hi", "3.0", channel="C1", thread_ts="1.0")

    out = await history.build(client, event, BOT)

    assert [m["role"] for m in out] == ["system", "user"]
    assert out[1]["content"] == "ubob: hi"


async def test_prefetched_replies_are_reused():
    client = FakeClient(fail=True)  # would raise if it fetched again
    event = msg("UBOB", "hi", "3.0", channel="C1", thread_ts="1.0")

    out = await history.build(
        client, event, BOT, replies=[msg("UALICE", "earlier", "1.0")]
    )

    assert out[1]["content"] == "ualice: earlier"
