# Sametra node: the web app is built first and embedded into the binary.
# `docker compose up` runs it next to a Kubo IPFS daemon (see docker-compose.yml).

# ---- web app (Astro + Preact) -------------------------------------------------
FROM node:26-bookworm-slim AS web
RUN npm install -g pnpm@12.22.0
WORKDIR /src/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY proto/ /src/proto/
COPY web/ ./
RUN pnpm build

# ---- node and sametra-mcp -------------------------------------------------------
FROM rust:1.98-bookworm AS build
WORKDIR /src
COPY . .
COPY --from=web /src/web/dist web/dist
RUN cargo build --release --locked --workspace \
 && cp target/release/sametra target/release/sametra-mcp /usr/local/bin/

# ---- runtime ----------------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data sametra \
 && mkdir -p /data && chown sametra /data
COPY --from=build /usr/local/bin/sametra /usr/local/bin/sametra-mcp /usr/local/bin/
USER sametra
ENV SAMETRA_DATA=/data
VOLUME /data
EXPOSE 42424
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD ["sametra", "call", "info", "--api", "http://127.0.0.1:42424"]
ENTRYPOINT ["sametra"]
CMD ["run", "--listen", "0.0.0.0:42424"]
