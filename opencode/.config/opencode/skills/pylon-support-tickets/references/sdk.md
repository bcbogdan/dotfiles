# SDKs

SuperTokens uses SDKs in the app layer and a Core service. SDKs communicate with Core over an API through your backend.
Frontend SDK never talks to Core directly.
When responding, distinguish:

- **Backend SDK** (runs in API server)
- **Frontend SDK** (browser app integration)
- **Frontend UI SDK** (prebuilt UI components)

Repos:

- Web SDK: https://github.com/supertokens/supertokens-web-js
- Auth React UI SDK: https://github.com/supertokens/supertokens-auth-react
- Node backend SDK: https://github.com/supertokens/supertokens-node
- Python backend SDK: https://github.com/supertokens/supertokens-python
- Go backend SDK: https://github.com/supertokens/supertokens-golang
- Android SDK: https://github.com/supertokens/supertokens-android
- iOS SDK: https://github.com/supertokens/supertokens-ios
- React Native SDK: https://github.com/supertokens/supertokens-react-native
- Flutter SDK: https://github.com/supertokens/supertokens-flutter

Version guidance:

- When reading code, use the latest tag, not main/master.
- If you need to inspect locally: clone the repo, list tags, checkout the latest semver tag, then read.

Terminology:

- "Recipe" refers to an auth flow/module (e.g., EmailPassword, ThirdParty, Passwordless, Session).
- SDKs integrate recipes into the app and communicate with the Core service.
