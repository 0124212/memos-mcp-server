# memos-mcp-server

Standalone Rust MCP server for [Memos](https://github.com/usememos/memos) — speaks the same 20 curated tools as the official Go `server/router/mcp` package, but runs as a separate HTTP service over any Memos instance.

Built on [`rmcp` 3.3.0](https://github.com/modelcontextprotocol/rust-sdk) (`modelcontextprotocol/rust-sdk`, not `rustify-community/rmcp`) + `axum` + `reqwest`.

## Tools

22 tools total (20 official 1:1 + 2 tag helpers ported from `chriscurrycc/memos-mcp`):

| Toolset | Tools |
|---------|-------|
| `memos` | `memo_list_memos`, `memo_create_memo`, `memo_get_memo`, `memo_update_memo`, `memo_delete_memo`, `memo_list_memo_comments`, `memo_create_memo_comment`, `shortcut_list_shortcuts`, `auth_get_current_user` |
| `attachments` | `memo_list_memo_attachments`, `memo_set_memo_attachments`, `attachment_list_attachments`, `attachment_create_attachment`, `attachment_get_attachment`, `attachment_delete_attachment` |
| `reactions` | `memo_list_memo_reactions`, `memo_upsert_memo_reaction`, `memo_delete_memo_reaction` |
| `relations` | `memo_list_memo_relations`, `memo_set_memo_relations` |
| `tags` | `tag_list_tags`, `tag_rename_tag` |

Plus 4 prompts (`capture`, `digest`, `tag_overview`, `relation_graph`) and 2 resource templates (`memo://memos/{uid}`, `memo://prompts/{name}`) — extensions beyond the official tools-only package.

Tool names, REST mappings, and readonly flags mirror `server/router/mcp/catalog.go` at Memos `v0.30.0` (verified live against `0.30.0` with `bytes` field quirks patched).

## Transports

- **Streamable HTTP (default, stateless, JSON)** on `BIND` (default `127.0.0.1:8080`):
  - `POST/GET/DELETE /mcp` — full catalog
  - `POST/GET/DELETE /mcp/readonly` — 10 write tools rejected
  - `POST/GET/DELETE /mcp/x/{toolsets}` and `/mcp/x/{toolsets}/readonly` — per-route toolset filter
  - Per-request headers `X-MCP-Readonly`, `X-MCP-Toolsets`, `X-MCP-Tools`, `X-MCP-Exclude-Tools` further narrow the catalog (stateless: each request builds a freshly filtered service).
- **stdio**: `--stdio` flag or `MEMOS_MCP_STDIO=1`, for Claude Code / `opencode` local MCP.

Origin guard ported from `server/router/mcp/origin.go` — rejects foreign `Origin` (403) unless `Host` matches or `MEMOS_INSTANCE_URL` host matches.

## Config

Env:

| Var | Default | Notes |
|-----|---------|-------|
| `MEMOS_URL` | `http://localhost:5230` | Memos base URL |
| `MEMOS_TOKEN` / `MEMOS_ACCESS_TOKEN` | (empty) | Bearer PAT, `memos_pat_*` |
| `BIND` | `127.0.0.1:8080` | `host:port` or bare port `8080` (`0.0.0.0:8080`) |
| `MEMOS_INSTANCE_URL` | (empty) | Allowed `Origin` host for browser clients |
| `RUST_LOG` | `info` | `tracing` filter |

## Run

```bash
cargo run -- --stdio
# or
MEMOS_URL=https://memos.junilab.xyz MEMOS_TOKEN=memos_pat_xxx BIND=127.0.0.1:8080 cargo run
# check
curl -H "Authorization: Bearer $MEMOS_TOKEN" http://127.0.0.1:8080/mcp # 405 on GET is ok, POST tools/list via MCP client
```

### Docker

```bash
docker build -t memos-mcp-server .
docker run --rm -p 8080:8080 -e MEMOS_URL=https://memos.junilab.xyz -e MEMOS_TOKEN=memos_pat_xxx memos-mcp-server
# or compose (ak)
# see docker-compose.yml fragment in this repo
```

### opencode local MCP

In `opencode.json` `mcp`:

```json
"memos-rust": {
  "type": "local",
  "command": ["/path/to/memos-mcp-server", "--stdio"],
  "enabled": true,
  "environment": { "MEMOS_URL": "https://memos.junilab.xyz", "MEMOS_TOKEN": "{env:MEMOS_ADMIN_PAT}" }
}
```

Or remote (HTTP):

```json
"memos-rust-remote": {
  "type": "remote",
  "url": "http://127.0.0.1:8080/mcp",
  "enabled": true
}
```

## When to use this vs official

Memos already ships an official in-process MCP at `/mcp` (`server/router/mcp` — the one `https://memos.junilab.xyz/mcp` exposes, which `opencode` uses by default). Use this Rust server when you need: a standalone binary for any Memos instance (local dev, remote host), the extra `tags` helpers, prompts (`capture`/`digest`/`tag_overview`/`relation_graph`), `memo://` resources, or per-route `X-MCP-*` filtering (`/mcp/readonly`, `/mcp/x/{toolsets}`) without patching Memos.

## Study

Built from a study of 9 community Memos MCPs vs the official Go package at Memos `v0.30.0` (`server/router/mcp/catalog.go` + `proto/gen/openapi.yaml`). The 2 `tags` tools are ported from `chriscurrycc/memos-mcp` (Memos has no tag API — tags are aggregated client-side from `ListMemos` and rename is a `content` rewrite). Verified live against Memos `0.30.0`.

## License

MIT — same as Memos.
