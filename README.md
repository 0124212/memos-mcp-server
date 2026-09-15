# memos-mcp-server

[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![Memos](https://img.shields.io/badge/memos-0.30%20%2F%200.32-blue?style=flat-square)](https://github.com/usememos/memos)
[![MCP](https://img.shields.io/badge/MCP-streamable--http%20%2B%20stdio-green?style=flat-square)](https://modelcontextprotocol.io)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)
[![Docker](https://img.shields.io/badge/docker-ready-blue?style=flat-square&logo=docker)](Dockerfile)

Standalone Rust MCP server for [Memos](https://github.com/usememos/memos) — **full parity with the official Go `server/router/mcp` (20 tools at `v0.30.0` / forthcoming `0.32`) plus a small superset**: 2 tag helpers, 4 prompts, and `memo://` resources. Runs as a separate process against any Memos instance so you don't have to patch Memos.

Built on [`rmcp` 3.3.0](https://github.com/modelcontextprotocol/rust-sdk) (`modelcontextprotocol/rust-sdk`, not `rustify-community/rmcp`) + `axum` + `reqwest`.

## What you get

- **22 tools**: the 20 curated official tools 1:1 plus `tag_list_tags` / `tag_rename_tag` ported from `chriscurrycc/memos-mcp`. Tool names, paths, and `readOnly` flags mirror `server/router/mcp/catalog.go`.
- **Auth parity**: per-request `Authorization: Bearer` passthrough (the official in-process server authenticates as the caller) with `MEMOS_TOKEN`/`MEMOS_ACCESS_TOKEN` as fallback. `X-MCP-*` and route aliases narrow the catalog per request without affecting auth.
- **Extra**: 4 prompts (`capture`, `digest`, `tag_overview`, `relation_graph`) and `memo://memos/{uid}` + `memo://prompts/*` resources. The official package is tools-only by design.
- **Hardened**: origin guard from `origin.go`, 256 MiB body cap (`maxMCPRequestBytes`), `rmcp` 3.3.0 streamable HTTP (stateless, JSON).

## Tools

22 total — 20 official plus 2 tag helpers:

| Toolset | Tools |
|---------|-------|
| `memos` | `memo_list_memos`, `memo_create_memo` (`memo_id`, `pinned`, `visibility`, `create_time`), `memo_get_memo`, `memo_update_memo` (`content`/`visibility`/`pinned`/`state` via `?updateMask=`), `memo_delete_memo`, `memo_list_memo_comments`, `memo_create_memo_comment` (`comment_id`), `shortcut_list_shortcuts`, `auth_get_current_user` |
| `attachments` | `memo_list_memo_attachments`, `memo_set_memo_attachments`, `attachment_list_attachments`, `attachment_create_attachment` (`filename`+`type`+`content`/`externalLink`), `attachment_get_attachment`, `attachment_delete_attachment` |
| `reactions` | `memo_list_memo_reactions`, `memo_upsert_memo_reaction` (`reaction.contentId = memos/{uid}` required since 0.30), `memo_delete_memo_reaction` |
| `relations` | `memo_list_memo_relations`, `memo_set_memo_relations` (`{name, relations: REFERENCE}`) |
| `tags` | `tag_list_tags` (counts, `parent`/`recursive`), `tag_rename_tag` (global content rewrite, child segments preserved) |

Prompts: `capture`, `digest`, `tag_overview`, `relation_graph` (port of `chriscurrycc/memos-mcp`; `review`/`on_this_day` omitted — no `/api/v1/review/*` backing). Resources: `memo://memos/{uid}` (frontmatter `uid`/`visibility`/`pinned`/`tags` + markdown) and `memo://prompts/{name}`.

## Transports

- **Streamable HTTP (default, stateless, JSON)** on `BIND` (default `127.0.0.1:8080`, bare `8080` → `0.0.0.0:8080`):
  - `POST/GET/DELETE /mcp` — full catalog
  - `POST/GET/DELETE /mcp/readonly` — 11 read-only tools; writes return `Invalid params`
  - `POST/GET/DELETE /mcp/x/{toolsets}` and `/mcp/x/{toolsets}/readonly` — per-route toolset filter
  - Per-request headers `X-MCP-Readonly`, `X-MCP-Toolsets`, `X-MCP-Tools`, `X-MCP-Exclude-Tools` further narrow the catalog (stateless: each request builds a freshly filtered service and forwards to `StreamableHttpService::handle`).
- **stdio**: `--stdio` or `MEMOS_MCP_STDIO=1`, for Claude Code / `opencode` local MCP.

Origin guard ported from `server/router/mcp/origin.go` — rejects foreign `Origin` (403) unless `Host` matches or `MEMOS_INSTANCE_URL` host matches. Request bodies capped at 256 MiB (same as `server/router/api/v1.MaxAPIRequestBytes`); Axum would otherwise 413 attachment uploads.

## Parity notes (what was closed)

Against `server/router/mcp` at `v0.30.0` (commit `2036c1f`, `ShortcutService_ListShortcuts`) and `main` (`751005b`):

- `memoId` / `commentId` query params forwarded on create (`memo_create_memo`, `memo_create_memo_comment`). Missing before → client-assigned UIDs now work.
- `pinned` accepted on `memo_create_memo` (forwarded; see behavior note below).
- `reaction.contentId = memos/{uid}` required since 0.30 — already handled.
- `SetMemoAttachments` / `SetMemoRelations` send `{name, attachments|relations}` — already handled; `type: "REFERENCE"` only (API rejects `COMMENT` there).
- Tag rename is a client-side `content` rewrite (no tag API exists) — fixed: previous regex used look-ahead (`(?![\w\-/])`) unsupported by the `regex` crate; now captured boundary `([^\w\-/]|$)` re-emitted as `$2`, child segments like `#a/b` preserved.
- Per-request `Authorization` passthrough added; previously only env PAT was used. Malformed `Authorization` (e.g. `Basic …` or `Bearer ` with no token) correctly falls back to env PAT; a present `Bearer <token>` always wins even if invalid (returns 401 rather than silently succeeding as another user).
- Body cap 256 MiB added. `BIND` env already supported bare `8080`.

## Behavior notes

- **Tags**: Memos has no tag API. `tag_list_tags` aggregates `tags[]` from `ListMemos` (paged, 1000/page) and `tag_rename_tag` rewrites `content` across matched memos (CEL `tag in ["name"]`). Destructive — use with care. Regex is boundary-aware and preserves `/child` segments.
- **Pinned on create**: `POST /api/v1/memos` currently ignores `pinned` server-side (create returns `pinned:false` even when sent); `PATCH /api/v1/memos/{id}?updateMask=pinned` works. The tool forwards `pinned` on create for parity, but set pin with `memo_update_memo` if needed.
- **Auth**: PATs are `memos_pat_*` and ride standard `Authorization: Bearer`. The server sniffs the `memos_pat_` prefix (`server/auth/authenticator.go:150`); no special header needed. `/api/v1/auth/signin` with password does not work on 0.30 — use PATs.
- **Filters**: `ListMemos` CEL uses `tag in ["name"]`, `content.contains("…")`, `created_ts`/`updated_ts` (filter) vs `create_time`/`update_time` (`orderBy`). All verified live.

## Config

| Var | Default | Notes |
|-----|---------|-------|
| `MEMOS_URL` | `http://localhost:5230` | Memos base URL |
| `MEMOS_TOKEN` / `MEMOS_ACCESS_TOKEN` | (empty) | Fallback PAT (`memos_pat_*`); per-request `Authorization: Bearer` wins when present |
| `BIND` | `127.0.0.1:8080` | `host:port` or bare port `8080` (`0.0.0.0:8080`) |
| `MEMOS_INSTANCE_URL` | (empty) | Allowed `Origin` host for browser clients |
| `RUST_LOG` | `info` | `tracing` filter |

## Run

```bash
# from source
cargo run -- --stdio
# or
MEMOS_URL=https://memos.junilab.xyz MEMOS_TOKEN=memos_pat_xxx BIND=127.0.0.1:8080 cargo run

# install binary
cargo install --git https://github.com/0124212/memos-mcp-server
memos-mcp-server --stdio
# or: cargo install --path . (local clone)
```

### Docker (256MB, `debian:bookworm-slim`, `rust:1.88-bookworm` builder)

```bash
docker build -t memos-mcp-server .
docker run --rm -p 8080:8080 -e MEMOS_URL=https://memos.junilab.xyz -e MEMOS_TOKEN=memos_pat_xxx memos-mcp-server
# compose — add to your memos stack:
#   memos-mcp:
#     build: https://github.com/0124212/memos-mcp-server.git
#     image: memos-mcp-server:main
#     environment: { MEMOS_URL: https://memos.junilab.xyz, MEMOS_TOKEN: ${MEMOS_ADMIN_PAT}, BIND: 0.0.0.0:8080 }
#     ports: ["127.0.0.1:8081:8080"]
```

### opencode

Keep the official `memos` entry (in-process, always available) and add this one alongside it:

```json
"memos": {
  "type": "remote",
  "url": "https://memos.junilab.xyz/mcp",
  "enabled": true,
  "headers": { "Authorization": "Bearer {env:MEMOS_ADMIN_PAT}" }
},
"memos-rust": {
  "type": "local",
  "command": ["/root/memos-mcp-study/memos-mcp/target/release/memos-mcp-server", "--stdio"],
  "enabled": true,
  "environment": { "MEMOS_URL": "https://memos.junilab.xyz", "MEMOS_TOKEN": "{env:MEMOS_ADMIN_PAT}" }
}
```

Stdio is recommended for local use (no port to manage, per-session PAT). Remote HTTP (`http://127.0.0.1:8080/mcp`) also works once the binary is running — see `BIND` above.

## When to use this vs official

Memos already ships an official in-process MCP at `/mcp` (`server/router/mcp` — `https://memos.junilab.xyz/mcp`). Use this Rust server when you want a standalone binary for any Memos instance (local dev, remote host, CI), the tag helpers, prompts/resources, or per-route `X-MCP-*` filtering without patching Memos. Otherwise the official is the zero-ops default.

## Study

Built from a study of 9 community Memos MCPs vs the official Go package (`server/router/mcp/catalog.go` + `proto/gen/openapi.yaml` at `v0.30.0`). The 2 `tags` tools are ported from `chriscurrycc/memos-mcp` (Memos has no tag API — tags are aggregated client-side and rename is a `content` rewrite). Verified live against `memos-test` (0.30.0) and `memos.junilab.xyz`; 20/20 unit tests + `clippy` clean.

## License

MIT — same as Memos.
