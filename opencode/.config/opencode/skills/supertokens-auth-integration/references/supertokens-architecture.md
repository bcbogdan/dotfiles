# SuperTokens Architecture Notes

## Core flow

- Frontend SDK never talks to Core directly.
- Frontend SDK talks to your backend auth routes.
- Backend SDK exposes auth routes and talks to SuperTokens Core.

## Core placement

- Managed core: hosted by SuperTokens.
- Self-hosted core: run inside your infra; backend SDK points to it.

## Recipes

- Recipes are feature bundles (auth methods, session, user management).
- Session recipe is required for session handling.

## Edge cases

- If backend stack has no SDK, add a small auth service in Node/Python/Go.
