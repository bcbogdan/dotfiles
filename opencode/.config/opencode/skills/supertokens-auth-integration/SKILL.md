---
name: supertokens-auth-integration
description: Integrate SuperTokens authentication into apps. Use when asked to add, migrate, or plan auth with SuperTokens, including choosing recipes, wiring frontend and backend SDKs, setting up sessions, and handling managed vs self-hosted core.
---

# SuperTokens Auth Integration

## Overview

Set up SuperTokens auth by wiring the frontend SDK to your backend SDK, and the backend SDK to the SuperTokens Core. Decide recipes, stack-specific wiring, and migration approach when existing auth is present.

## Workflow

Follow this sequence, branching with the decision trees below.

1. Identify current state and goals.
2. Pick architecture (managed vs self-hosted) and SDK placement.
3. Choose recipes and UI strategy.
4. Integrate backend SDK routes and session middleware.
5. Integrate frontend SDK and UI.
6. Validate end-to-end auth flow.

## Decision Trees

### A. Existing auth?

- **No existing auth** → greenfield flow. Create auth routes, sessions, and UI from scratch.
- **Existing auth present** → migration flow.
  - Determine: keep current provider temporarily, or cutover fully.
  - If gradual: run both systems, map user identities, and plan session coexistence.
  - If full cutover: replace auth routes and login UI; migrate user data if needed.

### B. Backend SDK availability

- **Backend stack has SDK** (Node, Python, Go) → integrate SDK in the existing backend.
- **Backend stack has no SDK** → add a dedicated auth service in Node/Python/Go; proxy frontend auth requests to it.

### C. Deployment model

- **Managed core** → use hosted SuperTokens Core.
- **Self-hosted core** → deploy Core in your infra; set core connection to internal URL.

### D. App topology

- **Monolith** → add backend SDK routes in the main server.
- **Backend + frontend separate** → add backend SDK routes in API service; configure frontend to call API base URL.
- **Multiple backends** → centralize auth in one service; share session verification across services.

### E. Client platform

- **Web SPA/SSR** → use SuperTokens frontend SDK; handle cookies and session refresh.
- **Mobile** → use backend SDK + your own UI; handle session tokens per mobile SDK guidance.

### F. UI strategy

- **Prebuilt UI** → drop in SuperTokens UI components; faster integration.
- **Custom UI** → build login/register UI and call frontend SDK APIs.

### G. Recipe choice

- **Email/password** → standard web login.
- **Third-party (OAuth)** → social login; configure providers.
- **Passwordless** → magic link or OTP.
- **Session recipe** → always required for session management.
- **User roles/permissions** → add if you need RBAC.

## Integration Steps

### 1. Discovery

- Identify backend and frontend frameworks, deployment topology, and current auth.
- Confirm core model (managed vs self-hosted).
- Confirm desired recipes and UI approach.

### 2. Backend SDK setup

- Add backend SDK config, including core connection and app info.
- Expose auth routes from the SDK.
- Add session middleware in the correct order for your framework.
- If using multiple services, add session verification helpers to downstream services.

### 3. Frontend SDK setup

- Add frontend SDK config pointing to backend API base URL.
- Integrate prebuilt or custom UI.
- Ensure session refresh and anti-CSRF handling is enabled.

### 4. Validation

- Verify signup/login, session creation, session refresh, and logout.
- Check cookie domain, CORS, and secure flags for prod.

## References

- Read `references/supertokens-architecture.md` for the core SDK flow and constraints.
- Read `references/stack-notes.md` for stack-specific placement and common pitfalls.
