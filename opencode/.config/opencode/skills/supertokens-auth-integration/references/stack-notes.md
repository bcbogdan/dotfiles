# Stack Notes and Pitfalls

## Placement

- Backend SDK routes must be registered before custom auth routes that depend on sessions.
- Session verification middleware should run early in the request pipeline.

## CORS and cookies

- Ensure API and frontend origins are configured in app info.
- Use correct cookie domain for subdomains.
- In prod, enforce secure cookies and same-site rules based on deployment.

## Multi-service

- Centralize auth in one service and share session verification utilities across services.
- Avoid multiple services exposing conflicting auth routes.

## UI

- Prebuilt UI speeds initial integration; custom UI needs explicit error handling.
