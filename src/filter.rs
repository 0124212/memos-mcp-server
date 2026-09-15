//! Tool filtering: readonly / toolsets / include / exclude.
//!
//! Implements the `X-MCP-*` header semantics plus the path-based route
//! aliases (`/mcp/readonly`, `/mcp/x/{toolsets}`, ...). Prompts and resources
//! are inherently read-only and pass through unfiltered.

use std::{collections::HashSet, sync::Arc};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, GetPromptRequestParams, GetPromptResponse,
        InitializeResult, ListPromptsResult, ListResourcesResult, ListResourceTemplatesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ServerCapabilities,
    },
    service::RequestContext,
};

use crate::{catalog::filter_tools, server::MemosServer};

#[derive(Clone, Debug, Default)]
pub struct ToolFilter {
    pub readonly: bool,
    pub toolsets: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl ToolFilter {
    pub fn allow_list(&self) -> HashSet<String> {
        filter_tools(self.readonly, &self.toolsets, &self.include, &self.exclude)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Parse `X-MCP-Readonly`, `X-MCP-Toolsets`, `X-MCP-Tools`,
    /// `X-MCP-Exclude-Tools` headers into a filter.
    pub fn from_headers(headers: &axum::http::HeaderMap) -> Self {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let split = |s: String| {
            s.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let readonly =
            matches!(get("x-mcp-readonly").to_lowercase().as_str(), "1" | "true" | "yes");
        Self {
            readonly,
            toolsets: split(get("x-mcp-toolsets")),
            include: split(get("x-mcp-tools")),
            exclude: split(get("x-mcp-exclude-tools")),
        }
    }

    /// Combine path-derived constraints (route aliases) with header filters.
    pub fn with_path(mut self, path: &str) -> Self {
        let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
        // /mcp | /mcp/readonly | /mcp/x/{toolsets} | /mcp/x/{toolsets}/readonly
        match segs.as_slice() {
            ["mcp", "readonly"] => self.readonly = true,
            ["mcp", "x", toolsets] => self.toolsets.extend(split_csv(toolsets)),
            ["mcp", "x", toolsets, "readonly"] => {
                self.readonly = true;
                self.toolsets.extend(split_csv(toolsets));
            }
            _ => {}
        }
        self
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Clone)]
pub struct FilteredServer {
    inner: MemosServer,
    allow: Arc<HashSet<String>>,
}

impl FilteredServer {
    pub fn new(inner: MemosServer, filter: &ToolFilter) -> Self {
        Self {
            inner,
            allow: Arc::new(filter.allow_list()),
        }
    }
}

impl ServerHandler for FilteredServer {
    fn get_info(&self) -> InitializeResult {
        let mut info =
            InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
        info.capabilities.prompts = Some(Default::default());
        info.capabilities.resources = Some(Default::default());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(self.inner.list_tools_filtered(&self.allow))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.allow.contains(request.name.as_ref()) {
            return Err(ErrorData::invalid_params(
                format!("tool '{}' is not enabled for this endpoint", request.name),
                None,
            ));
        }
        self.inner.call_tool_filtered(request, context).await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(self.inner.list_prompts_all())
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        self.inner.get_prompt_filtered(request, context).await
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: self.inner.list_memo_resources(),
            ..Default::default()
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult {
            resource_templates: self.inner.list_memo_resource_templates(),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.inner.read_memo_resource(&request.uri).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                k.parse::<HeaderName>().expect("header name"),
                HeaderValue::from_str(v).expect("header value"),
            );
        }
        h
    }

    #[test]
    fn path_aliases() {
        assert!(!ToolFilter::default().with_path("/mcp").readonly);
        assert!(ToolFilter::default().with_path("/mcp/readonly").readonly);
        let f = ToolFilter::default().with_path("/mcp/x/memos,tags");
        assert_eq!(f.toolsets, vec!["memos".to_string(), "tags".to_string()]);
        assert!(!f.readonly);
        let f = ToolFilter::default().with_path("/mcp/x/reactions/readonly");
        assert!(f.readonly);
        assert_eq!(f.toolsets, vec!["reactions".to_string()]);
    }

    #[test]
    fn headers_parse() {
        let f = ToolFilter::from_headers(&headers(&[
            ("x-mcp-readonly", "true"),
            ("x-mcp-toolsets", "tags, memos"),
            ("x-mcp-exclude-tools", "tag_rename_tag"),
        ]));
        assert!(f.readonly);
        assert_eq!(f.toolsets, vec!["tags".to_string(), "memos".to_string()]);
        assert!(f.include.is_empty());
        assert_eq!(f.exclude, vec!["tag_rename_tag".to_string()]);

        let f = ToolFilter::from_headers(&headers(&[]));
        assert!(!f.readonly);
        assert!(f.toolsets.is_empty());
    }

    #[test]
    fn allow_list_matches_catalog() {
        let f = ToolFilter {
            readonly: true,
            toolsets: vec![],
            include: vec![],
            exclude: vec![],
        };
        let allow = f.allow_list();
        assert_eq!(allow.len(), 11);
        assert!(allow.contains("memo_list_memos"));
        assert!(!allow.contains("memo_create_memo"));
    }
}
