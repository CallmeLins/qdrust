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


async def _tool_names() -> set[str]:
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "qdrust_mcp.server"],
        env={**os.environ, "QDRUST_TOKEN": "qd_test", "QDRUST_URL": "http://127.0.0.1:9"},
    )
    async with stdio_client(params) as (read, write), ClientSession(read, write) as session:
        await session.initialize()
        listed = await session.list_tools()
        return {tool.name for tool in listed.tools}


def test_registers_the_documented_tools() -> None:
    assert asyncio.run(_tool_names()) == EXPECTED_TOOLS
