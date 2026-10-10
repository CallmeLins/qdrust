"""The REST mapping: path, query, body and the "omit means unchanged" rule."""

from __future__ import annotations

import json

import httpx
import pytest

from qdrust_mcp.client import QdrustClient, QdrustError, present


def make_client(handler, *, token: str = "qd_test") -> QdrustClient:
    return QdrustClient(
        "http://qd.test",
        token,
        http=httpx.Client(transport=httpx.MockTransport(handler)),
    )


def test_requires_a_token(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("QDRUST_TOKEN", raising=False)
    with pytest.raises(QdrustError):
        QdrustClient()


def test_present_drops_unset_fields_but_keeps_empty_ones() -> None:
    assert present(a=1, b=None, c="") == {"a": 1, "c": ""}


def test_list_tasks_sends_the_bearer_and_omits_an_unset_group() -> None:
    seen: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["auth"] = request.headers.get("authorization", "")
        return httpx.Response(200, json=[])

    assert make_client(handler).list_tasks() == []
    assert seen["url"] == "http://qd.test/api/v1/tasks"
    assert seen["auth"] == "Bearer qd_test"


def test_list_tasks_passes_the_group() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json=[])

    make_client(handler).list_tasks("daily")
    assert seen["url"] == "http://qd.test/api/v1/tasks?grp=daily"


def test_a_blank_group_is_no_filter_at_all() -> None:
    # `?grp=` would ask for the tasks grouped under "" — an ungrouped task holds
    # NULL, so it would silently match nothing.
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json=[])

    client = make_client(handler)
    client.list_tasks("")
    assert seen["url"] == "http://qd.test/api/v1/tasks"
    client.list_tasks("   ")
    assert seen["url"] == "http://qd.test/api/v1/tasks"


def test_paging_and_template_filters_reach_the_query_string() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json=[])

    client = make_client(handler)
    client.list_runs(status="failed", task_id=3, before_id=99)
    assert seen["url"] == "http://qd.test/api/v1/runs?status=failed&task_id=3&before_id=99"
    client.list_templates(grp="daily", cursor=12)
    assert seen["url"] == "http://qd.test/api/v1/templates?grp=daily&cursor=12"


def test_update_task_sends_only_the_fields_the_caller_set() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["method"] = request.method
        seen["body"] = json.loads(request.content)
        return httpx.Response(200, json={})

    make_client(handler).update_task(7, disabled=True)
    assert seen["method"] == "PUT"
    # `cron` was not passed: the server must keep the stored value, so it must
    # not appear as null.
    assert seen["body"] == {"disabled": True}


def test_update_task_can_erase_a_field_with_an_explicit_null() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(request.content)
        return httpx.Response(200, json={})

    make_client(handler).update_task(7, name="renamed", clear=["grp", "timezone"])
    assert seen["body"] == {"name": "renamed", "grp": None, "timezone": None}


def test_clearing_rejects_an_unknown_field_and_a_double_assignment() -> None:
    client = make_client(lambda request: httpx.Response(200, json={}))
    with pytest.raises(QdrustError, match="cannot clear 'cron'"):
        client.update_task(7, clear=["cron"])
    with pytest.raises(QdrustError, match="both given a value and listed in clear"):
        client.update_task(7, grp="daily", clear=["grp"])


def test_create_task_carries_a_template_and_variables() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(request.content)
        return httpx.Response(201, json={})

    make_client(handler).create_task(
        name="check-in",
        cron="0 0 8 * * * *",
        template_id=3,
        variables={"username": "me"},
    )
    assert seen["body"] == {
        "name": "check-in",
        "cron": "0 0 8 * * * *",
        "template_id": 3,
        "variables": {"username": "me"},
    }


def test_run_and_cancel_send_no_body() -> None:
    calls = []

    def handler(request: httpx.Request) -> httpx.Response:
        calls.append((request.method, request.url.path, request.content))
        return httpx.Response(200, json={})

    client = make_client(handler)
    client.run_task(1)
    client.cancel_run(9)
    assert calls == [
        ("POST", "/api/v1/tasks/1/run", b""),
        ("POST", "/api/v1/runs/9/cancel", b""),
    ]


def test_bind_notification_defaults_to_the_failure_event() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(request.content)
        return httpx.Response(200, json={"created": 1})

    make_client(handler).bind_notification(task_ids=[1, 2], channel_id=4)
    assert seen["body"] == {
        "task_ids": [1, 2],
        "channel_id": 4,
        "event": "failure",
        "failure_threshold": 1,
        "automatic_only": False,
    }


def test_list_notification_actions_is_one_get() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json=[])

    assert make_client(handler).list_notification_actions() == []
    assert seen["url"] == "http://qd.test/api/v1/notification-actions"


def test_preview_notification_omits_an_unset_run() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json={"source": "sample"})

    client = make_client(handler)
    client.preview_notification_action(7)
    # `?run_id=` would be read by the server as "the run whose id is empty",
    # which is the same mistake `_query` exists to avoid elsewhere.
    assert seen["url"] == "http://qd.test/api/v1/notification-actions/7/preview"

    client.preview_notification_action(7, 42)
    assert seen["url"] == "http://qd.test/api/v1/notification-actions/7/preview?run_id=42"


def test_set_notification_default_replaces_one_event() -> None:
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["method"] = request.method
        seen["url"] = str(request.url)
        seen["body"] = json.loads(request.content)
        return httpx.Response(200, json={"event": "success"})

    make_client(handler).set_notification_default(
        "success", title_template="OK {task}", body_template="{log}"
    )
    assert seen["method"] == "PUT"
    assert seen["url"] == "http://qd.test/api/v1/notification-defaults/success"
    assert seen["body"] == {"title_template": "OK {task}", "body_template": "{log}"}


def test_set_notification_default_refuses_an_event_it_cannot_store() -> None:
    def handler(request: httpx.Request) -> httpx.Response:  # pragma: no cover
        raise AssertionError("no request should be sent")

    client = make_client(handler)
    with pytest.raises(QdrustError, match="success.*failure"):
        client.set_notification_default("always")
    with pytest.raises(QdrustError, match="success.*failure"):
        client.set_notification_default("Failure")


def test_a_refused_request_carries_qdrusts_own_message() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            422,
            json={"code": "validation_error", "message": "task URL is required"},
        )

    with pytest.raises(QdrustError) as raised:
        make_client(handler).run_task(1)
    assert "task URL is required" in str(raised.value)


def test_an_empty_success_body_is_not_parsed_as_json() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(204)

    assert make_client(handler).delete_task(5) is None


def test_notepad_paths_and_the_slot_body() -> None:
    calls = []

    def handler(request: httpx.Request) -> httpx.Response:
        calls.append((request.method, request.url.path, request.content))
        return httpx.Response(200, json={})

    client = make_client(handler)
    client.list_notepads()
    client.get_notepad(2)
    client.set_notepad(2, "value")
    client.delete_notepad(2)
    client.delete_run(9)
    assert calls == [
        ("GET", "/api/v1/notepads", b""),
        ("GET", "/api/v1/notepads/2", b""),
        ("PUT", "/api/v1/notepads/2", b'{"content":"value"}'),
        ("DELETE", "/api/v1/notepads/2", b""),
        ("DELETE", "/api/v1/runs/9", b""),
    ]


def test_a_redirect_names_the_likely_cause() -> None:
    # A proxy mounted under a prefix answers with the SPA, not the API.
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(302, headers={"location": "/qd/"})

    with pytest.raises(QdrustError) as raised:
        make_client(handler).list_tasks()
    assert "QDRUST_URL does not point at the API" in str(raised.value)


def test_a_200_that_is_html_is_reported_as_such() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            text="<!doctype html><title>qdrust</title>",
            headers={"content-type": "text/html"},
        )

    with pytest.raises(QdrustError) as raised:
        make_client(handler).list_tasks()
    message = str(raised.value)
    assert "not JSON" in message
    assert "text/html" in message
    assert "<!doctype html>" in message


def test_the_timeout_comes_from_the_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("QDRUST_TIMEOUT", "5")
    assert QdrustClient("http://qd.test", "qd_test")._http.timeout.connect == 5.0

    monkeypatch.setenv("QDRUST_TIMEOUT", "not-a-number")
    with pytest.raises(QdrustError, match="must be a number"):
        QdrustClient("http://qd.test", "qd_test")

    monkeypatch.setenv("QDRUST_TIMEOUT", "0")
    with pytest.raises(QdrustError, match="must be positive"):
        QdrustClient("http://qd.test", "qd_test")
