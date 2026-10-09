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

See [docs/mcp.md](../docs/mcp.md) for the client configuration, the tool list,
and a ready-to-paste prompt that has an AI assistant do the whole setup.

```bash
uv sync                 # create the venv
uv run ruff check       # lint
uv run pytest           # unit + stdio integration tests
```
