# Tiny static server for the cc-screen docs site.
# Multi-stage: build a small musl binary, then serve ./public from a minimal,
# non-root Alpine runtime. Plain HTTP on :8080 (Dibbla terminates TLS upstream).

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM alpine:3.20
RUN adduser -D -u 10001 app
WORKDIR /app
COPY --from=build /app/target/release/cc-screen-site /usr/local/bin/cc-screen-site
COPY public ./public
# Hosted release assets (installers + binaries), served at /dl. Staged by
# release-host.sh from the CI build artifacts; just a .gitkeep on a fresh checkout.
COPY dl ./public/dl
USER app
ENV PORT=8080
EXPOSE 8080
CMD ["cc-screen-site"]
