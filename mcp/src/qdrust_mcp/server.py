"""qdrust MCP server.

Every tool is a thin wrapper over :class:`~qdrust_mcp.client.QdrustClient`; the
REST mapping (path, query, body, and "omit means unchanged") lives in the
client so it can be tested without a running server.

Tools return a JSON **string** rather than a bare dict/list: FastMCP converts a
container into one content block per element, and an empty list would become no
content at all — an empty result has to read as `[]`, not as silence.

Run it over stdio with ``qdrust-mcp`` (installed entry point) or
``python -m qdrust_mcp.server``. Configuration is environment-only:

- ``QDRUST_URL``     base URL of the qdrust server (default ``http://localhost:8923``)
- ``QDRUST_TOKEN``   a personal access token (``qd_...``) from Settings → API tokens
- ``QDRUST_TIMEOUT`` per-request timeout in seconds (default 120)
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
    "and variables. update_task changes only the fields you pass, and clear=[...] "
    "erases one. Notepad slots hold small values that templates read and write."
)


def _json(value: Any) -> str:
    """One text block for every result, including `[]` and `null`."""
    return json.dumps(value, ensure_ascii=False, indent=2)


def build_server(client: QdrustClient | None = None) -> FastMCP:
    api = client or QdrustClient()
    mcp = FastMCP(SERVER_NAME, instructions=INSTRUCTIONS)
    # FastMCP takes no `version`: passing one raises TypeError, and the server
    # then advertises the *SDK's* own version (1.30.0) to every client. The
    # low-level Server underneath does carry it and uses it in the initialize
    # response, so set it there. If a future SDK renames it this silently stops
    # working — which is why tests/test_server.py asserts the reported version.
    underlying = getattr(mcp, "_mcp_server", None)
    if underlying is not None:
        underlying.version = __version__

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
        method: str | None = None,
        headers: dict[str, Any] | None = None,
        body: str | None = None,
        timeout_seconds: int | None = None,
        retry_count: int | None = None,
        retry_interval_seconds: int | None = None,
        priority: int | None = None,
        random_delay_max_seconds: int | None = None,
        variables: dict[str, Any] | None = None,
    ) -> str:
        """Create a scheduled task.

        A task's requests come from its template, so bind `template_id`; `url`
        (with `method`, `headers`, `body`) is only for a task that is not bound
        to a template. `cron` is the 7-field form
        `sec min hour day month weekday year`, e.g. `0 0 8 * * * *` is 08:00
        daily. `timezone` is an IANA name (default UTC). `variables` seeds the
        template, e.g. {"username": "..."}. `retry_count` is 0 = never,
        -1 = always, N = up to N retries; `random_delay_max_seconds` jitters a
        due run by 0..=N seconds.
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
                method=method,
                headers=headers,
                body=body,
                timeout_seconds=timeout_seconds,
                retry_count=retry_count,
                retry_interval_seconds=retry_interval_seconds,
                priority=priority,
                random_delay_max_seconds=random_delay_max_seconds,
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
        method: str | None = None,
        url: str | None = None,
        headers: dict[str, Any] | None = None,
        body: str | None = None,
        template_id: int | None = None,
        timeout_seconds: int | None = None,
        retry_count: int | None = None,
        retry_interval_seconds: int | None = None,
        priority: int | None = None,
        random_delay_max_seconds: int | None = None,
        clear: list[str] | None = None,
    ) -> str:
        """Update a task. Fields left out keep their stored value.

        To erase one instead — ungroup a task, drop its timezone, forget its
        variables — name it in `clear`, e.g. clear=["grp"]. A field may not be
        both set and cleared.
        """
        return _json(
            api.update_task(
                task_id,
                name=name,
                cron=cron,
                disabled=disabled,
                grp=grp,
                timezone=timezone,
                variables=variables,
                method=method,
                url=url,
                headers=headers,
                body=body,
                template_id=template_id,
                timeout_seconds=timeout_seconds,
                retry_count=retry_count,
                retry_interval_seconds=retry_interval_seconds,
                priority=priority,
                random_delay_max_seconds=random_delay_max_seconds,
                clear=clear,
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
        before_id: int | None = None,
    ) -> str:
        """List runs across the caller's tasks, newest first.

        `status` filters to one of pending, leased, running, succeeded, failed,
        cancelled; `limit` is 1..=500 (default 100). `before_id` pages back:
        pass a run id and only older runs come back.
        """
        return _json(
            api.list_runs(status=status, task_id=task_id, limit=limit, before_id=before_id)
        )

    @mcp.tool()
    def list_task_runs(task_id: int) -> str:
        """List one task's runs."""
        return _json(api.list_task_runs(task_id))

    @mcp.tool()
    def get_run_steps(run_id: int) -> str:
        """List the steps of one run: per-request name, status and body size."""
        return _json(api.get_run_steps(run_id))

    @mcp.tool()
    def cancel_run(run_id: int) -> str:
        """Cancel an active run."""
        return _json(api.cancel_run(run_id))

    @mcp.tool()
    def delete_run(run_id: int) -> str:
        """Delete one run and its steps."""
        return _json(api.delete_run(run_id))

    # -------------------------------------------------------------- templates

    @mcp.tool()
    def list_templates(
        q: str | None = None,
        grp: str | None = None,
        cursor: int | None = None,
        limit: int | None = None,
    ) -> str:
        """List the caller's templates, newest first.

        `q` matches the name, `grp` the group. `cursor` pages back: pass the id
        of the last template you saw.
        """
        return _json(api.list_templates(q=q, grp=grp, cursor=cursor, limit=limit))

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

    # --------------------------------------------------------------- notepads

    @mcp.tool()
    def list_notepads() -> str:
        """List the caller's notepad slots: id, size, a short preview, updated_at.

        These slots are the small store that `toolbox/notepad` template steps
        read and write (cookies, tokens, cursors). Read one with get_notepad.
        """
        return _json(api.list_notepads())

    @mcp.tool()
    def get_notepad(notepad_id: int) -> str:
        """Read one notepad slot's whole value."""
        return _json(api.get_notepad(notepad_id))

    @mcp.tool()
    def set_notepad(notepad_id: int, content: str) -> str:
        """Create or overwrite one notepad slot.

        Slots are numbered from 1 and there are at most 20 of them, so a
        template asks for a specific slot to read the value a previous run left.
        `content` is capped at 256 KiB.
        """
        return _json(api.set_notepad(notepad_id, content))

    @mcp.tool()
    def delete_notepad(notepad_id: int) -> str:
        """Remove one notepad slot. Answers not-found when the slot is absent."""
        return _json(api.delete_notepad(notepad_id))

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

    @mcp.tool()
    def list_notification_actions() -> str:
        """List every notification binding (task ↔ channel) the account has.

        Each row carries `id`, `task_id`, `channel_id`, `event`,
        `failure_threshold`, `automatic_only` and the two templates. The `id` is
        what `preview_notification` takes. `bind_notification` is the way to add
        one of these; this tool is how you find one.
        """
        return _json(api.list_notification_actions())

    @mcp.tool()
    def preview_notification(action_id: int, run_id: int | None = None) -> str:
        """Render one binding's message without sending it anywhere.

        The text comes from the binding's own templates, else the account's
        default for that event, else the built-in pair — against the task's
        newest run, or the one named by `run_id`. `source` is `sample` when the
        task has never run: the message still renders, but `{status}` `{error}`
        `{log}` are empty, so an empty log is not a broken template. This is the
        only way to read a template's output before a scheduled run does.
        """
        return _json(api.preview_notification_action(action_id, run_id))

    @mcp.tool()
    def get_notification_defaults() -> str:
        """The account's default notification templates, per event.

        `defaults` holds one entry per event the account has written (a cleared
        one comes back with nulls); `builtin` is the pair used when neither the
        binding nor a default sets a template.
        """
        return _json(api.notification_defaults())

    @mcp.tool()
    def set_notification_default(
        event: str,
        title_template: str = "",
        body_template: str = "",
    ) -> str:
        """Set the account's default title/body template for one event.

        `event` is `success` or `failure` — a default is the wording for an
        outcome, so the binding-only `always` is not accepted. Any binding
        without a template of its own uses these; a blank string clears it, so
        the built-in pair is used again. Variables: `{event}` `{status_cn}`
        `{task_id}` `{task}` `{run_id}` `{status}` `{error}` `{log}` `{t}`.
        """
        return _json(
            api.set_notification_default(
                event,
                title_template=title_template,
                body_template=body_template,
            )
        )

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
