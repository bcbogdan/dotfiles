# Pylon Issues API

Use the Pylon Issues API to fetch issue details. Docs:
- https://docs.usepylon.com/pylon-docs/developer/api/api-reference/issues

Guidance:
- Prefer read-only operations.
- If API auth is not available, ask the user for the required token or a ready-to-use curl snippet.
- Fetch the issue with all replies/messages included (conversation thread) before summarizing.

Typical data to extract from the payload:
- Issue id, title, description, status, priority
- Customer/org context
- Tags or labels
- Metadata fields that might contain SDK/Core versions
- Conversation messages or attachments

## Fetch commands

Use Bearer token auth. Base URL: `https://api.usepylon.com`

Token loading:
- Load `PYLON_TOKEN` from a local `.env` file if present.
- Accept `PYLON_API_TOKEN` as an alternative env var name.

Get issue details:
```bash
curl -sS \
  -H "Authorization: Bearer $PYLON_TOKEN" \
  "https://api.usepylon.com/issues/$ISSUE_ID"
```

Get all messages (replies):
```bash
curl -sS \
  -H "Authorization: Bearer $PYLON_TOKEN" \
  "https://api.usepylon.com/issues/$ISSUE_ID/messages"
```

Get all threads:
```bash
curl -sS \
  -H "Authorization: Bearer $PYLON_TOKEN" \
  "https://api.usepylon.com/issues/$ISSUE_ID/threads"
```

Pagination note:
- `GetIssueMessagesResponseBody` includes `pagination` with `cursor` and `has_next_page`. If responses are paginated, keep fetching until `has_next_page` is false.
- If the endpoint accepts `cursor`, use it like:
```bash
curl -sS \
  -H "Authorization: Bearer $PYLON_TOKEN" \
  "https://api.usepylon.com/issues/$ISSUE_ID/messages?cursor=$CURSOR"
```
