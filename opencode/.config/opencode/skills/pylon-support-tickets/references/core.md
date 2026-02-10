# Core

Core is the SuperTokens service that implements auth logic and stores user/session data. Apps talk to Core through Backend SDKs over an API.
Frontend SDK calls the backend, not Core directly.

Repo:
- https://github.com/supertokens/supertokens-core

Version guidance:
- Always record the Core version from the ticket.
- When reading code, use the latest tag, not main/master.

When triaging:
- Confirm Core is reachable and logs show the request path.
- Check Core logs for recipe-specific errors.
- If SDK/Core versions look mismatched, ask for exact versions and align during repro.
