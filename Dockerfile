# Sub-path support. The WebUI is built with a RELATIVE base by default, so this
# image serves correctly at the bare root OR any reverse-proxied sub-directory
# (e.g. https://host/qd) without rebuilding — set QDRUST_BASE_PATH=/qd at
# runtime to match the proxy prefix. VITE_BASE_PATH below is only for the
# legacy absolute-base mode (build-time pinning); leave it empty for the
# runtime-adaptive default.
ARG VITE_BASE_PATH=
FROM node:24-bookworm-slim AS web-builder
WORKDIR /build
COPY docs/openapi-v1.json docs/openapi-v1.json
COPY webui/package.json webui/package-lock.json webui/
RUN npm --prefix webui ci
COPY webui webui
RUN npm --prefix webui run generate:api && VITE_BASE_PATH="$VITE_BASE_PATH" npm --prefix webui run build

FROM rust:1.97-bookworm AS rust-builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY migrations migrations
COPY migrations-mysql migrations-mysql
COPY docs/openapi-v1.json docs/openapi-v1.json
# Build the server. The browser plugin (chromiumoxide/CDP) is compiled in as a
# library dependency, so only qdrust-server needs to be produced.
RUN cargo build --locked --release -p qdrust-server

FROM debian:bookworm-slim AS runtime
# The tag only ships the packages of its last rebuild, so a fix published in
# bookworm-security (e.g. libpcre2-8-0 10.42-1 -> 10.42-1+deb12u1) does not
# reach us until Debian cuts the next point release. That window is enough to
# fail the release image's Trivy gate, which blocks on any vulnerability that
# has a fixed version. Upgrade in place rather than trusting the tag; the
# archive and its -security/-updates suites are live mirrors in the official
# image, so this picks the fixes up. DEBIAN_SECURITY_REFRESH is a cache-buster
# (the release workflow passes a per-build value): without it buildx would keep
# serving the layer cached on the first build and the image would stay frozen
# on whatever was current then — red gate, nothing a rebuild could fix.
ARG DEBIAN_SECURITY_REFRESH=local
RUN echo "debian archive refresh: $DEBIAN_SECURITY_REFRESH" \
    && export DEBIAN_FRONTEND=noninteractive \
    && apt-get update \
    && apt-get -y upgrade \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 qdrust \
    && useradd --uid 10001 --gid qdrust --no-create-home --shell /usr/sbin/nologin qdrust
WORKDIR /app
COPY --from=rust-builder /build/target/release/qdrust-server /usr/local/bin/qdrust-server
COPY --from=web-builder /build/webui/dist webui/dist
RUN mkdir -p /data && chown qdrust:qdrust /data
USER qdrust:qdrust
ENV BIND=0.0.0.0 \
    PORT=8923 \
    DATABASE_URL=sqlite:///data/qdrust.db \
    DATABASE_MIN_CONNECTIONS=1 \
    DATABASE_MAX_CONNECTIONS=8 \
    RUST_LOG=qdrust_server=info,qdrust_core=info,tower_http=info \
    QDRUST_DEFAULT_TIMEZONE=Asia/Shanghai \
    QDRUST_BASE_PATH=
VOLUME ["/data"]
EXPOSE 8923
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["curl", "--fail", "--silent", "http://127.0.0.1:8923/health"]
ENTRYPOINT ["qdrust-server"]
