# AI Gateway

The Dibbla AI gateway lets a deployed app (or a developer's laptop) talk to OpenAI- and Anthropic-compatible LLM APIs **using the user's Dibbla API token** instead of a provider key. Every call is captured, attributed to the user (and, optionally, to the calling app), and visible in the gateway's console for the org.

Why apps should use it:

- **One key.** The app authenticates with the same Dibbla token it already uses for everything else. The provider key (OpenAI / Anthropic) lives only on the platform — never in the app's secrets, never in the customer's image.
- **Audit trail.** Every prompt, response, token count, latency, and tool call lands in `ai_gateway_db` for the user's org and is browsable at `https://ai.dibbla.net/console`.
- **Per-app attribution.** When the app declares which Dibbla app it is via the `X-Dibbla-App` header, the call is tagged with the app alias in the ledger.

## Endpoints

| Path on `ai.dibbla.net` | Compatible with | Notes |
|---|---|---|
| `/openai/v1/...` | OpenAI SDKs (Node `openai`, Python `openai`, Go `openai-go`, …) | Set the SDK's `base_url` to `https://ai.dibbla.net/openai/v1`. |
| `/anthropic` | Anthropic SDKs | The Anthropic SDK appends `/v1/messages` itself, so the base URL ends at `/anthropic`. |
| `/health` | — | unauthenticated liveness check. |
| `/console/` | Browser | dashboard (cookie auth via the same Dibbla session as `app.dibbla.net`). |

## Authentication

Pass the user's **Dibbla API token** the same way you'd pass an OpenAI/Anthropic key. The gateway validates it with `auth-service` and swaps it for the platform-managed provider key on the way out — neither the customer nor their app ever sees the upstream key.

| SDK | Header set automatically | What to put as the key |
|---|---|---|
| OpenAI Node/Python/Go | `Authorization: Bearer <key>` | the Dibbla API token |
| Anthropic Node/Python/Go | `x-api-key: <key>` | the Dibbla API token |

Get a token interactively at `https://app.dibbla.net/api-keys`, or in CI via `dibbla login --api-key=<token>`. Don't burn a provider key into the app — the gateway makes that obsolete.

## The `X-Dibbla-App` header (per-app attribution)

When a Dibbla-deployed app calls the gateway, it can identify itself with one extra header:

```
X-Dibbla-App: <DIBBLA_ALIAS value>
X-Dibbla-App-Service: <DIBBLA_SERVICE_NAME value>   # optional
```

The gateway cross-checks the alias against the user's organization in deploy-api. If (alias, org) matches a real deployment, the call is recorded with `source=app` and `app_alias=<alias>` in the ledger. If it doesn't (wrong org, typo, never deployed), the request **still succeeds** — it's just recorded as `source=external` with no app attribution. The gateway never fails an LLM call because of an attribution miss.

A laptop / Claude Code / Cursor user makes calls with no `X-Dibbla-App` and shows up as `source=external` in the ledger — that's the right thing.

## Environment variables auto-injected by the platform

Every Dibbla-deployed pod gets these env vars without the user setting anything:

| Variable | Value | Purpose |
|---|---|---|
| `DIBBLA_ALIAS` | the app's alias (e.g. `shop`) | use as the value of `X-Dibbla-App`. |
| `DIBBLA_AI_GATEWAY_URL` | `https://ai.dibbla.net` (env-driven) | base URL the SDK should point at. |
| `DIBBLA_SERVICE_NAME` | this pod's service name (e.g. `web`) | optional value of `X-Dibbla-App-Service`. |

`DIBBLA_AI_GATEWAY_URL` is empty in environments where deploy-api isn't configured to inject it; treat empty as "no gateway, fall back to direct provider calls".

## Code patterns

### Node — OpenAI SDK

```js
import OpenAI from 'openai'

const client = new OpenAI({
  apiKey: process.env.DIBBLA_API_TOKEN,
  baseURL: `${process.env.DIBBLA_AI_GATEWAY_URL}/openai/v1`,
  defaultHeaders: { 'X-Dibbla-App': process.env.DIBBLA_ALIAS },
})

const reply = await client.chat.completions.create({
  model: 'gpt-4o-mini',
  messages: [{ role: 'user', content: 'hello' }],
})
```

### Python — OpenAI SDK

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["DIBBLA_API_TOKEN"],
    base_url=f"{os.environ['DIBBLA_AI_GATEWAY_URL']}/openai/v1",
    default_headers={"X-Dibbla-App": os.environ["DIBBLA_ALIAS"]},
)

reply = client.chat.completions.create(
    model="gpt-4o-mini",
    messages=[{"role": "user", "content": "hello"}],
)
```

### Node — Anthropic SDK

```js
import Anthropic from '@anthropic-ai/sdk'

const client = new Anthropic({
  apiKey: process.env.DIBBLA_API_TOKEN,         // sent as x-api-key
  baseURL: `${process.env.DIBBLA_AI_GATEWAY_URL}/anthropic`,
  defaultHeaders: { 'X-Dibbla-App': process.env.DIBBLA_ALIAS },
})

const msg = await client.messages.create({
  model: 'claude-sonnet-4-6',
  max_tokens: 1024,
  messages: [{ role: 'user', content: 'hello' }],
})
```

### Go — `openai-go`

```go
import (
    "os"
    openai "github.com/openai/openai-go"
    "github.com/openai/openai-go/option"
)

client := openai.NewClient(
    option.WithAPIKey(os.Getenv("DIBBLA_API_TOKEN")),
    option.WithBaseURL(os.Getenv("DIBBLA_AI_GATEWAY_URL")+"/openai/v1"),
    option.WithHeader("X-Dibbla-App", os.Getenv("DIBBLA_ALIAS")),
)
```

### curl

```bash
curl https://ai.dibbla.net/openai/v1/chat/completions \
  -H "Authorization: Bearer $DIBBLA_API_TOKEN" \
  -H "X-Dibbla-App: $DIBBLA_ALIAS" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}'
```

## Console

Org members can browse all captured traffic at `https://ai.dibbla.net/console` — ledger view (filtered by user, app, or all org), trace drawer with full request/response and tool calls, live-updating as new calls land. The console uses the same Dibbla session cookie that signs into `app.dibbla.net`, so no extra login.

## Rules of thumb

- **Don't smuggle provider keys past the gateway.** If the app is configured with both a Dibbla token and an `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`, point the SDK at the gateway anyway. The gateway is the audit boundary.
- **Always read the gateway URL from `DIBBLA_AI_GATEWAY_URL`.** Don't hard-code `https://ai.dibbla.net`; that string changes between dev/staging/prod.
- **The `X-Dibbla-App` header is best-effort, not authentication.** Setting it without the user's Dibbla token, or with an alias from a different org, will not unlock anything — it just gets ignored. Don't use it as a security boundary.
- **Streaming works the same way as direct calls.** SSE is forwarded byte-for-byte with no buffering; the gateway also tees a copy into the parser so the captured record carries full content blocks. Use `stream: true` (OpenAI) or `stream: true` (Anthropic) exactly as you would direct.
