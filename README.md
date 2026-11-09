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
- Cookie jar per workspace, kept outside git, with a viewer to inspect and delete cookies
- OAuth 2.0: client credentials and authorization code with PKCE, tokens fetched, cached and refreshed automatically
- Client settings per workspace: timeout, redirects, HTTP or SOCKS proxy, custom CA, client certificates (mutual TLS) and an insecure mode for self-signed servers
- Native desktop GUI built with [egui](https://github.com/emilk/egui)

## Building

```sh
cargo run -p cornu-gui --release
```

On Linux you may need the usual windowing dev packages (X11/Wayland, `libxkbcommon`).

## Workspace layout

| Crate | Purpose |
|-------|---------|
| `cornu-core` | Request model, variable interpolation, HTTP execution (no UI dependency) |
| `cornu-storage` | Git-friendly workspace format: one TOML file per request, secrets kept in `.cornu/` |
| `cornu-import` | Importers: Postman v2.0 / v2.1 collections and environments, OpenAPI 3 / Swagger 2 (JSON or YAML, found from a base URL), cURL (parse and export) |
| `cornu-gui` | Desktop application (`cornu` binary) |

## License

cornu is licensed under the [GNU General Public License v3.0 or later](LICENSE).
