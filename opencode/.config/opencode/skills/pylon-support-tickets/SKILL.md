---
name: pylon-support-tickets
description: |
  Triage and troubleshoot Pylon support tickets for Supertokens.
  Use when reading Pylon issues via API, checking report clarity, requesting missing data (logs, SDK/Core versions), attempting repros with create-supertokens-app or local SDK/core clones, and drafting responses.
---

# Pylon Support Tickets

## Overview

Handle Pylon issues end-to-end: fetch the ticket, assess clarity, gather missing context, attempt repro, and respond with findings or next steps.
Do not respond directly to the ticket. Present conclusions and recommended replies to the user only.

## Workflow

### 1) Fetch issue

Use Pylon API to read the issue payload including all replies/messages. See `references/pylon-api.md` for endpoints and fields.

### 2) Extract essentials

Pull: title, description, environment, SDKs + versions, Core version, framework, error messages, logs, reproduction steps, expected vs actual.
If missing, prepare a focused request using `references/checklist.md`.
Use `references/sdk.md` and `references/core.md` to describe components and version expectations.

### 3) Decide clarity

- **Clear**: has repro steps + versions + environment + logs/stack trace.
- **Unclear**: ask for missing details before deep investigation.

### 4) Attempt repro

Match the reporter's stack. Use `create-supertokens-app` when possible; otherwise create a minimal local project with the relevant SDK (e.g. NestJS). See `references/repro.md` for guidance and defaults.
Confirm SDK/Core version alignment using `references/sdk.md` and `references/core.md`.

### 5) Investigate deeper

If needed, clone and inspect the SDK or Core repo, compare recent changes, and look for related issues.

### 6) Respond

Provide a short summary, repro status, suspected cause, and next steps. Ask only for missing info.

## Decision trees

### Core not responding/ Service error

### CORS related issues

### Feature not working

###

## References

- `references/pylon-api.md`
- `references/checklist.md`
- `references/repro.md`
- `references/sdk.md`
- `references/core.md`
