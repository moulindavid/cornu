# cornu

A fast, local-first API client written in Rust. No account, no cloud sync, just a single binary
and plain text files you can commit to git.

> *cornu* is Latin for "horn", a nod to the post horn that announced the mail coach.

**Status:** early development, not usable yet.

## Goals

- HTTP(S), GraphQL (query and variables, schema explorer, field autocompletion), WebSocket (message log and composer), server-sent events (shown live when a response is `text/event-stream`) and gRPC (methods found through server reflection, JSON messages, unary and streaming calls)
- Collections stored as git-friendly TOML files, secrets kept out of git
- Environments with variables, auth inherited from folders (set a Bearer token once)
- Import from Postman collections and environments, OpenAPI / Swagger and cURL
- Data-driven runs: replace a variable by a list of values or a CSV file (with or without header)
- Request chaining: capture response values (JSONPath, headers, status) into variables
- Response tests: assert on status, headers, JSONPath values, time and size
- Pre-request and post-response scripts in [Rhai](https://rhai.rs), on requests and folders: change the request, sign it (HMAC, SHA-256), set variables, add tests
- Cookie jar per workspace, kept outside git, with a viewer to inspect and delete cookies
- OAuth 2.0: client credentials and authorization code with PKCE, tokens fetched, cached and refreshed automatically
- Client settings per workspace: timeout, redirects, HTTP or SOCKS proxy, custom CA, client certificates (mutual TLS) and an insecure mode for self-signed servers
- Native desktop GUI built with [egui](https://github.com/emilk/egui), and a headless CLI for CI, scripts and agents

## Building

```sh
cargo run -p cornu-gui --release
```

On Linux you may need the usual windowing dev packages (X11/Wayland, `libxkbcommon`).

## Command line

`cornu-cli` runs the requests of a workspace without the app, with the same variables, auth,
captures, tests, scripts, cookies and client settings. It finds the workspace from the current folder up,
or takes `--workspace`.

```sh
cargo install --path crates/cornu-cli

cornu-cli list                                   # every request, by path
cornu-cli send users/get --env dev               # one request, with its full response
cornu-cli run users --env dev --var id=7         # a folder, then a summary
cornu-cli run --env ci --data users.csv --bail   # the whole workspace, once per CSV row
cornu-cli run --env ci --junit report.xml        # also a JUnit report for CI
cornu-cli envs                                   # environments, secrets hidden
```

`--json` prints a stable JSON document instead of text, for scripts and agents: the resolved
request (secret values hidden unless `--show-secrets`), the response, captures, test results and
what scripts printed.
The exit code is `0` when every request passed, `1` when one failed or could not be sent, and `2`
for a usage or workspace error. The CLI never prompts: an OAuth 2.0 authorization code token has
to be fetched once from the app, client credentials tokens are fetched on their own.

## Workspace layout

| Crate | Purpose |
|-------|---------|
| `cornu-core` | Request model, variable interpolation, HTTP execution (no UI dependency) |
| `cornu-storage` | Git-friendly workspace format: one TOML file per request, secrets kept in `.cornu/` |
| `cornu-import` | Importers: Postman v2.0 / v2.1 collections and environments, OpenAPI 3 / Swagger 2 (JSON or YAML, found from a base URL), cURL (parse and export) |
| `cornu-gui` | Desktop application (`cornu` binary) |
| `cornu-cli` | Headless runner (`cornu-cli` binary) for CI, scripts and agents |

## License

cornu is licensed under the [GNU General Public License v3.0 or later](LICENSE).
