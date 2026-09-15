//! Curated tool catalog.
//!
//! The 20 official operations mirror `server/router/mcp/catalog.go` at
//! v0.30.0 (`ShortcutService_ListShortcuts` replaced
//! `UserService_ListMemoViews` in 0.30). Tool names follow the official
//! `toolNameFromOperationID` rule: strip `Service`, snake_case both parts.
//! Two extra `tags` tools are ported from chriscurrycc/memos-mcp and work
//! against the stock API (tags come from memo listing; rename is a
//! client-side read-modify-write since no rename endpoint exists).

/// (tool_name, operation_id, method, path, readonly, toolset)
pub const CURATED_TOOLS: &[(&str, &str, &str, &str, bool, &str)] = &[
    ("memo_list_memos", "MemoService_ListMemos", "GET", "/api/v1/memos", true, "memos"),
    ("memo_create_memo", "MemoService_CreateMemo", "POST", "/api/v1/memos", false, "memos"),
    ("memo_get_memo", "MemoService_GetMemo", "GET", "/api/v1/memos/{memo}", true, "memos"),
    ("memo_update_memo", "MemoService_UpdateMemo", "PATCH", "/api/v1/memos/{memo}", false, "memos"),
    ("memo_delete_memo", "MemoService_DeleteMemo", "DELETE", "/api/v1/memos/{memo}", false, "memos"),
    ("memo_list_memo_comments", "MemoService_ListMemoComments", "GET", "/api/v1/memos/{memo}/comments", true, "memos"),
    ("memo_create_memo_comment", "MemoService_CreateMemoComment", "POST", "/api/v1/memos/{memo}/comments", false, "memos"),
    ("memo_list_memo_attachments", "MemoService_ListMemoAttachments", "GET", "/api/v1/memos/{memo}/attachments", true, "attachments"),
    ("memo_set_memo_attachments", "MemoService_SetMemoAttachments", "PATCH", "/api/v1/memos/{memo}/attachments", false, "attachments"),
    ("memo_list_memo_reactions", "MemoService_ListMemoReactions", "GET", "/api/v1/memos/{memo}/reactions", true, "reactions"),
    ("memo_upsert_memo_reaction", "MemoService_UpsertMemoReaction", "POST", "/api/v1/memos/{memo}/reactions", false, "reactions"),
    ("memo_delete_memo_reaction", "MemoService_DeleteMemoReaction", "DELETE", "/api/v1/memos/{memo}/reactions/{reaction}", false, "reactions"),
    ("memo_list_memo_relations", "MemoService_ListMemoRelations", "GET", "/api/v1/memos/{memo}/relations", true, "relations"),
    ("memo_set_memo_relations", "MemoService_SetMemoRelations", "PATCH", "/api/v1/memos/{memo}/relations", false, "relations"),
    ("attachment_list_attachments", "AttachmentService_ListAttachments", "GET", "/api/v1/attachments", true, "attachments"),
    ("attachment_create_attachment", "AttachmentService_CreateAttachment", "POST", "/api/v1/attachments", false, "attachments"),
    ("attachment_get_attachment", "AttachmentService_GetAttachment", "GET", "/api/v1/attachments/{attachment}", true, "attachments"),
    ("attachment_delete_attachment", "AttachmentService_DeleteAttachment", "DELETE", "/api/v1/attachments/{attachment}", false, "attachments"),
    ("shortcut_list_shortcuts", "ShortcutService_ListShortcuts", "GET", "/api/v1/users/{user}/shortcuts", true, "memos"),
    ("auth_get_current_user", "AuthService_GetCurrentUser", "GET", "/api/v1/auth/me", true, "memos"),
    ("tag_list_tags", "-", "GET", "/api/v1/memos", true, "tags"),
    ("tag_rename_tag", "-", "PATCH", "/api/v1/memos/{memo}", false, "tags"),
];

/// Filter a tool list by readonly flag / toolsets / include / exclude.
/// Mirrors the `X-MCP-*` header semantics.
pub fn filter_tools(
    readonly: bool,
    toolsets: &[String],
    include: &[String],
    exclude: &[String],
) -> Vec<&'static str> {
    CURATED_TOOLS
        .iter()
        .filter(|(name, .., ro, ts)| {
            if readonly && !*ro {
                return false;
            }
            if !toolsets.is_empty() && !toolsets.iter().any(|t| t == *ts) {
                return false;
            }
            if !include.is_empty() && !include.iter().any(|t| t == *name) {
                return false;
            }
            if exclude.iter().any(|t| t == *name) {
                return false;
            }
            true
        })
        .map(|(name, ..)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn names(readonly: bool, toolsets: &[&str], include: &[&str], exclude: &[&str]) -> HashSet<String> {
        filter_tools(
            readonly,
            &toolsets.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &include.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &exclude.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn full_catalog_has_22_tools() {
        assert_eq!(names(false, &[], &[], &[]).len(), 22);
    }

    #[test]
    fn readonly_keeps_only_readonly_tools() {
        let got = names(true, &[], &[], &[]);
        assert_eq!(got.len(), 11);
        assert!(got.contains("memo_list_memos"));
        assert!(got.contains("tag_list_tags"));
        assert!(!got.contains("memo_create_memo"));
        assert!(!got.contains("tag_rename_tag"));
    }

    #[test]
    fn toolset_filter_selects_tags() {
        let got = names(false, &["tags"], &[], &[]);
        assert_eq!(got, HashSet::from(["tag_list_tags".to_string(), "tag_rename_tag".to_string()]));
    }

    #[test]
    fn include_and_exclude_combine() {
        let got = names(false, &[], &["memo_list_memos", "memo_create_memo"], &["memo_create_memo"]);
        assert_eq!(got, HashSet::from(["memo_list_memos".to_string()]));
    }

    #[test]
    fn readonly_plus_toolset() {
        let got = names(true, &["memos"], &[], &[]);
        assert!(got.contains("memo_list_memos"));
        assert!(!got.contains("memo_create_memo"));
        assert!(!got.contains("tag_list_tags"));
    }
}
