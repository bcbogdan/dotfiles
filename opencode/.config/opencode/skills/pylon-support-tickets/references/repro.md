# Repro Guidance

Prefer the smallest repro that matches the user's stack.

Options:
- **create-supertokens-app**: use when a standard quickstart matches the framework.
  - Repo: https://github.com/supertokens/create-supertokens-app
  - Use the latest CLI and select the closest framework and auth recipe.
- **Local minimal project**: use when the issue is framework-specific or requires custom setup.
  - Example SDK: https://github.com/supertokens/supertokens-nestjs

General approach:
1) Align SDK/Core versions with the report.
2) Reproduce on default config first, then add custom settings.
3) Capture logs, stack traces, and exact steps.
4) If not reproducible, state what you tried and what differs.

When deeper inspection is needed:
- Clone SDK/Core repos and search recent changes related to the affected area.
- Cross-check open issues and recent releases for regressions.
