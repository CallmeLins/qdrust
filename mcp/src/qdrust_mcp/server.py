"""qdrust MCP server.

Every tool is a thin wrapper over :class:`~qdrust_mcp.client.QdrustClient`; the
REST mapping (path, query, body, and "omit means unchanged") lives in the
client so it can be tested without a running server.

Tools return a JSON **string** rather than a bare dict/list: FastMCP converts a
container into one content block per element, and an empty list would become no
content at all — an empty result has to read as `[]`, not as silence.

Run it over stdio with ``qdrust-mcp`` (installed entry point) or
``python -m qdrust_mcp.server``. Configuration is environment-only:

- ``QDRUST_URL``   base URL of the qdrust server (default ``http://localhost:8923``)
- ``QDRUST_TOKEN`` a personal access token (``qd_...``) from Settings → API tokens
"""

from __future__ import annotations

import json
from typing import Any

from mcp.server.fastmcp import FastMCP

from . import __version__
from .client import QdrustClient, QdrustError

SERVER_NAME = "qdrust-mcp"
INSTRUCTIONS = (
    "Create, run and inspect qdrust HTTP automation tasks. A task executes a "
    "template: use list_templates to find one, then create_task with template_id "
    "and variables. update_task changes only the fields you pass."
)


def _json(value: Any) -> str:
    """One text block for every result, including `[]` and `null`."""
    return json.dumps(value, ensure_ascii=False, indent=2)


def build_server(client: QdrustClient | None = None) -> FastMCP:
    api = client or QdrustClient()
    try:
        mcp = FastMCP(SERVER_NAME, instructions=INSTRUCTIONS, version=__version__)
    except TypeError:
        # `version` was added to FastMCP after 1.10; the name alone is enough.
        mcp = FastMCP(SERVER_NAME, instructions=INSTRUCTIONS)

    # ------------------------------------------------------------------ tasks

    @mcp.tool()
    def list_tasks(grp: str | None = None) -> str:
        """List the caller's tasks, newest first. Filter by group with `grp`."""
        return _json(api.list_tasks(grp))

    @mcp.tool()
    def get_task(task_id: int) -> str:
        """Fetch one task by id."""
        return _json(api.get_task(task_id))

    @mcp.tool()
    def create_task(
        name: str,
        cron: str,
        template_id: int | None = None,
        grp: str | None = None,
        timezone: str | None = None,
        disabled: bool | None = None,
        url: str | None = None,
        timeout_seconds: int | None = None,
        variables: dict[str, Any] | None = None,
    ) -> str:
        """Create a scheduled task.

        A task's requests come from its template, so bind `template_id`; `url` is
        only for a task that is not bound to a template. `cron` is the 7-field
        form `sec min hour day month weekday year`, e.g. `0 0 8 * * * *` is 08:00
        daily. `variables` seeds the template, e.g. {"username": "..."}.
        """
        return _json(
            api.create_task(
                name=name,
                cron=cron,
                template_id=template_id,
                grp=grp,
                timezone=timezone,
                disabled=disabled,
                url=url,
                timeout_seconds=timeout_seconds,
                variables=variables,
            )
        )

    @mcp.tool()
    def update_task(
        task_id: int,
        name: str | None = None,
        cron: str | None = None,
        disabled: bool | None = None,
        grp: str | None = None,
        timezone: str | None = None,
        variables: dict[str, Any] | None = None,
    ) -> str:
        """Update a task. Fields left out keep their stored value."""
        return _json(
            api.update_task(
                task_id,
                name=name,
                cron=cron,
                disabled=disabled,
                grp=grp,
                timezone=timezone,
                variables=variables,
            )
        )

    @mcp.tool()
    def delete_task(task_id: int) -> str:
        """Delete a task and its runs."""
        return _json(api.delete_task(task_id))

    @mcp.tool()
    def run_task(task_id: int) -> str:
        """Run a task immediately and return its new run record."""
        return _json(api.run_task(task_id))

    @mcp.tool()
    def batch_tasks(ids: list[int], action: str) -> str:
        """Enable, pause, delete or run several tasks at once.

        `action` is one of `enable`, `disable`, `delete`, `run`.
        """
        return _json(api.batch_tasks(ids=ids, action=action))

    @mcp.tool()
    def list_task_groups() -> str:
        """List the caller's task groups."""
        return _json(api.list_task_groups())

    # ------------------------------------------------------------------- runs

    @mcp.tool()
    def list_runs(
        status: str | None = None,
        task_id: int | None = None,
        limit: int | None = None,
    ) -> str:
        """List runs across the caller's tasks, newest first.

        `status` filters to one of pending, leased, running, succeeded, failed,
        cancelled; `limit` is 1..=500 (default 100).
        """
        return _json(api.list_runs(status=status, task_id=task_id, limit=limit))

    @mcp.tool()
    def list_task_runs(task_id: int) -> str:
        """List one task's runs."""
        return _json(api.list_task_runs(task_id))

    @mcp.tool()
    def get_run_steps(run_id: int) -> str:
        """List the steps of one run: per-request status and body size."""
        return _json(api.get_run_steps(run_id))

    @mcp.tool()
    def cancel_run(run_id: int) -> str:
        """Cancel an active run."""
        return _json(api.cancel_run(run_id))

    # -------------------------------------------------------------- templates

    @mcp.tool()
    def list_templates(q: str | None = None, limit: int | None = None) -> str:
        """List the caller's templates, newest first. `q` matches the name."""
        return _json(api.list_templates(q=q, limit=limit))

    @mcp.tool()
    def get_template(template_id: int) -> str:
        """Fetch one template by id, including its input variables and defaults."""
        return _json(api.get_template(template_id))

    @mcp.tool()
    def test_template(template_id: int, variables: dict[str, Any] | None = None) -> str:
        """Run a saved template with these variables and return its steps.

        Nothing is persisted: no task and no run record. Use it to check a
        template and its variables before creating a task.
        """
        return _json(api.test_template(template_id, variables=variables))

    @mcp.tool()
    def import_template(name: str, har: dict[str, Any], description: str | None = None) -> str:
        """Import a QD HAR document as a template.

        `har` is the QD document (`{"log": {"version": "1.2", "entries": [...]}}`)
        or a QD request array.
        """
        return _json(api.import_template(name=name, har=har, description=description))

    # ---------------------------------------------------------- notifications

    @mcp.tool()
    def list_notification_channels() -> str:
        """List notification channels (Webhook, Email, Telegram, …)."""
        return _json(api.list_notification_channels())

    @mcp.tool()
    def bind_notification(
        task_ids: list[int],
        channel_id: int,
        event: str | None = None,
    ) -> str:
        """Bind a notification channel to one or more tasks.

        `event` is `success`, `failure` (default) or `always`.
        """
        return _json(api.bind_notification(task_ids=task_ids, channel_id=channel_id, event=event))

    return mcp


def main() -> None:
    # httpx logs every request at INFO. On a stdio server that noise lands in the
    # client's log for no reason, so keep only warnings and above.
    import logging
    import sys

    logging.getLogger("httpx").setLevel(logging.WARNING)
    try:
        server = build_server()
    except QdrustError as error:
        # A misconfigured client should read one clear line, not a traceback.
        print(f"qdrust-mcp: {error}", file=sys.stderr)
        raise SystemExit(2) from None
    server.run()


if __name__ == "__main__":
    main()
