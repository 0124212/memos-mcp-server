//! Memos MCP server: 20 official curated tools + tags + prompts + resources.
//!
//! Tool names, REST mappings and readonly flags mirror the official Go
//! package (`server/router/mcp/catalog.go` at v0.30.0). All request shapes
//! were verified live against Memos 0.30.0:
//! - `UpdateMemo` requires `?updateMask=` with proto field names; memo state
//!   is `state` (`NORMAL`/`ARCHIVED`)
//! - `UpsertMemoReaction` requires `reaction.contentId = memos/{uid}`
//!   (v0.30 added required `Reaction.content_id`)
//! - `SetMemoAttachments`/`SetMemoRelations` take `{name, attachments|relations}`
//! - `Attachment` uses field `type` (not `mimeType`) for the MIME type
//! - tag search CEL is `tag in ["name"]`
//!
//! Tool failures return `CallToolResult::error` (user-visible) rather than
//! protocol errors, per the `ServerHandler::call_tool` contract.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{
        prompt::PromptContext,
        router::prompt::PromptRouter,
        tool::{ToolCallContext, ToolRouter},
        wrapper::Parameters,
    },
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
        GetPromptResponse, InitializeResult, ListPromptsResult, ListResourcesResult,
        ListResourceTemplatesResult,         ListToolsResult, PaginatedRequestParams, PromptMessage, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
        Role, ServerCapabilities,
    },
    prompt, prompt_handler, prompt_router,
    service::RequestContext,
    tool, tool_handler, tool_router,
    RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::memos::{MemosClient, bare_id, opt_query};

// ---------- helpers ----------

fn api_fail(e: anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("memos api error: {e:#}"))])
}

fn ok_json(v: Value) -> CallToolResult {
    CallToolResult::structured(v)
}

// ---------- tool param structs ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListMemosParams {
    pub page_size: Option<i32>,
    pub page_token: Option<String>,
    pub state: Option<String>,
    pub order_by: Option<String>,
    pub filter: Option<String>,
    pub show_deleted: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMemoParams {
    /// Markdown content (required).
    pub content: String,
    pub visibility: Option<String>,
    pub create_time: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoRef {
    /// `memos/UID` or bare UID.
    pub memo: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateMemoParams {
    pub memo: String,
    pub content: Option<String>,
    pub visibility: Option<String>,
    pub pinned: Option<bool>,
    pub state: Option<String>,
    pub create_time: Option<String>,
    pub update_time: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoPageParams {
    pub memo: String,
    pub page_size: Option<i32>,
    pub page_token: Option<String>,
    pub order_by: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMemoCommentParams {
    pub memo: String,
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetMemoAttachmentsParams {
    pub memo: String,
    /// Attachment names (`attachments/UID`), bare UIDs, or full objects.
    pub attachments: Vec<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpsertMemoReactionParams {
    pub memo: String,
    /// Emoji / shortcode, e.g. `"👍"` or `"+1"`.
    pub reaction_type: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteMemoReactionParams {
    pub memo: String,
    /// Reaction id (last segment of `memos/{uid}/reactions/{id}`).
    pub reaction: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetMemoRelationsParams {
    pub memo: String,
    /// Memos to reference (`memos/UID` or bare UID). Replaces the set.
    pub related: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAttachmentsParams {
    pub page_size: Option<i32>,
    pub page_token: Option<String>,
    pub filter: Option<String>,
    pub order_by: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAttachmentParams {
    pub filename: String,
    /// MIME type; guessed from extension when omitted.
    pub mime_type: Option<String>,
    /// Base64-encoded content.
    pub content: Option<String>,
    /// Link to a memo (`memos/UID` or bare UID).
    pub memo: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AttachmentRef {
    pub attachment: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShortcutParams {
    /// `users/ID`, `users/username`, or bare segment.
    pub user: String,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct EmptyParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListTagsParams {
    pub parent: Option<String>,
    pub recursive: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenameTagParams {
    pub old_tag: String,
    pub new_tag: String,
}

// ---------- prompt arg structs ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureArgs {
    pub content: String,
    pub tags: Option<String>,
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DigestArgs {
    pub period: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RelationGraphArgs {
    pub memo: String,
}

// ---------- service ----------

#[derive(Clone)]
pub struct MemosServer {
    client: MemosClient,
}

impl MemosServer {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            client: MemosClient::new(base_url, token),
        }
    }

    async fn all_memos(&self, filter: Option<String>) -> Result<Vec<Value>, anyhow::Error> {
        let mut out = vec![];
        let mut page_token: Option<String> = None;
        loop {
            let q = opt_query(vec![
                ("pageSize", Some("1000".to_string())),
                ("pageToken", page_token.clone()),
                ("filter", filter.clone()),
            ]);
            let v = self.client.get("/api/v1/memos", q).await?;
            if let Some(arr) = v.get("memos").and_then(|m| m.as_array()) {
                out.extend(arr.iter().cloned());
            }
            page_token = v
                .get("nextPageToken")
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    // ----- resources (shared with FilteredServer) -----

    pub fn list_memo_resources(&self) -> Vec<Resource> {
        PROMPT_DEFS
            .iter()
            .map(|(name, desc, _)| {
                Resource::new(format!("memo://prompts/{name}"), format!("prompt-{name}"))
                    .with_description(format!("Prompt: {desc}"))
                    .with_mime_type("text/markdown")
            })
            .collect()
    }

    pub fn list_memo_resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new("memo://memos/{uid}", "memo")
                .with_description("A memo by its UID")
                .with_mime_type("text/markdown"),
        ]
    }

    pub async fn read_memo_resource(&self, uri: &str) -> Result<ReadResourceResponse, ErrorData> {
        if let Some(name) = uri.strip_prefix("memo://prompts/") {
            let text = PROMPT_DEFS
                .iter()
                .find(|(n, _, _)| *n == name)
                .map(|(_, _, t)| t.to_string())
                .ok_or_else(|| ErrorData::invalid_params(format!("unknown prompt: {name}"), None))?;
            return Ok(text_resource(uri, &text));
        }
        if let Some(uid) = uri.strip_prefix("memo://memos/") {
            let uid = uid.trim_matches('/');
            if uid.is_empty() || uid.contains('/') {
                return Err(ErrorData::invalid_params("invalid memo URI: missing UID", None));
            }
            let memo = self
                .client
                .get(&format!("/api/v1/memos/{uid}"), vec![])
                .await
                .map_err(|e| ErrorData::internal_error(format!("memos api error: {e:#}"), None))?;
            return Ok(text_resource(uri, &memo_markdown(&memo)));
        }
        Err(ErrorData::invalid_params(format!("unknown resource: {uri}"), None))
    }
}

fn text_resource(uri: &str, text: &str) -> ReadResourceResponse {
    ReadResourceResponse::Complete(ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type("text/markdown"),
    ]))
}

fn memo_markdown(memo: &Value) -> String {
    let get = |k: &str| memo.get(k).and_then(|v| v.as_str()).unwrap_or("");
    // The API returns `name: "memos/UID"` (no `uid` field) — derive it.
    let uid = bare_id(get("name"));
    let tags = memo
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut fm = vec!["---".to_string(), format!("uid: {uid}"), format!("visibility: {}", get("visibility"))];
    if memo.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false) {
        fm.push("pinned: true".to_string());
    }
    fm.push(format!("created: {}", get("createTime")));
    fm.push(format!("updated: {}", get("updateTime")));
    if !tags.is_empty() {
        fm.push(format!("tags: [{tags}]"));
    }
    fm.push("---".to_string());
    format!("{}\n\n{}", fm.join("\n"), get("content"))
}

/// Compiled matcher for `#old` plus any `/child` suffix segments.
///
/// `regex` supports no look-around, so the trailing boundary char (or
/// end-of-string) is captured and re-emitted by [`apply_tag_rename`] as `$2`
/// instead of asserting `(?![\w\-/])`.
fn tag_rename_regex(old: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!(
        r"#{}((?:/[\w\-]+)*)([^\w\-/]|$)",
        regex::escape(old)
    ))
}

fn apply_tag_rename(re: &Regex, content: &str, new: &str) -> String {
    re.replace_all(content, format!("#{new}$1$2")).to_string()
}

fn guess_mime(filename: &str) -> &str {
    match filename.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

// ---------- tools ----------

#[tool_router(vis = "pub")]
impl MemosServer {
    #[tool(description = "ListMemos lists readable non-comment memos with pagination and filter.")]
    async fn memo_list_memos(&self, Parameters(p): Parameters<ListMemosParams>) -> CallToolResult {
        let q = opt_query(vec![
            ("pageSize", p.page_size.map(|v| v.to_string())),
            ("pageToken", p.page_token),
            ("state", p.state),
            ("orderBy", p.order_by),
            ("filter", p.filter),
            ("showDeleted", p.show_deleted.map(|v| v.to_string())),
        ]);
        match self.client.get("/api/v1/memos", q).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "CreateMemo creates a memo. `content` is required.")]
    async fn memo_create_memo(&self, Parameters(p): Parameters<CreateMemoParams>) -> CallToolResult {
        let mut body = json!({ "content": p.content });
        if let Some(v) = p.visibility {
            body["visibility"] = json!(v);
        }
        if let Some(t) = p.create_time {
            body["createTime"] = json!(t);
        }
        match self.client.post("/api/v1/memos", body).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "GetMemo gets a memo by name (`memos/UID` or bare UID).")]
    async fn memo_get_memo(&self, Parameters(p): Parameters<MemoRef>) -> CallToolResult {
        let path = format!("/api/v1/memos/{}", bare_id(&p.memo));
        match self.client.get(&path, vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "UpdateMemo patches a memo. At least one field must be set.")]
    async fn memo_update_memo(&self, Parameters(p): Parameters<UpdateMemoParams>) -> CallToolResult {
        let mut body = json!({});
        let mut mask = vec![];
        if let Some(c) = p.content {
            body["content"] = json!(c);
            mask.push("content");
        }
        if let Some(v) = p.visibility {
            body["visibility"] = json!(v);
            mask.push("visibility");
        }
        if let Some(pinned) = p.pinned {
            body["pinned"] = json!(pinned);
            mask.push("pinned");
        }
        if let Some(s) = p.state {
            body["state"] = json!(s);
            mask.push("state");
        }
        if let Some(t) = p.create_time {
            body["createTime"] = json!(t);
            mask.push("create_time");
        }
        if let Some(t) = p.update_time {
            body["updateTime"] = json!(t);
            mask.push("update_time");
        }
        if mask.is_empty() {
            return CallToolResult::error(vec![ContentBlock::text(
                "at least one of content/visibility/pinned/state/create_time/update_time must be set",
            )]);
        }
        let path = format!("/api/v1/memos/{}", bare_id(&p.memo));
        let q = vec![("updateMask".to_string(), mask.join(","))];
        match self.client.patch_q(&path, q, body).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "DeleteMemo deletes a memo.")]
    async fn memo_delete_memo(&self, Parameters(p): Parameters<MemoRef>) -> CallToolResult {
        let path = format!("/api/v1/memos/{}", bare_id(&p.memo));
        match self.client.delete(&path).await {
            Ok(_) => ok_json(json!({ "ok": true })),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "ListMemoComments lists comments of a memo.")]
    async fn memo_list_memo_comments(
        &self,
        Parameters(p): Parameters<MemoPageParams>,
    ) -> CallToolResult {
        let path = format!("/api/v1/memos/{}/comments", bare_id(&p.memo));
        let q = opt_query(vec![
            ("pageSize", p.page_size.map(|v| v.to_string())),
            ("pageToken", p.page_token),
            ("orderBy", p.order_by),
        ]);
        match self.client.get(&path, q).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "CreateMemoComment creates a comment on a memo. `content` is required.")]
    async fn memo_create_memo_comment(
        &self,
        Parameters(p): Parameters<CreateMemoCommentParams>,
    ) -> CallToolResult {
        let path = format!("/api/v1/memos/{}/comments", bare_id(&p.memo));
        match self.client.post(&path, json!({ "content": p.content })).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "ListMemoAttachments lists attachments of a memo.")]
    async fn memo_list_memo_attachments(&self, Parameters(p): Parameters<MemoRef>) -> CallToolResult {
        let path = format!("/api/v1/memos/{}/attachments", bare_id(&p.memo));
        match self.client.get(&path, vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "SetMemoAttachments replaces the attachment set of a memo.")]
    async fn memo_set_memo_attachments(
        &self,
        Parameters(p): Parameters<SetMemoAttachmentsParams>,
    ) -> CallToolResult {
        let uid = bare_id(&p.memo);
        let attachments: Vec<Value> = p
            .attachments
            .into_iter()
            .map(|a| match a {
                Value::String(s) => json!({ "name": format!("attachments/{}", bare_id(&s)) }),
                other => other,
            })
            .collect();
        let path = format!("/api/v1/memos/{uid}/attachments");
        let body = json!({ "name": format!("memos/{uid}"), "attachments": attachments });
        match self.client.patch(&path, body).await {
            Ok(v) => ok_json(if v.is_null() { json!({ "ok": true }) } else { v }),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "ListMemoReactions lists reactions of a memo.")]
    async fn memo_list_memo_reactions(&self, Parameters(p): Parameters<MemoRef>) -> CallToolResult {
        let path = format!("/api/v1/memos/{}/reactions", bare_id(&p.memo));
        match self.client.get(&path, vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "UpsertMemoReaction upserts a reaction (emoji) on a memo.")]
    async fn memo_upsert_memo_reaction(
        &self,
        Parameters(p): Parameters<UpsertMemoReactionParams>,
    ) -> CallToolResult {
        let uid = bare_id(&p.memo);
        let path = format!("/api/v1/memos/{uid}/reactions");
        let body = json!({
            "reaction": {
                "contentId": format!("memos/{uid}"),
                "reactionType": p.reaction_type,
            }
        });
        match self.client.post(&path, body).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "DeleteMemoReaction deletes a reaction from a memo.")]
    async fn memo_delete_memo_reaction(
        &self,
        Parameters(p): Parameters<DeleteMemoReactionParams>,
    ) -> CallToolResult {
        let path = format!(
            "/api/v1/memos/{}/reactions/{}",
            bare_id(&p.memo),
            bare_id(&p.reaction)
        );
        match self.client.delete(&path).await {
            Ok(_) => ok_json(json!({ "ok": true })),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "ListMemoRelations lists relations of a memo.")]
    async fn memo_list_memo_relations(&self, Parameters(p): Parameters<MemoRef>) -> CallToolResult {
        let path = format!("/api/v1/memos/{}/relations", bare_id(&p.memo));
        match self.client.get(&path, vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "SetMemoRelations replaces the REFERENCE relation set of a memo.")]
    async fn memo_set_memo_relations(
        &self,
        Parameters(p): Parameters<SetMemoRelationsParams>,
    ) -> CallToolResult {
        let uid = bare_id(&p.memo);
        let relations: Vec<Value> = p
            .related
            .into_iter()
            .map(|t| {
                let tid = bare_id(&t);
                json!({
                    "memo": { "name": format!("memos/{uid}") },
                    "relatedMemo": { "name": format!("memos/{tid}") },
                    "type": "REFERENCE",
                })
            })
            .collect();
        let path = format!("/api/v1/memos/{uid}/relations");
        let body = json!({ "name": format!("memos/{uid}"), "relations": relations });
        match self.client.patch(&path, body).await {
            Ok(v) => ok_json(if v.is_null() { json!({ "ok": true }) } else { v }),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "ListAttachments lists attachments with pagination and filter.")]
    async fn attachment_list_attachments(
        &self,
        Parameters(p): Parameters<ListAttachmentsParams>,
    ) -> CallToolResult {
        let q = opt_query(vec![
            ("pageSize", p.page_size.map(|v| v.to_string())),
            ("pageToken", p.page_token),
            ("filter", p.filter),
            ("orderBy", p.order_by),
        ]);
        match self.client.get("/api/v1/attachments", q).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "CreateAttachment creates an attachment (base64 content).")]
    async fn attachment_create_attachment(
        &self,
        Parameters(p): Parameters<CreateAttachmentParams>,
    ) -> CallToolResult {
        let mime = p.mime_type.unwrap_or_else(|| guess_mime(&p.filename).to_string());
        let mut body = json!({ "filename": p.filename, "type": mime });
        if let Some(c) = p.content {
            body["content"] = json!(c);
        }
        if let Some(m) = p.memo {
            body["memo"] = json!(format!("memos/{}", bare_id(&m)));
        }
        match self.client.post("/api/v1/attachments", body).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "GetAttachment gets an attachment by name.")]
    async fn attachment_get_attachment(
        &self,
        Parameters(p): Parameters<AttachmentRef>,
    ) -> CallToolResult {
        let path = format!("/api/v1/attachments/{}", bare_id(&p.attachment));
        match self.client.get(&path, vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "DeleteAttachment deletes an attachment.")]
    async fn attachment_delete_attachment(
        &self,
        Parameters(p): Parameters<AttachmentRef>,
    ) -> CallToolResult {
        let path = format!("/api/v1/attachments/{}", bare_id(&p.attachment));
        match self.client.delete(&path).await {
            Ok(_) => ok_json(json!({ "ok": true })),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "ListShortcuts returns a user's saved shortcuts (reusable CEL filters).")]
    async fn shortcut_list_shortcuts(
        &self,
        Parameters(p): Parameters<ShortcutParams>,
    ) -> CallToolResult {
        let path = format!("/api/v1/users/{}/shortcuts", bare_id(&p.user));
        match self.client.get(&path, vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "GetCurrentUser returns the current authenticated user (whoami).")]
    async fn auth_get_current_user(&self, Parameters(_): Parameters<EmptyParams>) -> CallToolResult {
        match self.client.get("/api/v1/auth/me", vec![]).await {
            Ok(v) => ok_json(v),
            Err(e) => api_fail(e),
        }
    }

    #[tool(description = "List tags with usage counts. No args: top-level tags; recursive=true: every tag; parent=\"x\": children of x.")]
    async fn tag_list_tags(&self, Parameters(p): Parameters<ListTagsParams>) -> CallToolResult {
        let memos = match self.all_memos(None).await {
            Ok(m) => m,
            Err(e) => return api_fail(e),
        };
        let recursive = p.recursive.unwrap_or(false);
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut all: HashSet<String> = HashSet::new();
        for m in &memos {
            let tags = m
                .get("tags")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            for t in tags.iter().filter_map(|t| t.as_str()) {
                all.insert(t.to_string());
                if let Some(parent) = &p.parent {
                    let prefix = format!("{parent}/");
                    if !t.starts_with(&prefix) {
                        continue;
                    }
                    if recursive {
                        *counts.entry(t.to_string()).or_default() += 1;
                    } else {
                        let child = format!("{prefix}{}", t[prefix.len()..].split('/').next().unwrap_or(""));
                        *counts.entry(child).or_default() += 1;
                    }
                } else if recursive {
                    *counts.entry(t.to_string()).or_default() += 1;
                } else {
                    let top = t.split('/').next().unwrap_or(t).to_string();
                    *counts.entry(top).or_default() += 1;
                }
            }
        }
        let mut tags: Vec<Value> = counts
            .into_iter()
            .map(|(name, count)| {
                let mut e = json!({ "name": name, "count": count });
                if all.iter().any(|t| t.starts_with(&format!("{name}/"))) {
                    e["hasChildren"] = json!(true);
                }
                e
            })
            .collect();
        tags.sort_by(|a, b| {
            b["count"]
                .as_u64()
                .cmp(&a["count"].as_u64())
                .then(a["name"].as_str().cmp(&b["name"].as_str()))
        });
        ok_json(json!({ "tags": tags }))
    }

    #[tool(description = "Rename a tag across ALL memos by rewriting memo content. Destructive — modifies memos globally.")]
    async fn tag_rename_tag(&self, Parameters(p): Parameters<RenameTagParams>) -> CallToolResult {
        let old = p.old_tag.trim().trim_start_matches('#').to_string();
        let new = p.new_tag.trim().trim_start_matches('#').to_string();
        if old.is_empty() || new.is_empty() {
            return CallToolResult::error(vec![ContentBlock::text("old_tag and new_tag must be non-empty")]);
        }
        if old == new {
            return CallToolResult::error(vec![ContentBlock::text("old_tag and new_tag are identical")]);
        }
        let filter = format!("tag in [\"{}\"]", old.replace('"', "\\\""));
        let memos = match self.all_memos(Some(filter)).await {
            Ok(m) => m,
            Err(e) => return api_fail(e),
        };
        let re = match tag_rename_regex(&old) {
            Ok(r) => r,
            Err(e) => return CallToolResult::error(vec![ContentBlock::text(format!("bad tag pattern: {e}"))]),
        };
        let mut updated = 0usize;
        let mut failed = vec![];
        for m in &memos {
            let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let rewritten = apply_tag_rename(&re, &content, &new);
            if rewritten == content {
                continue;
            }
            let uid = bare_id(&name);
            let q = vec![("updateMask".to_string(), "content".to_string())];
            match self
                .client
                .patch_q(&format!("/api/v1/memos/{uid}"), q, json!({ "content": rewritten }))
                .await
            {
                Ok(_) => updated += 1,
                Err(e) => failed.push(format!("{name}: {e:#}")),
            }
        }
        ok_json(json!({
            "renamed_from": old,
            "renamed_to": new,
            "memos_matched": memos.len(),
            "memos_updated": updated,
            "failures": failed,
        }))
    }
}

// ---------- prompts ----------

/// (name, description, text) — ported from chriscurrycc/memos-mcp, tool
/// references rewritten to this server's tool names. `review` and
/// `on_this_day` are omitted: their `/api/v1/review/*` backends don't exist.
const PROMPT_DEFS: &[(&str, &str, &str)] = &[
    (
        "capture",
        "Quick-save a thought as a memo",
        "Create a memo with the provided content using the memo_create_memo tool.",
    ),
    (
        "digest",
        "Summarize memo activity for a time period",
        "Create a digest of my memo activity for the specified period.",
    ),
    (
        "tag_overview",
        "Review your tag system and organization",
        "Give me an overview of my tag system:\n1. Call tag_list_tags to get all top-level tags with counts\n2. For tags with children (hasChildren=true), call tag_list_tags with parent param to show the hierarchy\n3. Analyze and present:\n   - Tag hierarchy structure\n   - Most used vs rarely used tags\n   - Suggestions for cleanup (similar tags that could be merged, unused tags, etc.)",
    ),
    (
        "relation_graph",
        "Explore the relation graph starting from a memo",
        "Explore the relation graph starting from the specified memo:\n1. Call memo_get_memo to show the starting memo's content\n2. Call memo_list_memo_relations with the memo id to get relations\n3. For each connected memo, call memo_get_memo to understand its content\n4. Present:\n   - A text-based visualization of the graph structure\n   - Brief summary of each connected memo\n   - The themes and topics that connect these memos",
    ),
];

#[prompt_router(vis = "pub")]
impl MemosServer {
    #[prompt(name = "capture", description = "Quick-save a thought as a memo")]
    fn capture(&self, Parameters(a): Parameters<CaptureArgs>) -> Vec<PromptMessage> {
        let mut content = a.content;
        if let Some(tags) = a.tags.filter(|t| !t.trim().is_empty()) {
            let list = tags.split(',').map(|t| format!("#{}", t.trim().trim_start_matches('#'))).collect::<Vec<_>>().join(" ");
            content = format!("{content}\n\n{list}");
        }
        let vis = a.visibility.unwrap_or_else(|| "PRIVATE".to_string());
        vec![PromptMessage::new_text(
            Role::User,
            format!("Create a memo with the following content using the memo_create_memo tool. Visibility: {vis}.\n\nContent:\n{content}"),
        )]
    }

    #[prompt(name = "digest", description = "Summarize memo activity for a time period")]
    fn digest(&self, Parameters(a): Parameters<DigestArgs>) -> Vec<PromptMessage> {
        let days = match a.period.as_deref().unwrap_or("week") {
            "today" => 1,
            "month" => 30,
            _ => 7,
        };
        vec![PromptMessage::new_text(Role::User, format!(
            "Create a digest of my memo activity for the past {days} days:\n1. Call memo_list_memos with a filter on created_ts to get recent memos\n2. Call tag_list_tags to see tag usage patterns\n3. Summarize total memos, key themes, and trends as a concise briefing"
        ))]
    }

    #[prompt(name = "tag_overview", description = "Review your tag system and organization")]
    fn tag_overview(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, PROMPT_DEFS[2].2.to_string())]
    }

    #[prompt(name = "relation_graph", description = "Explore the relation graph starting from a memo")]
    fn relation_graph(&self, Parameters(a): Parameters<RelationGraphArgs>) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, format!(
            "Explore the relation graph starting from memo {}:\n1. Call memo_get_memo to show the starting memo's content\n2. Call memo_list_memo_relations with the memo id to get relations\n3. For each connected memo, call memo_get_memo to understand its content\n4. Present a text-based visualization of the graph plus theme analysis",
            a.memo
        ))]
    }
}

// ---------- ServerHandler ----------

#[tool_handler]
#[prompt_handler]
impl ServerHandler for MemosServer {
    fn get_info(&self) -> InitializeResult {
        let mut info =
            InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
        info.capabilities.prompts = Some(Default::default());
        info.capabilities.resources = Some(Default::default());
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: self.list_memo_resources(),
            ..Default::default()
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult {
            resource_templates: self.list_memo_resource_templates(),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.read_memo_resource(&request.uri).await
    }
}

// ---------- FilteredServer support ----------

impl MemosServer {
    pub async fn call_tool_filtered(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let router: ToolRouter<MemosServer> = Self::tool_router();
        let ctx = ToolCallContext::new(self, request, context);
        router.call(ctx).await
    }

    pub fn list_tools_filtered(&self, allow: &HashSet<String>) -> ListToolsResult {
        let tools = Self::tool_router()
            .list_all()
            .into_iter()
            .filter(|t| allow.contains(t.name.as_ref()))
            .collect::<Vec<_>>();
        ListToolsResult { tools, ..Default::default() }
    }

    pub async fn get_prompt_filtered(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let router: PromptRouter<MemosServer> = Self::prompt_router();
        let ctx = PromptContext::new(self, request.name, request.arguments, context);
        router.get_prompt(ctx).await
    }

    pub fn list_prompts_all(&self) -> ListPromptsResult {
        ListPromptsResult { prompts: Self::prompt_router().list_all(), ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(old: &str, new: &str, content: &str) -> String {
        let re = tag_rename_regex(old).expect("test pattern compiles");
        apply_tag_rename(&re, content, new)
    }

    #[test]
    fn rename_simple_and_end_of_string() {
        assert_eq!(rename("rust", "rustlang", "I love #rust"), "I love #rustlang");
        assert_eq!(rename("rust", "rustlang", "#rust"), "#rustlang");
    }

    #[test]
    fn rename_preserves_boundary_char() {
        assert_eq!(rename("rust", "rustlang", "#rust, and #rust."), "#rustlang, and #rustlang.");
        assert_eq!(rename("rust", "rustlang", "#rust\nmore"), "#rustlang\nmore");
        assert_eq!(rename("rust", "rustlang", "(#rust)"), "(#rustlang)");
    }

    #[test]
    fn rename_does_not_match_longer_tag() {
        assert_eq!(rename("e2e-test", "e2e-done", "#e2e-testing"), "#e2e-testing");
        assert_eq!(
            rename("e2e-test", "e2e-done", "#e2e-test #e2e-testing"),
            "#e2e-done #e2e-testing"
        );
    }

    #[test]
    fn rename_keeps_child_segments() {
        assert_eq!(
            rename("parent", "p2", "#parent/child and #parent"),
            "#p2/child and #p2"
        );
        assert_eq!(
            rename("parent/child", "parent/kid", "#parent/child/grand"),
            "#parent/kid/grand"
        );
    }

    #[test]
    fn rename_multiple_occurrences() {
        assert_eq!(
            rename("a", "b", "#a #a! #a?"),
            "#b #b! #b?"
        );
    }

    #[test]
    fn rename_escapes_regex_meta_in_old_tag() {
        assert_eq!(rename("a.b", "c", "#a.b x"), "#c x");
        assert_eq!(rename("a.b", "c", "#axb"), "#axb");
    }

    #[test]
    fn memo_markdown_derives_uid_from_name() {
        let memo = json!({
            "name": "memos/abc123",
            "content": "hello #x",
            "visibility": "PRIVATE",
            "createTime": "2026-01-01T00:00:00Z",
            "updateTime": "2026-01-02T00:00:00Z",
            "tags": ["x"],
        });
        let md = memo_markdown(&memo);
        assert!(md.contains("uid: abc123"), "uid missing: {md}");
        assert!(md.contains("hello #x"));
    }
}
