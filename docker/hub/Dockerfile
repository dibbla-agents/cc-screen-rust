# syntax=docker/dockerfile:1
#
# Container image for the cc-screen-rust **hub** (the aggregator). The hub owns no
# PTY and no filesystem — it's a registry + client-auth gate + byte relay — so it
# containerizes cleanly. It serves the embedded React PWA, so it must be built
# AFTER frontend/dist, hence the node stage below.
#
# Build context is the repo ROOT (the hub is a Cargo workspace member):
#   docker build -t cc-screen-hub -f docker/hub/Dockerfile .
# or just use docker/hub/docker-compose.yml.

# ── Stage 1: build the PWA the hub embeds ────────────────────────────────────
FROM node:20-alpine AS frontend
WORKDIR /app/frontend
# Install against the lockfile first for a cacheable layer.
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build          # → /app/frontend/dist

# ── Stage 2: build the hub binary (static musl) ──────────────────────────────
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# The whole workspace is needed to resolve + build the member crate. target/ and
# node_modules/ are kept out by .dockerignore.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
# The hub embeds the PWA at compile time (rust-embed → ../../frontend/dist).
COPY --from=frontend /app/frontend/dist ./frontend/dist
# Cache the cargo registry + target dir across builds. Because target/ is a cache
# mount it isn't in the image layer, so copy the binary out before the RUN ends.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p cc-screen-hub && \
    cp target/release/cc-screen-hub /cc-screen-hub

# ── Stage 3: minimal non-root runtime ────────────────────────────────────────
FROM alpine:3.20
RUN adduser -D -u 10001 app
COPY --from=build /cc-screen-hub /usr/local/bin/cc-screen-hub
USER app
# Persisted state (session.key cookie-signing key, favorites, VAPID push keys,
# push subscriptions) lives under $HOME/.config/cc-screen-hub — mount a volume
# there to survive restarts.
ENV HOME=/home/app
# Listen on all interfaces inside the container; the host port mapping / proxy
# decides actual exposure. (Default without this is 127.0.0.1:8840, unreachable
# from outside the container.)
ENV CCWEB_ADDR=0.0.0.0:8840
EXPOSE 8840
CMD ["cc-screen-hub"]
