# qdrust-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server that exposes one
qdrust instance's REST API to MCP clients (Claude Desktop, Cursor, …), so a model
can create, run and inspect tasks without leaving the client.

Pure Python, no compile step. It is a thin, authenticated HTTP client: every tool
is one call to qdrust's REST API.

```bash
# from a checkout of this repo
uvx --from ./mcp qdrust-mcp            # or: uv run --project mcp qdrust-mcp
```

Configuration is environment-only:

| Variable | Meaning |
|---|---|
| `QDRUST_URL` | Base URL of the qdrust server. Default `http://localhost:8923`. |
| `QDRUST_TOKEN` | Personal access token (`qd_…`) from **Settings → API tokens**. Required. |
| `QDRUST_TIMEOUT` | Per-request timeout in seconds. Default `120`. |

See [docs/mcp.md](../docs/mcp.md) for the client configuration, the tool list,
and a ready-to-paste prompt that has an AI assistant do the whole setup.

```bash
uv sync                  # create the venv
uv run ruff format --check
uv run ruff check        # lint
uv run pytest            # unit + stdio integration tests
```

There is deliberately **no committed `uv.lock`**: `uvx` resolves the dependencies
when a client starts the server, and CI runs the tests the same way, so a
breaking change in the `mcp` SDK shows up as a red build instead of a stale pin
nobody bumps. The trade-off is that two machines may run different SDK versions;
what must hold is pinned by tests instead, and CI also runs them against the
oldest supported SDK (see the `mcp` job in `.github/workflows/ci.yml`). The
`mcp` floor in `pyproject.toml` is that oldest-verified version — a lower one
cannot be installed at all today.
