# Microsoft Entra ID setup for organization sign-in

This guide is for the Entra administrator. When it's done, users sign in to Warp with their
Microsoft work account, and only the users or groups you assign can use it.

You need an Entra role that can register applications and manage enterprise applications
(Application Administrator or Cloud Application Administrator).

## 1. Register the application

1. Open the [Microsoft Entra admin center](https://entra.microsoft.com) and go to
   **Identity > Applications > App registrations > New registration**.
2. **Name:** `Warp` (users see this on the sign-in and consent screens).
3. **Supported account types:** *Accounts in this organizational directory only (Single tenant)*.
4. **Redirect URI:** choose the platform **Public client/native (mobile & desktop)** and enter:

   ```
   http://localhost/callback
   ```

   Warp listens on a free local port each time, for example `http://localhost:53124/callback`.
   Entra ignores the port for `localhost` redirect URIs, but the `/callback` path must match.
5. Select **Register**.

Do not use the **Web** or **Single-page application** platforms. Warp is a public client with no
client secret, and those platforms reject its token requests.

## 2. Copy the IDs

On the application's **Overview** page, copy:

- **Application (client) ID**
- **Directory (tenant) ID**

You'll give both to whoever deploys Warp and the backend (step 7).

## 3. Leave client authentication as public

Under **Authentication**:

- No client secret or certificate is needed. Don't create one for Warp.
- **Allow public client flows** can stay **No**. Warp uses the authorization-code flow with PKCE,
  which doesn't need it.

## 4. API permissions

Under **API permissions**, the registration needs these **Microsoft Graph** delegated
permissions:

| Permission | Why |
|---|---|
| `openid` | Sign the user in and issue an ID token |
| `profile` | The user's name |
| `email` | The user's email address |
| `offline_access` | Keep the user signed in between sessions |

`User.Read` is added by default and is harmless; you can remove it.

If users in your tenant can't consent to apps themselves, select **Grant admin consent for
{your organization}** so they don't see a consent request.

## 5. Restrict who can sign in

By default, every user in the tenant can sign in. To limit it:

1. Go to **Identity > Applications > Enterprise applications** and open **Warp** (created
   automatically with the registration).
2. Under **Properties**, set **Assignment required?** to **Yes** and save.
3. Under **Users and groups**, select **Add user/group** and assign the people or groups who may
   use Warp.

Unassigned users who try to sign in see Microsoft's "not assigned to this application" error
(`AADSTS50105`), and Warp tells them their organization hasn't given them access.

Optional: target the **Warp** enterprise application with Conditional Access policies (MFA,
compliant devices, locations) like any other app.

## 6. Optional: include groups in the ID token

Only needed if the Warp backend is configured to require a group, in addition to assignment.

1. In the app registration, go to **Token configuration > Add groups claim**.
2. Select **Security groups** (or **Groups assigned to the application**, which keeps the token
   small) and check **ID** under token types.

Users in more than 200 groups get no group list in the token (Entra's "group overage"). Prefer
**Groups assigned to the application** if that's a risk.

## 7. Values to hand over

| Setting | Value |
|---|---|
| Issuer URL | `https://login.microsoftonline.com/{tenant-id}/v2.0` |
| Client ID | the Application (client) ID |
| Display name (optional) | `Microsoft`, so the button reads "Sign in with Microsoft" |

For local testing, these map to environment variables on the client:

```
WARP_ORG_SIGN_IN_ISSUER=https://login.microsoftonline.com/<tenant-id>/v2.0
WARP_ORG_SIGN_IN_CLIENT_ID=<application-client-id>
WARP_ORG_SIGN_IN_DISPLAY_NAME=Microsoft
```

To deploy to laptops instead, put the same values in the organization config file. Device
management can push it to `/Library/Application Support/Warp/organization.toml` (macOS),
`%ProgramData%\Warp\organization.toml` (Windows), or `/etc/warp/organization.toml` (Linux):

```toml
[sign_in]
issuer = "https://login.microsoftonline.com/<tenant-id>/v2.0"
client_id = "<application-client-id>"
display_name = "Microsoft"
```

By default Warp runs its sign-in server embedded in the app, so nothing else needs deploying. A
standalone `selfhost-server` (set `server_url` in `organization.toml`) needs the same issuer URL and
client ID to validate tokens.

To check the issuer URL, open
`https://login.microsoftonline.com/{tenant-id}/v2.0/.well-known/openid-configuration` in a browser.
It should return JSON whose `issuer` field contains your tenant ID.

## Removing access

Remove the user's assignment, or disable or delete the user. Their next sign-in is refused, and
an already signed-in user loses access when their session next refreshes (within about an hour).

## Troubleshooting

| Error in the browser | Cause | Fix |
|---|---|---|
| `AADSTS50011` redirect URI mismatch | The redirect URI is missing, has the wrong path, or is under the wrong platform | Register `http://localhost/callback` under **Public client/native (mobile & desktop)** |
| `AADSTS7000218` request must contain `client_assertion` or `client_secret` | The redirect URI is under the **Web** platform | Move it to **Public client/native (mobile & desktop)** |
| `AADSTS50105` user not assigned | Assignment is required and the user isn't assigned | Assign the user or one of their groups (step 5) |
| `AADSTS65001` consent required | Users can't consent and admin consent wasn't granted | Grant admin consent (step 4) |
| `AADSTS700016` application not found in the directory | Wrong client ID or tenant ID | Recheck the values from step 2 |
| `AADSTS90002` tenant not found | Wrong tenant ID in the issuer URL | Use the Directory (tenant) ID from step 2 |
