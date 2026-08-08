"""AG-UI client: POST a RunAgentInput and iterate the SSE response."""

from __future__ import annotations

import json
import os
from collections.abc import AsyncIterator

import httpx
from httpx_sse import aconnect_sse

ARLO_URL = os.getenv("ARLO_URL", "http://arlo:8080/")


async def stream(
    messages: list[dict], thread_id: str, run_id: str, url: str = ARLO_URL
) -> AsyncIterator[dict]:
    """Yield AG-UI events as dicts. Keys are camelCase — the protocol wire format."""
    body = {
        "threadId": thread_id,
        "runId": run_id,
        "messages": messages,
        "state": None,
        "tools": [],  # Arlo ignores client-supplied tools and uses its own
        "context": [],
        "forwardedProps": {},
    }
    # No client-level timeout: the run budget is enforced by the caller's
    # asyncio.timeout around the whole stream (read timeouts would fire during
    # a long tool call).
    async with httpx.AsyncClient(timeout=httpx.Timeout(30.0, read=None)) as client:
        async with aconnect_sse(
            client,
            "POST",
            url,
            json=body,
            headers={"Accept": "text/event-stream"},
        ) as es:
            es.response.raise_for_status()
            async for sse in es.aiter_sse():
                if sse.data:
                    yield json.loads(sse.data)
