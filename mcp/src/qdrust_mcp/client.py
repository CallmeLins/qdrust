"""A thin, authenticated REST client for one qdrust instance.

Everything the MCP tools do is one call on this class, so the URL / query /
body shape lives here rather than in the tool wrappers, and the tests can drive
it against an ``httpx.MockTransport`` without a live server.
"""

from __future__ import annotations

import os
from typing import Any

import httpx

DEFAULT_URL = "http://localhost:8923"
DEFAULT_TIMEOUT = 120.0

#: Fields `update_task(..., clear=[...])` may erase. Each one is an
#: ``Option<Option<T>>`` on the server, where an explicit ``null`` clears the
#: stored value — something a tool call cannot express by passing ``None``,
#: because ``None`` is how "leave this field alone" travels.
CLEARABLE_FIELDS = frozenset(
    {
        "grp",
        "timezone",
        "variables",
        "timeout_seconds",
        "retry_count",
        "retry_interval_seconds",
        "priority",
        "random_delay_max_seconds",
    }
)


class QdrustError(RuntimeError):
    """A refused or failed request, or an argument the API cannot express."""


def present(**fields: Any) -> dict[str, Any]:
    """Keep only the fields the caller actually set.

    The REST API treats an omitted field as "keep the stored value", so a
    ``null`` would be a different (and destructive) operation. A legitimate
    empty string is kept.
    """
    return {name: value for name, value in fields.items() if value is not None}


def _query(params: dict[str, Any] | None) -> dict[str, Any] | None:
    """Only the parameters that carry a value.

    A blank string is dropped, ``None`` is dropped. ``?grp=`` would ask the
    server for the tasks whose group is the empty string, and an ungrouped task
    holds NULL — so "I did not mean to filter" would silently return nothing.
    """
    kept = {
        name: value
        for name, value in (params or {}).items()
        if value is not None and not (isinstance(value, str) and not value.strip())
    }
    return kept or None


def _resolve_timeout() -> float:
    raw = os.environ.get("QDRUST_TIMEOUT")
    if not raw or not raw.strip():
        return DEFAULT_TIMEOUT
    try:
        timeout = float(raw)
    except ValueError:
        raise QdrustError(f"QDRUST_TIMEOUT must be a number of seconds, got {raw!r}") from None
    if timeout <= 0:
        raise QdrustError(f"QDRUST_TIMEOUT must be positive, got {raw!r}")
    return timeout


def _preview(response: httpx.Response, limit: int = 300) -> str:
    """The head of a body we could not parse, for an error message."""
    text = response.content[:limit].decode("utf-8", errors="replace")
    return text + ("…" if len(response.content) > limit else "")


def _summarise(response: httpx.Response) -> str:
    """qdrust's error body is JSON ``{code, message}``; fall back to raw text."""
    try:
        payload = response.json()
    except ValueError:
        text = response.text
        return text[:600] + ("…" if len(text) > 600 else "")
    if isinstance(payload, dict):
        message = payload.get("message") or payload.get("code")
        if message:
            return str(message)[:600]
    return str(payload)[:600]


class QdrustClient:
    def __init__(
        self,
        base_url: str | None = None,
        token: str | None = None,
        *,
        timeout: float | None = None,
        http: httpx.Client | None = None,
    ) -> None:
        self.base_url = (base_url or os.environ.get("QDRUST_URL") or DEFAULT_URL).rstrip("/")
        token = token or os.environ.get("QDRUST_TOKEN")
        if not token:
            raise QdrustError(
                "QDRUST_TOKEN is required: create a personal access token in qdrust "
                "under Settings -> API tokens"
            )
        self.token = token
        self._http = http or httpx.Client(timeout=timeout or _resolve_timeout())

    def close(self) -> None:
        self._http.close()

    # ------------------------------------------------------------ transport

    def request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        json: Any | None = None,
    ) -> Any:
        # The credentials are set per request, not on the client, so an injected
        # client (tests) sends them too and the token has one home.
        response = self._http.request(
            method,
            f"{self.base_url}{path}",
            params=_query(params),
            json=json,
            headers={
                "Authorization": f"Bearer {self.token}",
                "Accept": "application/json",
            },
        )
        # A redirect means the base URL is not the API itself: a reverse proxy
        # mounted under a prefix (QDRUST_URL without it) answers with the SPA.
        # Saying so beats an unparsable body two lines further down.
        if 300 <= response.status_code < 400:
            location = response.headers.get("location") or "?"
            raise QdrustError(
                f"qdrust answered {response.status_code} (redirect to {location}): "
                "QDRUST_URL does not point at the API — check the URL and its prefix"
            )
        if response.status_code >= 400:
            raise QdrustError(f"qdrust returned {response.status_code}: {_summarise(response)}")
        if not response.content:
            return None
        try:
            return response.json()
        except ValueError:
            # A 200 that is not JSON is the other half of the wrong-URL story:
            # the SPA fallback serves index.html with 200 for any unknown path.
            content_type = response.headers.get("content-type", "unknown type")
            raise QdrustError(
                f"qdrust returned {response.status_code} {content_type}, which is not JSON: "
                f"{_preview(response)!r} — is QDRUST_URL pointing at the API root?"
            ) from None

    def get(self, path: str, *, params: dict[str, Any] | None = None) -> Any:
        return self.request("GET", path, params=params)

    def post(self, path: str, json: Any | None = None) -> Any:
        return self.request("POST", path, json=json)

    def put(self, path: str, json: Any | None = None) -> Any:
        return self.request("PUT", path, json=json)

    def delete(self, path: str) -> Any:
        return self.request("DELETE", path)

    # ---------------------------------------------------------------- tasks

    def list_tasks(self, grp: str | None = None) -> Any:
        return self.get("/api/v1/tasks", params={"grp": grp})

    def get_task(self, task_id: int) -> Any:
        return self.get(f"/api/v1/tasks/{task_id}")

    def create_task(
        self,
        *,
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
        variables: Any | None = None,
    ) -> Any:
        return self.post(
            "/api/v1/tasks",
            present(
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
            ),
        )

    def update_task(
        self,
        task_id: int,
        *,
        name: str | None = None,
        cron: str | None = None,
        disabled: bool | None = None,
        grp: str | None = None,
        timezone: str | None = None,
        variables: Any | None = None,
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
    ) -> Any:
        payload = present(
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
        )
        for field in clear or ():
            if field not in CLEARABLE_FIELDS:
                raise QdrustError(
                    f"cannot clear {field!r}: known fields are "
                    f"{', '.join(sorted(CLEARABLE_FIELDS))}"
                )
            if field in payload:
                raise QdrustError(
                    f"{field!r} was both given a value and listed in clear: pass one or the other"
                )
            # An explicit null is the server's "erase this" — see CLEARABLE_FIELDS.
            payload[field] = None
        return self.put(f"/api/v1/tasks/{task_id}", payload)

    def delete_task(self, task_id: int) -> Any:
        return self.delete(f"/api/v1/tasks/{task_id}")

    def run_task(self, task_id: int) -> Any:
        return self.post(f"/api/v1/tasks/{task_id}/run")

    def list_task_runs(self, task_id: int) -> Any:
        return self.get(f"/api/v1/tasks/{task_id}/runs")

    def batch_tasks(self, *, ids: list[int], action: str) -> Any:
        return self.post("/api/v1/tasks/batch", {"ids": ids, "action": action})

    def list_task_groups(self) -> Any:
        return self.get("/api/v1/task-groups")

    # ----------------------------------------------------------------- runs

    def list_runs(
        self,
        *,
        status: str | None = None,
        task_id: int | None = None,
        limit: int | None = None,
        before_id: int | None = None,
    ) -> Any:
        return self.get(
            "/api/v1/runs",
            params={
                "status": status,
                "task_id": task_id,
                "limit": limit,
                "before_id": before_id,
            },
        )

    def get_run_steps(self, run_id: int) -> Any:
        return self.get(f"/api/v1/runs/{run_id}/steps")

    def cancel_run(self, run_id: int) -> Any:
        return self.post(f"/api/v1/runs/{run_id}/cancel")

    def delete_run(self, run_id: int) -> Any:
        return self.delete(f"/api/v1/runs/{run_id}")

    # ------------------------------------------------------------ templates

    def list_templates(
        self,
        *,
        q: str | None = None,
        grp: str | None = None,
        cursor: int | None = None,
        limit: int | None = None,
    ) -> Any:
        return self.get(
            "/api/v1/templates",
            params={"q": q, "grp": grp, "cursor": cursor, "limit": limit},
        )

    def get_template(self, template_id: int) -> Any:
        return self.get(f"/api/v1/templates/{template_id}")

    def test_template(self, template_id: int, *, variables: Any | None = None) -> Any:
        return self.post(
            f"/api/v1/templates/{template_id}/test",
            {"variables": variables or {}},
        )

    def import_template(
        self,
        *,
        name: str,
        har: Any,
        description: str | None = None,
    ) -> Any:
        return self.post(
            "/api/v1/templates/import-qd-har",
            present(name=name, har=har, description=description),
        )

    # --------------------------------------------------------------- notepads

    def list_notepads(self) -> Any:
        return self.get("/api/v1/notepads")

    def get_notepad(self, notepad_id: int) -> Any:
        return self.get(f"/api/v1/notepads/{notepad_id}")

    def set_notepad(self, notepad_id: int, content: str) -> Any:
        return self.put(f"/api/v1/notepads/{notepad_id}", {"content": content})

    def delete_notepad(self, notepad_id: int) -> Any:
        return self.delete(f"/api/v1/notepads/{notepad_id}")

    # --------------------------------------------------------- notifications

    def list_notification_channels(self) -> Any:
        return self.get("/api/v1/notification-channels")

    def bind_notification(
        self,
        *,
        task_ids: list[int],
        channel_id: int,
        event: str | None = None,
    ) -> Any:
        return self.post(
            "/api/v1/notification-actions/batch",
            {
                "task_ids": task_ids,
                "channel_id": channel_id,
                "event": event or "failure",
                "failure_threshold": 1,
                "automatic_only": False,
            },
        )

    def list_notification_actions(self) -> Any:
        """Every binding the token's owner has, across all of their tasks."""
        return self.get("/api/v1/notification-actions")

    def preview_notification_action(self, action_id: int, run_id: int | None = None) -> Any:
        """Render one binding's message without delivering it.

        ``run_id`` renders against a specific run; the task's newest one is used
        otherwise. A blank ``run_id`` is dropped rather than sent as an empty
        query value — see ``_query``.
        """
        return self.get(
            f"/api/v1/notification-actions/{action_id}/preview",
            params={"run_id": run_id},
        )

    def notification_defaults(self) -> Any:
        """The owner's default templates per event, plus the built-in pair."""
        return self.get("/api/v1/notification-defaults")

    def set_notification_default(
        self,
        event: str,
        *,
        title_template: str = "",
        body_template: str = "",
    ) -> Any:
        """Replace the owner's default templates for one event.

        Checked here as well as on the server so a typo comes back as a sentence
        rather than a 422 whose `field_errors` are empty: an event is one of two
        words, not a free-form label.
        """
        if event not in ("success", "failure"):
            raise QdrustError(
                f"event must be 'success' or 'failure', got {event!r}: a default is the "
                "wording for an outcome, while 'always' belongs to a binding"
            )
        return self.put(
            f"/api/v1/notification-defaults/{event}",
            {"title_template": title_template, "body_template": body_template},
        )
