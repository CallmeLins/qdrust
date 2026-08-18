# API error code contract

API responses use the stable `code` field as their message key. Clients translate that key locally and may fall back to the server-provided English `message`. Adding a locale must not change HTTP status codes or error codes.

| Code | HTTP status | Default message | Meaning |
| --- | --- | --- | --- |
| `api_endpoint_not_found` | 404 | API endpoint not found | The requested API route does not exist |
| `task_not_found` | 404 | Task not found | The requested task does not exist |
| `run_not_found` | 404 | Run not found | The requested run does not exist or is not owned by the user |
| `template_not_found` | 404 | Template not found | The requested template does not exist |
| `validation_error` | 422 | Request-specific | JSON or domain validation failed |
| `internal_error` | 500 | An internal error occurred | An unexpected internal failure occurred |
| `authentication_required` | 401 | Authentication required | Session is absent, expired, revoked, or disabled |
| `invalid_credentials` | 401 | Invalid username or password | Login credentials did not validate |
| `csrf_validation_failed` | 403 | CSRF validation failed | CSRF cookie/header/session binding did not validate |
| `bootstrap_already_completed` | 409 | Initial administrator already exists | First-user initialization cannot run again |
| `login_rate_limited` | 429 | Too many login attempts | Login attempts for this username are temporarily throttled |

Every error response also includes an opaque `request_id` and `field_errors`. Internal error details are only written to server logs.
