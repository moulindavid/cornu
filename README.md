# cornu

A fast, local-first API client written in Rust. No account, no cloud sync, just a single binary
and plain text files you can commit to git.

> *cornu* is Latin for "horn", a nod to the post horn that announced the mail coach.

**Status:** early development, not usable yet.

## Goals

- HTTP(S), GraphQL, and later WebSocket, SSE and gRPC
- Collections stored as git-friendly TOML files, secrets kept out of git
- Environments with variables, auth inherited from folders (set a Bearer token once)
- Import from Postman collections and environments, OpenAPI / Swagger and cURL
- Data-driven runs: replace a variable by a list of values or a CSV file (with or without header)
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
