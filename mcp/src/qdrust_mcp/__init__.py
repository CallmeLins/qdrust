"""qdrust MCP server: a stdio MCP front end for a qdrust instance's REST API."""

from importlib.metadata import PackageNotFoundError, version

try:
    __version__ = version("qdrust-mcp")
except PackageNotFoundError:  # a source checkout that was never installed
    __version__ = "0.0.0"

__all__ = ["__version__"]
