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


class QdrustError(RuntimeError):
    """A refused or failed request, carrying qdrust's own message."""


def present(**fields: Any) -> dict[str, Any]:
    """Keep only the fields the caller actually set.

    The REST API treats an omitted field as "keep the stored value", so a
    ``null`` would be a different (and destructive) operation. A legitimate
    empty string is kept.
    """
    return {name: value for name, value in fields.items() if value is not None}


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
        timeout: float = 120.0,
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
        self._http = http or httpx.Client(timeout=timeout)

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
        query = {k: v for k, v in (params or {}).items() if v is not None}
        # The credentials are set per request, not on the client, so an injected
        # client (tests) sends them too and the token has one home.
        response = self._http.request(
            method,
            f"{self.base_url}{path}",
            params=query or None,
            json=json,
            headers={
                "Authorization": f"Bearer {self.token}",
                "Accept": "application/json",
            },
        )
        if response.status_code >= 400:
            raise QdrustError(f"qdrust returned {response.status_code}: {_summarise(response)}")
        if not response.content:
            return None
        return response.json()

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
        timeout_seconds: int | None = None,
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
                timeout_seconds=timeout_seconds,
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
    ) -> Any:
        return self.put(
            f"/api/v1/tasks/{task_id}",
            present(
                name=name,
                cron=cron,
                disabled=disabled,
                grp=grp,
                timezone=timezone,
                variables=variables,
            ),
        )

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
    ) -> Any:
        return self.get(
            "/api/v1/runs",
            params={"status": status, "task_id": task_id, "limit": limit},
        )

    def get_run_steps(self, run_id: int) -> Any:
        return self.get(f"/api/v1/runs/{run_id}/steps")

    def cancel_run(self, run_id: int) -> Any:
        return self.post(f"/api/v1/runs/{run_id}/cancel")

    # ------------------------------------------------------------ templates

    def list_templates(self, *, q: str | None = None, limit: int | None = None) -> Any:
        return self.get("/api/v1/templates", params={"q": q, "limit": limit})

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
            {"name": name, "har": har, "description": description},
        )

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
