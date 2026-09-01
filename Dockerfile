FROM node:24-bookworm-slim AS web-builder
WORKDIR /build
COPY docs/openapi-v1.json docs/openapi-v1.json
COPY webui/package.json webui/package-lock.json webui/
RUN npm --prefix webui ci
COPY webui webui
RUN npm --prefix webui run generate:api && npm --prefix webui run build

FROM rust:1.97-bookworm AS rust-builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY migrations migrations
COPY migrations-mysql migrations-mysql
COPY docs/openapi-v1.json docs/openapi-v1.json
RUN cargo build --locked --release -p qdrust-server -p qdrust-plugin-browser

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 qdrust \
    && useradd --uid 10001 --gid qdrust --no-create-home --shell /usr/sbin/nologin qdrust
WORKDIR /app
COPY --from=rust-builder /build/target/release/qdrust-server /usr/local/bin/qdrust-server
COPY --from=rust-builder /build/target/release/qdrust-plugin-browser /usr/local/bin/qdrust-plugin-browser
COPY --from=web-builder /build/webui/dist webui/dist
RUN mkdir -p /data && chown qdrust:qdrust /data
USER qdrust:qdrust
ENV BIND=0.0.0.0 \
    PORT=8923 \
    DATABASE_URL=sqlite:///data/qdrust.db \
    DATABASE_MIN_CONNECTIONS=1 \
    DATABASE_MAX_CONNECTIONS=8 \
    RUST_LOG=qdrust_server=info,qdrust_core=info,tower_http=info
VOLUME ["/data"]
EXPOSE 8923
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["curl", "--fail", "--silent", "http://127.0.0.1:8923/health"]
ENTRYPOINT ["qdrust-server"]
