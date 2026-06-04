# syntax=docker/dockerfile:1
#
# Container image for the cc-screen-rust **agent** (the machine host / server). It
# spawns PTYs running the YOLO coding CLIs (claude/codex/gemini/…), so the image
# bundles Node + those CLIs on a full Ubuntu (glibc) userland. Running it in a
# container is a feature, not a workaround: the `--dangerously-skip-permissions`
# agents are confined to the container + the home volume you mount, not your host.
#
# Build context is the repo ROOT (the agent is the workspace root package):
#   docker build -t cc-screen-agent -f docker/agent/Dockerfile .
# or use docker/agent/docker-compose.yml.

# ── Stage 1: build the embedded PWA ──────────────────────────────────────────
FROM node:20-alpine AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build          # → /app/frontend/dist

# ── Stage 2: build the agent binary (glibc) ──────────────────────────────────
# Built on Debian (glibc 2.36) so it runs on the Ubuntu 24.04 runtime below.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
COPY --from=frontend /app/frontend/dist ./frontend/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p cc-screen-rust && \
    cp target/release/cc-screen-rust /cc-screen-rust

# ── Stage 3: Ubuntu runtime + Node + the coding CLIs ─────────────────────────
FROM ubuntu:24.04

# Userland the agents and the projects they work on expect. Add your own (e.g.
# go, rustup, pnpm) as needed — this is the box your assistants actually run in.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git ripgrep less jq nano \
      python3 python3-venv build-essential tini \
    && rm -rf /var/lib/apt/lists/*

# Node.js 22 — the coding CLIs below are npm packages.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# The YOLO coding CLIs the agent launches by default (cmd → prefix):
#   claude (cc), gemini (gc), codex (coc)  — installed here.
#   kimi   (kc)                            — distribution varies; add it yourself.
#   shell  (tt) → bash                     — already present.
# Each is OPTIONAL: the agent lists every tool regardless, and a missing binary
# just fails that one session. Package names can change upstream — pin/verify.
RUN npm install -g \
      @anthropic-ai/claude-code \
      @google/gemini-cli \
      @openai/codex \
    && npm cache clean --force

# Non-root user. Its $HOME is where you mount projects, the CLIs' login dirs, and
# the agent's own state (~/.config/cc-screen-rust/sessions.json, session.key, …).
RUN useradd -m -u 10001 -s /bin/bash app
COPY --from=build /cc-screen-rust /usr/local/bin/cc-screen-rust

USER app
ENV HOME=/home/app
WORKDIR /home/app
# Bind to all interfaces *inside* the container; the host port mapping (or running
# --hub-only with no port at all) decides real exposure. This box runs YOLO agents
# — never publish :8839 to a public interface. (CCWEB_HUB_ONLY=1 drops the bind.)
ENV CCWEB_ADDR=0.0.0.0:8839
EXPOSE 8839
# tini as PID 1 reaps the PTY child processes the CLIs spawn.
ENTRYPOINT ["tini", "--"]
CMD ["cc-screen-rust"]
