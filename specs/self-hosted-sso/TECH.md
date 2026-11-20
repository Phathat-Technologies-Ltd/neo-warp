# Self-Hosted Backend with Organization Sign-In: Tech Spec (Phase 1, Auth)

See `PRODUCT.md` for scope.

## Current client auth flow

1. Sign-in produces a login token, either from the browser redirect after `{server}/login/remote`
   (`app/src/auth/auth_manager.rs`, `sign_in_url`) or from the OAuth2 device flow at
   `{server}/api/v1/oauth/device/auth` and `/api/v1/oauth/token`
   (`crates/warp_server_client/src/auth/session.rs`, `create_oauth_client`). The device flow ends
   with a Firebase custom token.
2. `fetch_user(LoginToken::Firebase(..))` trades that token for an ID token and refresh token at
   Google (`securetoken.googleapis.com/v1/token` for refresh tokens,
   `identitytoolkit.googleapis.com/v1/accounts:signInWithCustomToken` for custom tokens; see
   `FirebaseToken::access_token_url` in `crates/warp_server_auth/src/credentials.rs`). On failure
   it retries through `{server}/proxy/token` and `{server}/proxy/customToken`.
3. It then queries the server for user properties (`fetch_user_properties`), which also returns
   server experiments and the model catalog.
4. Every later request sends the ID token as `Credentials::Firebase`, refreshed through step 2.

Steps 1, 3 and 4 already go to `SERVER_ROOT_URL`. Only step 2 is hard-wired to Google.

## Design

### Client

**1. Organization sign-in flow** (new module, provider-neutral)

Works with any OpenID Connect provider. Configuration is an issuer URL and a public client ID,
plus optional extra scopes and an optional fixed redirect port.

- Fetch `{issuer}/.well-known/openid-configuration` for `authorization_endpoint` and
  `token_endpoint`. Cache it for the session.
- Generate PKCE (S256), `state`, and `nonce`.
- Bind a loopback listener on `127.0.0.1`, on the configured port if set, otherwise an
  OS-assigned one. Generalize the listener in `crates/ai/src/grok_subscription/oauth.rs` (bind,
  single callback, cancellation, release) into a shared helper instead of copying it.
- Open the authorization endpoint with `client_id`, `response_type=code`,
  `redirect_uri=http://localhost:{port}/callback`, `scope=openid profile email offline_access`
  plus any configured scopes, `code_challenge`, `code_challenge_method=S256`, `state`, `nonce`.
- On callback, check `state`, then exchange the code at the token endpoint as a public client (no
  secret).
- Send the provider's ID token to the backend: `POST {server}/auth/sso/exchange` with
  `{ "id_token": "..." }`. The response is `{ "custom_token": "..." }`.
- Continue with the existing path: `fetch_user(LoginToken::Firebase(FirebaseToken::Custom(token)))`.

Loopback port rules differ by provider. Entra and Google accept a registered `http://localhost`
redirect on any port for public clients; Okta and some others require the exact registered port,
which is what the fixed-port setting is for. Verify against the Entra test tenant first.

Example issuer URLs: Entra `https://login.microsoftonline.com/{tenant}/v2.0`, Okta
`https://{org}.okta.com/oauth2/default`, Google `https://accounts.google.com`, Keycloak
`https://{host}/realms/{realm}`.

**Configuration source.** The settings (issuer URL, client ID, display name, extra scopes,
redirect port, token-service root URL) form one `OrganizationSignInConfig`. It is read from
environment variables, then a deployed config file, then the channel config's `server_config`
(where `server_root_url` already lives), first match wins per field.

**2. Configurable token endpoint**

Add a token-service root URL (env `AUTH_TOKEN_ROOT_URL`, later channel config). When set,
`FirebaseToken::access_token_url` builds `{root}/v1/token` and
`{root}/v1/accounts:signInWithCustomToken` instead of the Google URLs, and the proxy fallback is
skipped. When unset, behavior is unchanged.

**3. Login UI (TUI)**

When organization sign-in is configured, `TuiLoginModel` offers only the organization option and
hides Warp signup, sign-in, and skip. Phases: awaiting browser (with the URL and copy action),
exchanging, failed (sanitized message plus retry), signed in. An `error=access_denied` callback
from the provider (for example, an Entra user who isn't assigned to the app) maps to "Your
organization hasn't given you access".

**4. Gating**

Gate everything behind a new `FeatureFlag::OrganizationSignIn`, plus the presence of the issuer
URL and client ID. Without both, the client is unchanged.

### Backend (phase 1 surface)

A new Rust binary crate in this workspace (`crates/selfhost_server`), so it shares the proto,
auth, and GraphQL schema types with the client and builds with the rest of the workspace. It runs
as its own process.

| Endpoint | Purpose |
|---|---|
| `POST /auth/sso/exchange` | Validate the provider's ID token: signature against the `jwks_uri` from the issuer's discovery document, `iss` = configured issuer, `aud` = client ID, `exp`, `nonce`. Optional claim checks per deployment (for Entra, `tid`; for any provider, a required group). Create or update the user keyed on `iss` + `sub`. Return a short-lived custom token. |
| `POST /v1/accounts:signInWithCustomToken` | Firebase-compatible: trade the custom token for `idToken`, `refreshToken`, `expiresIn`. |
| `POST /v1/token` | Firebase-compatible refresh (`grant_type=refresh_token`): returns `id_token`, `refresh_token`, `expires_in`, `user_id`. Refuses disabled users. |
| `POST /graphql/v2` (user query only) | Answer the user-properties query from `fetch_user_properties` with the signed-in user, empty experiments, and an empty model catalog. |

- ID tokens are JWTs the backend signs (1-hour lifetime) and verifies on every request.
- Refresh tokens are opaque and stored server-side, so disabling a user revokes them.
- Response shapes must match what `crates/warp_server_client/src/auth/session.rs` decodes; capture
  them from a real sign-in against Warp's service before implementing.

## Testing

- Unit: authorize-URL building, `state` mismatch rejection, callback error mapping, token-endpoint
  URL selection with and without `AUTH_TOKEN_ROOT_URL`.
- TUI render-to-lines tests for each login phase.
- End to end, first with a test Entra tenant, then one more provider (Okta or Keycloak) to keep
  the flow provider-neutral, each with a local stub backend: sign in, restart (refresh path), then
  disable the user in the provider and confirm access ends within one token lifetime.

## Rollout

1. Capture the exact calls and response shapes for startup and sign-in against Warp's service.
2. Client: token endpoint override, then the sign-in flow and TUI screen, against a stub backend.
3. Backend: the four endpoints above.
4. GUI login screen.
5. Later phases: model catalog, the agent endpoint, the remaining server surface.
