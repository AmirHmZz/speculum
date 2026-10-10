# syntax=docker/dockerfile:1
FROM oven/bun:1.4.2-alpine AS dashboard
WORKDIR /build/dashboard
COPY dashboard/package.json dashboard/bun.lock ./
RUN bun install --frozen-lockfile
COPY dashboard/ ./
RUN bun run build

FROM rust:1.98-bookworm AS rust
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ ./src/
COPY --from=dashboard /build/dashboard/dist/ ./dashboard/dist/
ENV SPECULUM_REQUIRE_DASHBOARD=1
RUN --mount=type=cache,id=speculum-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=speculum-cargo-target,target=/build/target,sharing=locked \
    cargo build --locked --release && cp target/release/speculum /build/speculum

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 speculum && useradd --uid 10001 --gid 10001 --no-create-home speculum \
    && mkdir -p /app/data && chown 10001:10001 /app/data
WORKDIR /app
COPY --from=rust /build/speculum /usr/local/bin/speculum
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 CMD ["speculum", "healthcheck"]
ENTRYPOINT ["speculum"]
