# cc-screen-site

A tiny Rust (axum + tower-http) static-file server that hosts the cc-screen docs
site on **Dibbla** as the `cc-screen` app.

`../docs` is the single source of truth (also served by GitHub Pages). This crate
just serves a copy of it from a small, non-root Alpine container.

## Deploy

```sh
./deploy.sh              # syncs ../docs -> ./public, then `dibbla deploy`
./deploy.sh "msg"        # with a custom commit message
```

`deploy.sh` creates the app on first run and does a zero-downtime `--update` after
that. The site is then live at `https://cc-screen-<id>.dibbla.app`.

## Local check

```sh
cp -R ../docs/. public/
docker build -t cc-screen-site .
docker run --rm -p 8080:8080 cc-screen-site   # -> http://localhost:8080
```
