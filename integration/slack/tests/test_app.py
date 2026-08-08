import asyncio

import pytest

from arlo_slack import app, history, render

BOT = "UBOT"


def ev(**kw):
    return {"channel": "C1", "ts": "1.0", "user": "UALICE", "text": "hi", **kw}


@pytest.fixture(autouse=True)
def _reset():
    app._seen.clear()
    app._locks.clear()
    app._waiting.clear()
    app.ALLOWED_CHANNELS = set()
    app.ALLOWED_USERS = set()
    app._bot_id = BOT


def test_mention_is_handled():
    assert app.should_handle(ev(), BOT, mention=True)


def test_bot_authored_events_are_ignored():
    assert not app.should_handle(ev(bot_id="B1"), BOT, mention=True)
    assert not app.should_handle(ev(user=BOT), BOT, mention=True)


def test_subtypes_are_ignored():
    assert not app.should_handle(ev(subtype="message_changed"), BOT, mention=False)


def test_dm_needs_no_thread_or_mention():
    assert app.should_handle(ev(channel_type="im"), BOT, mention=False)


def test_unthreaded_channel_message_is_ignored():
    assert not app.should_handle(ev(channel_type="channel"), BOT, mention=False)


def test_threaded_channel_message_is_accepted():
    assert app.should_handle(
        ev(channel_type="channel", thread_ts="0.5"), BOT, mention=False
    )


def test_channel_and_user_allowlists():
    app.ALLOWED_CHANNELS = {"C9"}
    assert not app.should_handle(ev(), BOT, mention=True)
    app.ALLOWED_CHANNELS = {"C1"}
    assert app.should_handle(ev(), BOT, mention=True)

    app.ALLOWED_USERS = {"UBOB"}
    assert not app.should_handle(ev(), BOT, mention=True)


async def test_mention_in_an_existing_thread_runs_exactly_once(monkeypatch):
    """Slack delivers an @-mention twice: app_mention and message.channels.

    Both deliveries must reach the same verdict — otherwise message.channels
    claims the dedup key, gets dropped for lack of bot participation in the
    thread, and the app_mention twin is discarded as a duplicate.
    """
    runs = []

    async def fake_turn(client, event, mention):
        runs.append(mention)

    monkeypatch.setattr(app, "_turn", fake_turn)
    threaded = ev(text=f"<@{BOT}> hi", thread_ts="0.5", channel_type="channel")

    await app._dispatch(None, dict(threaded), mention=False)
    await app._dispatch(None, dict(threaded, type="app_mention"), mention=True)
    await asyncio.sleep(0)

    assert runs == [True]


def test_duplicate_events_are_processed_once():
    assert app._first_time(("C1", "1.0"))
    assert not app._first_time(("C1", "1.0"))


def test_thread_key_falls_back_to_own_ts():
    assert app.thread_key(ev()) == "C1:1.0"
    assert app.thread_key(ev(thread_ts="0.5")) == "C1:0.5"


class FakeClient:
    """Slack + Arlo stand-in: records reactions and replies with one text event."""

    def __init__(self, replies=None):
        self.replies = replies or []
        self.reactions: list[str] = []
        self.posts: list[str] = []

    async def conversations_replies(self, **kw):
        return {"messages": self.replies}

    async def conversations_history(self, **kw):
        return {"messages": []}

    async def users_info(self, user):
        return {"user": {"profile": {"display_name": user.lower()}}}

    async def reactions_add(self, **kw):
        self.reactions.append("add")

    async def reactions_remove(self, **kw):
        self.reactions.append("remove")

    async def chat_postMessage(self, channel, thread_ts, text):
        self.posts.append(text)
        return {"ts": "p1"}

    async def chat_update(self, channel, ts, text):
        self.posts.append(text)
        return {"ok": True}


@pytest.fixture
def stub_run(monkeypatch):
    """Replace the AG-UI call with a canned stream, keeping the real renderer."""
    started = asyncio.Event()
    release = asyncio.Event()

    async def fake_stream(messages, thread_id, run_id, url=""):
        started.set()
        await release.wait()
        yield {"type": "TEXT_MESSAGE_CONTENT", "delta": "done"}
        yield {"type": "RUN_FINISHED", "outcome": {"type": "success"}}

    monkeypatch.setattr(app.arlo, "stream", fake_stream)
    monkeypatch.setattr(render, "UPDATE_INTERVAL_SECONDS", 0)
    monkeypatch.setattr(app, "UPDATE_INTERVAL_SECONDS", 0)
    history._names.clear()
    return started, release


async def test_thread_followup_without_bot_participation_is_dropped(stub_run):
    client = FakeClient(replies=[{"user": "UALICE", "text": "hi", "ts": "0.5"}])
    await app._turn(
        client, ev(channel_type="channel", thread_ts="0.5"), mention=False
    )
    assert client.posts == []  # never even posted a placeholder


async def test_thread_followup_with_bot_participation_runs(stub_run):
    started, release = stub_run
    release.set()
    client = FakeClient(
        replies=[{"user": BOT, "bot_id": "B1", "text": "earlier", "ts": "0.5"}]
    )
    await app._turn(
        client, ev(channel_type="channel", thread_ts="0.5"), mention=False
    )
    assert client.posts[-1] == "done"


async def test_second_message_in_a_busy_thread_is_queued_with_an_hourglass(stub_run):
    started, release = stub_run
    client = FakeClient()

    first = asyncio.create_task(app._turn(client, ev(), mention=True))
    await started.wait()  # first run holds the per-thread lock

    # Same Thread_Key: a reply inside the thread the first run is answering.
    second_event = ev(ts="2.0", thread_ts="1.0")
    second = asyncio.create_task(app._turn(client, second_event, mention=True))
    for _ in range(20):  # let it reach the contended lock
        await asyncio.sleep(0)
        if client.reactions:
            break
    assert client.reactions == ["add"]

    release.set()
    await asyncio.gather(first, second)
    assert client.reactions == ["add", "remove"]
    assert app._locks == {}  # registry drained


async def test_unhandled_error_reports_in_thread(monkeypatch):
    async def boom(*a, **kw):
        raise RuntimeError("kaboom")

    monkeypatch.setattr(app.history, "build", boom)
    client = FakeClient()
    await app._turn(client, ev(), mention=True)
    assert "Something went wrong" in client.posts[-1]
