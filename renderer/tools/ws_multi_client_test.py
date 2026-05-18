"""Two-client subscribe test for the orchestrator /ws endpoint.

Validates the Phase 1 lift of the single-client gate: with the old code,
the second client would have been rejected with HTTP 409 CONFLICT. With
the gate lifted, both should receive the initial `Idle` event.

Usage:
    python ws_multi_client_test.py [URL]
    # default URL: ws://localhost:8080/ws
"""

from __future__ import annotations

import asyncio
import json
import sys

import websockets


async def subscriber(idx: int, url: str, hold: asyncio.Event, results: list) -> None:
    try:
        async with websockets.connect(url) as ws:
            msg = await asyncio.wait_for(ws.recv(), timeout=3.0)
            try:
                parsed = json.loads(msg)
            except json.JSONDecodeError:
                parsed = {"raw": msg[:80]}
            results.append((idx, "OK", parsed))
            # Keep the socket open until the test releases us, so client 2
            # connects WHILE client 1 is still connected. The old gate
            # rejected the second concurrent client; this is what we test.
            await hold.wait()
    except websockets.exceptions.InvalidStatus as e:
        results.append((idx, "REJECTED", str(e.response.status_code)))
    except asyncio.TimeoutError:
        results.append((idx, "TIMEOUT", "no initial message in 3s"))
    except Exception as e:
        results.append((idx, "ERR", repr(e)))


async def main(url: str) -> int:
    results: list = []
    hold = asyncio.Event()

    tasks = [asyncio.create_task(subscriber(i, url, hold, results)) for i in (1, 2)]
    # Give both a beat to handshake + receive the initial Idle event.
    await asyncio.sleep(1.0)
    hold.set()
    await asyncio.gather(*tasks)

    for idx, status, payload in sorted(results):
        print(f"  client {idx}: {status:8s}  {payload}")

    ok = sum(1 for _, s, _ in results if s == "OK")
    if ok == 2:
        print("PASS: both clients subscribed and received the initial event")
        return 0
    print(f"FAIL: expected 2 OK, got {ok}")
    return 1


if __name__ == "__main__":
    url = sys.argv[1] if len(sys.argv) > 1 else "ws://localhost:8080/ws"
    sys.exit(asyncio.run(main(url)))
