# Dibbla Go SDK (`sdk-go`)

The Go SDK at `github.com/dibbla-agents/sdk-go` is how Go workers register **functions** and **jobs** with the Dibbla workflow runtime. A worker is a long-lived process that opens a gRPC connection to the platform, declares what it can do, and handles incoming events. The platform's workflow registry is the single source of truth — once your worker is connected, its functions become callable from any workflow YAML by `(server, function)` pair, and its jobs become triggerable from `job_trigger` nodes.

This doc covers building workers. For the workflow side (calling registered functions from YAML, validator errors, the agent+tools pattern), see [workflows.md](workflows.md). For deploying the worker as a Dibbla app, see [platform.md](platform.md).

## 1. When to use the SDK

| Goal | Use |
|---|---|
| Add a new function to the workflow function registry | **SDK** — `sdk.NewSimpleFunction` or `sdk.NewFunction` |
| Run a long-running background job that reports progress to the dashboard | **SDK** — `jobs.JobHandler` + `server.RegisterJob` |
| Call third-party APIs using a workflow user's OAuth tokens (Google/Microsoft/GitHub) | **SDK** — advanced `Function[In, Out]` with `gs.OAuth` |
| Deploy a regular HTTP app (web server, frontend, REST API) | `dibbla deploy` with a `Dockerfile`, **no SDK needed** |
| Build / iterate / call a workflow without writing Go | `dibbla wf` commands — see [workflows.md](workflows.md) |

The SDK is for **extending** the platform with custom Go logic. If you just want to run a Go HTTP server on Dibbla, you don't need the SDK at all.

## 2. Install

```bash
go get github.com/dibbla-agents/sdk-go@latest
```

- **Go ≥ 1.23.1** required.
- Single import: `import "github.com/dibbla-agents/sdk-go"` (the package name is `sdk`, not `sdkgo`).
- If you're migrating from the old `github.com/FatsharkStudiosAB/codex/workflows/workers/go/sdk` path, swap the import; the public API surface is the same.

## 3. Mental model

A worker process:

1. Calls `sdk.New(...Option)` to build a `*sdk.Server`.
2. Calls `server.RegisterFunction(...)` and `server.RegisterJob(...)` for everything it exposes.
3. Calls `server.Start()` — opens a gRPC connection, sends a registration broadcast (server name + function list + job schemas), then **blocks forever** dispatching incoming events.

The platform sees a worker as a `(server_name, [functions], [jobs])` triple. Workflow YAML references a function by **server + function name**, so renaming `WithServerName` is a breaking change for every workflow that references your functions. Pick a name early and keep it.

## 4. Server bootstrap

```go
package main

import (
    "log"
    "os"

    "github.com/dibbla-agents/sdk-go"
)

func main() {
    server, err := sdk.New(
        sdk.WithServerName("my-worker"),
        sdk.WithServerApiToken(os.Getenv("SERVER_API_TOKEN")),
    )
    if err != nil {
        log.Fatal(err)
    }

    // Register functions and jobs here, before Start().

    if err := server.Start(); err != nil {
        log.Fatal(err)
    }
}
```

`Start()` does its own `select {}` at the end — control never returns. Register everything first.

### Options (`sdk.Option`)

All options are functional; pass any subset to `sdk.New`. Each option also has an env var fallback so a Docker image can be configured without code changes.

| Option | Env var | Default | Notes |
|---|---|---|---|
| `WithServerName(name)` | `SERVER_NAME` | `codex-go-worker` | Unique per worker. Functions are keyed by `(server_name, function_name)`. |
| `WithServerApiToken(t)` | `SERVER_API_TOKEN` | _(empty)_ | Required against production. |
| `WithGrpcServerAddress(a)` | `GRPC_SERVER_ADDRESS` | `grpc.dibbla.com:443` | Override for local dev or staging. |
| `WithGrpcTLS(bool)` | `GRPC_USE_TLS` (`true`/`false`/`1`) | _auto-detect_ | Auto-on for `*.dibbla.com`, auto-off for `localhost`/`127.0.0.1`/`[::1]`. |
| `WithCodexEnvPath(path)` | `CODEX_ENV_PATH` | _(none)_ | Loads an additional `.env` after the default `.env`. |
| `WithPingInterval(sec)` | — | `30` | gRPC ping cadence (`0` disables). |

`sdk.New(...)` calls `godotenv.Load()` itself, so a `.env` next to your binary is picked up automatically.

## 5. Functions

Two builders. Pick `SimpleFunction` unless you specifically need event/state context — see §8 for why.

### 5.1 `SimpleFunction[In, Out]` — the default

For pure input → output transformations. This is the only builder that compiles cleanly from a project outside the `sdk-go` module.

```go
type GreetingInput  struct{ Name string `json:"name"` }
type GreetingOutput struct{ Message string `json:"message"` }

server.RegisterFunction(
    sdk.NewSimpleFunction[GreetingInput, GreetingOutput](
        "greeting", "1.0.0", "Greet a user by name",
    ).
        WithHandler(func(in GreetingInput) (GreetingOutput, error) {
            return GreetingOutput{Message: "Hello, " + in.Name + "!"}, nil
        }).
        WithTags("utility", "greeting"),
)
```

The generic `In` and `Out` types drive automatic JSON schema generation — the platform uses these schemas to render input forms in the dashboard, so add `json:"..."` tags on every field that should appear.

`SimpleFunction` does **not** expose `WithCacheTTL`; if you need caching, upgrade to `Function`.

### 5.2 `Function[In, Out]` — with event + global state

The handler signature exposes `*types.EventMessage` (workflow/run/node ids) and `*state.GlobalState` (RPC client, gRPC cache, OAuth client). This is what you need for OAuth, cross-function calls via RPC, or per-function cache control.

```go
import (
    "github.com/dibbla-agents/sdk-go"
    "github.com/dibbla-agents/sdk-go/internal/state"
    "github.com/dibbla-agents/sdk-go/internal/types"
)

server.RegisterFunction(
    sdk.NewFunction[Input, Output]("fancy_fn", "1.0.0", "...").
        WithHandler(func(in Input, event *types.EventMessage, gs *state.GlobalState) (Output, error) {
            // event.Workflow, event.Node, event.Run available
            // gs.RpcClient, gs.GrpcCache, gs.OAuth available
            return out, nil
        }).
        WithCacheTTL(5 * time.Minute). // per-function result cache; 0 disables
        WithTags("advanced"),
)
```

**Footgun: `Function[In, Out]` requires importing `internal/state` and `internal/types`.** Go's `internal/` rule blocks those imports from any module other than `sdk-go` itself. In practice this means `Function[In, Out]` is only usable when your worker code lives **inside** the `sdk-go` repo (e.g. as a file under `cmd/worker/examples/`), or in a fork/vendor of it. External modules are restricted to `SimpleFunction` — if you need OAuth or RPC from outside the SDK module, contribute the function to sdk-go's `cmd/worker/examples/` directory instead of trying to add it from your own module.

## 6. Jobs

Long-running work that needs progress reporting, structured task tracking, and dashboard visibility. Triggered from a workflow `job_trigger` node and run asynchronously — each trigger spawns a goroutine on the worker and streams events back over gRPC.

The `JobHost` abstraction was **removed** (see commit `0f2b190`). Register jobs directly on the server. Any tutorial or snippet that calls `server.NewJobHost(...)` or `jobs.NewJobHost(...)` is for an older SDK and will not compile.

### 6.1 The `JobHandler` interface

Implement four methods:

```go
import "github.com/dibbla-agents/sdk-go/jobs"

type DataProcessingJob struct{}

func (j *DataProcessingJob) GetJobID() string   { return "data_processing" }
func (j *DataProcessingJob) GetJobName() string { return "Data Processing Pipeline" }

func (j *DataProcessingJob) GetParameters() []jobs.JobParameter {
    return []jobs.JobParameter{
        {Name: "source",      Type: "string",  Required: true},
        {Name: "destination", Type: "string",  Required: true},
        {Name: "batch_size",  Type: "integer", Required: false, Default: 100},
        {Name: "dry_run",     Type: "boolean", Required: false, Default: false},
    }
}

func (j *DataProcessingJob) Execute(ctx *jobs.JobContext) error {
    src := ctx.GetStringArg("source", "")
    dst := ctx.GetStringArg("destination", "")
    batch := ctx.GetIntArg("batch_size", 100)
    dry := ctx.GetBoolArg("dry_run", false)

    ctx.Logger.Info(fmt.Sprintf("processing %s -> %s (batch=%d, dry=%v)", src, dst, batch, dry))

    ctx.Logger.TaskStarted("fetch")
    for i := 1; i <= 5; i++ {
        ctx.Logger.Progress(i, 5, fmt.Sprintf("file %d/5", i))
    }
    ctx.Logger.CompleteProgress()
    ctx.Logger.TaskCompleted()

    return nil // returning an error sends job_failed automatically
}
```

`GetParameters()` is what makes the dashboard render an input form — declare every argument up front, with `Type` as one of `string` / `integer` / `boolean` / `number`.

### 6.2 Registration

```go
server.RegisterJob(&DataProcessingJob{})
// ...register others...
server.Start() // blocks; jobs are advertised in the registration broadcast
```

### 6.3 `JobContext`

| Field / method | Purpose |
|---|---|
| `RunID, JobID, JobName` | Identifiers for this run. `RunID` is also the workflow run id. |
| `Args map[string]interface{}` | Raw arguments from the trigger payload. |
| `GetStringArg(name, default)` | Type-safe accessor; falls back to `default` on missing/wrong type. |
| `GetIntArg`, `GetBoolArg`, `GetFloat64Arg` | Same shape, other types. JSON numbers arrive as `float64`; the int helper converts. |
| `Logger` | See §7. |

Returning `nil` from `Execute` sends a `job_completed` event. Returning an error sends `job_failed` with the error string in `meta.error` — wrap with `fmt.Errorf("phase failed: %w", err)` so the dashboard message is useful.

## 7. Logger API

Every job has a `*jobs.Logger` on its context. Each call sends a gRPC event (visible in the dashboard) **and** writes a timestamped line to stdout (visible in `dibbla logs <app>`).

```go
// Levels
ctx.Logger.Info("loaded config")
ctx.Logger.Warn("rate limit at 80%")
ctx.Logger.Error("retry budget exhausted") // does NOT fail the job — return an error for that

// Task lifecycle (a task is a labelled phase within a job)
ctx.Logger.TaskStarted("fetch")
ctx.Logger.TaskCompleted()           // pairs with the most-recent TaskStarted
ctx.Logger.TaskFailed(err)
ctx.Logger.TaskSkipped("validation disabled")

// Progress (within a task)
ctx.Logger.Progress(current, total, "msg")  // determinate; renders a bar in console
ctx.Logger.ProgressIndeterminate(seen, "scanning")
ctx.Logger.CompleteProgress()               // always call before next TaskStarted

// Scoped logger
taskLog := ctx.Logger.WithTask("export")
taskLog.Info("...")
ctx.Logger.WithWriter(myBuf) // redirect console output (gRPC stream is unaffected)
```

`Error("...")` is a log line, not a job failure. To fail the job, **return** an error from `Execute`.

## 8. Authentication & TLS

The defaults work for production: `grpc.dibbla.com:443` with TLS, system CA bundle, `SERVER_API_TOKEN` for auth.

For local development against a workflow server on your laptop:

```go
server, _ := sdk.New(
    sdk.WithServerName("dev-worker"),
    sdk.WithGrpcServerAddress("localhost:50051"), // auto-detected as no-TLS
)
```

Override TLS explicitly when neither default fits:

```go
sdk.WithGrpcTLS(true)              // force TLS on a non-standard host
// or
GRPC_USE_TLS=false ./my-worker     // env var wins over auto-detect
```

`CODEX_DEBUG=true` enables verbose gRPC logging.

## 9. OAuth on behalf of the workflow user

Available via `gs.OAuth` on advanced `Function[In, Out]` handlers (see the §5.2 footgun about `internal/` imports).

```go
import (
    "github.com/dibbla-agents/sdk-go/internal/oauth"
    "github.com/dibbla-agents/sdk-go/internal/state"
    "github.com/dibbla-agents/sdk-go/internal/types"
)

func GetGoogleTokenFunction() sdk.FunctionBuilder {
    return sdk.NewFunction[Input, Output]("get_google_token", "1.0.0", "...").
        WithHandler(func(in Input, event *types.EventMessage, gs *state.GlobalState) (Output, error) {
            if gs.OAuth == nil {
                return Output{}, fmt.Errorf("OAuth client not available")
            }
            ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
            defer cancel()

            tok, err := gs.OAuth.GetAccessToken(ctx, oauth.ProviderGoogle, event.Run)
            if err != nil {
                return Output{}, fmt.Errorf("get token: %w", err)
            }
            // tok.AccessToken / tok.TokenType / tok.ExpiresAt / tok.Provider
            return Output{Token: tok.AccessToken}, nil
        })
}
```

- Providers: `oauth.ProviderGoogle`, `oauth.ProviderMicrosoft`, `oauth.ProviderGitHub`.
- `event.Run` is required — OAuth is scoped to the workflow run's authenticated user. This is why OAuth is impossible from `SimpleFunction` (no event handle).
- Capability discovery: `gs.OAuth.GetConnectedProviders(ctx, event.Run)` returns `map[string]*ProviderStatus` with `Email`, `LastUsed`, `Scopes`.
- Errors: assert `*oauth.OAuthError` with `Code` `"not_connected"` or `"token_expired"` to surface a useful dashboard message.

To request additional Google scopes (Drive, Calendar, Sheets, Gmail) at deploy time, see the `--google-scopes` flag in [reference.md](reference.md) and the auth-header / scope brokering section of [platform.md](platform.md).

## 10. Gotchas

- **`Start()` blocks forever.** Register every function and job before calling it.
- **`JobHost` is gone.** Use `server.RegisterJob(handler)` directly. `server.NewJobHost(...)` no longer exists.
- **Don't ship `codex-go-worker` as your real `SERVER_NAME`.** It's the default; two workers using it race for the same registry slot.
- **Function key = `(server_name, function_name, version)`.** Renaming any of those is a breaking change for every workflow that references the function — bump `version` and keep the old name registered for a transition window if you need to migrate.
- **`SimpleFunction` has no `WithCacheTTL`.** Caching is opt-in on `Function[In, Out]` only.
- **Result cache keys on input bytes.** Non-deterministic functions (time, randomness, external calls) need `WithCacheTTL(0)` or a cache-busting input field.
- **`internal/` packages can't be imported from outside the `sdk-go` module.** This restricts external users to `SimpleFunction`. For OAuth/RPC, contribute to `sdk-go/cmd/worker/examples/` rather than reaching for advanced functions in your own module.
- **`Logger.Error()` does not fail the job.** It's a log line. Return an error from `Execute` to fail.
- **Always call `CompleteProgress()` before the next `TaskStarted()`.** The progress bar is per-task and lingers otherwise.

## 11. Reference paths inside `sdk-go`

| Path | What it has |
|---|---|
| `sdk.go` | `Server` type, `New`, `RegisterFunction`, `RegisterJob`, `Init`, `Start`, job dispatch internals. |
| `config.go` | All `WithXxx` options, env var defaults. |
| `function.go` | `Function[In, Out]` and `SimpleFunction[In, Out]` builders. |
| `jobs/types.go` | `JobHandler`, `JobParameter`, `JobStatus` constants, `JobEventMeta`. |
| `jobs/context.go` | `JobContext`, `GetStringArg` / `GetIntArg` / `GetBoolArg` / `GetFloat64Arg`. |
| `jobs/logger.go` | `Logger` API: levels, task lifecycle, progress, `WithTask`, `WithWriter`. |
| `jobs/examples/simple_job.go` | Smallest runnable job. |
| `jobs/examples/data_processing_job.go` | Multi-phase job with progress, warnings, dry-run handling. |
| `cmd/worker/main.go` | Runnable worker registering several example functions. |
| `cmd/worker/examples/oauth_example.go` | Canonical OAuth function (Google token + connected-providers). |
| `cmd/worker/examples/google_sheets_example.go` | Calling a third-party API with the OAuth token. |
| `README.md` | Long-form overview; section 8+ (auth, OAuth, TLS) is current. |

## 12. End-to-end deploy

1. Write your worker (`main.go`) using the patterns above.
2. Author a `Dockerfile` that builds the binary and runs it. Pass `SERVER_NAME` and `SERVER_API_TOKEN` via env.
3. `dibbla deploy . --alias my-worker -m "feat: register summarize_doc fn" -e SERVER_API_TOKEN=...` — same flow as any Dibbla app, see [platform.md](platform.md).
4. After the worker comes up, verify it registered: `dibbla functions list` (should show your `(server, function)` pair) and check `dibbla logs my-worker` for the "Registered N functions" / "Registered N jobs" lines.
5. Reference the function from a workflow YAML by `(server, function)` — see [workflows.md](workflows.md) for the `function` node shape and the agent+tools wiring pattern.
