FROM node:26-bookworm-slim AS web-builder
WORKDIR /workspace
COPY package.json package-lock.json tsconfig.base.json ./
COPY apps/web/package.json apps/web/package.json
COPY apps/desktop/package.json apps/desktop/package.json
COPY packages/ui/package.json packages/ui/package.json
RUN npm ci
COPY apps/web apps/web
COPY packages/ui packages/ui
RUN npm run web:build

FROM rust:1.99-bookworm AS rust-builder
WORKDIR /workspace
COPY . .
RUN cargo build --locked --release -p rynna-cli

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 rynna
COPY --from=rust-builder /workspace/target/release/rynna /usr/local/bin/rynna
COPY --from=web-builder /workspace/apps/web/dist /opt/rynna/web
USER rynna
EXPOSE 3000
ENV RYNNA_API_BASE=http://host.docker.internal:11434/v1 \
    RYNNA_MODEL=qwen3:8b \
    RUST_LOG=warn
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:3000/healthz >/dev/null || exit 1
ENTRYPOINT ["rynna"]
CMD ["serve", "--bind", "0.0.0.0:3000", "--web-dir", "/opt/rynna/web"]
