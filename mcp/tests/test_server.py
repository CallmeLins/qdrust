"""The MCP surface itself: spawn the server over stdio and list its tools.

Tool names are a client-facing contract — renaming one silently breaks every
saved client config that names it — so the set is pinned here.
"""

from __future__ import annotations

import asyncio
import os
import sys

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from qdrust_mcp import __version__

EXPECTED_TOOLS = {
    "batch_tasks",
    "bind_notification",
    "cancel_run",
    "create_task",
    "delete_task",
    "get_run_steps",
    "get_task",
    "get_template",
    "import_template",
    "list_notification_channels",
    "list_runs",
    "list_task_groups",
    "list_task_runs",
    "list_tasks",
    "list_templates",
    "run_task",
    "test_template",
    "update_task",
}


async def _probe() -> tuple[set[str], str]:
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "qdrust_mcp.server"],
        env={**os.environ, "QDRUST_TOKEN": "qd_test", "QDRUST_URL": "http://127.0.0.1:9"},
    )
    async with stdio_client(params) as (read, write), ClientSession(read, write) as session:
        initialized = await session.initialize()
        listed = await session.list_tools()
        return {tool.name for tool in listed.tools}, initialized.serverInfo.version


def test_registers_the_documented_tools() -> None:
    assert asyncio.run(_probe())[0] == EXPECTED_TOOLS


def test_reports_its_own_version_not_the_sdks() -> None:
    # FastMCP has no `version` argument, so a plain FastMCP(...) advertises the
    # mcp package's version (1.30.0) instead of this server's. Pin it: if a
    # future SDK moves the attribute this test is what says so.
    assert asyncio.run(_probe())[1] == __version__
