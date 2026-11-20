# Self-Hosted Backend with Organization Sign-In

## Summary

Let a company run its own backend in place of Warp's service, and have its users sign in with the
company's identity provider (Microsoft Entra ID first) instead of Warp signup/sign-in. The client
keeps its existing account implementation; only the server behind it changes.

This document covers phase 1: authentication. Model catalog, the agent endpoint, and the remaining
server surface are later phases.

## Problem

Warp's client only signs users in to Warp's own account system. A company that wants to self-host
cannot control who uses the product from its own directory, and today there is no "sign in with
your organization" entry point in the product at all.

## Goals

- A build configured for a self-hosted backend shows "Sign in with your organization" in place of
  Warp signup/sign-in.
- Sign-in uses the company's Entra tenant through a public client app registration (no client
  secret) and a `http://localhost` loopback redirect.
- The company decides who can use the product in Entra (assigned users and groups, conditional
  access). Removing a user in Entra stops their access within one token lifetime.
- After sign-in, the client behaves as a normal signed-in user, using the existing account,
  token-refresh, and request-authentication code paths.
- Nothing in the sign-in path contacts Warp or Google services.

## Non-goals (phase 1)

- Identity providers other than Entra (the design keeps the provider generic, but only Entra is
  built and tested).
- SAML. Phase 1 is OIDC only.
- The model catalog, agent requests, Warp Drive, billing, and other server features.
- The GUI login screen. Phase 1 ships in the TUI; the GUI follows once the flow is proven.
- Admin UI for the backend.

## User experience

1. The user launches a build configured with the organization's tenant, client ID, and backend URL.
2. The login screen offers one option: **Sign in with your organization**. Warp signup, sign-in, and
   "skip" are not shown.
3. Choosing it opens the browser on the company's Microsoft sign-in page. The TUI shows the URL with
   a copy action, as the existing login does, for when the browser can't be opened.
4. After the user signs in (including any MFA or conditional-access prompts), the browser shows
   "You're signed in, return to the terminal" and the TUI moves to its signed-in state.
5. A user who isn't assigned to the app in Entra gets Entra's own "not assigned" error in the
   browser, and the TUI shows "Your organization hasn't given you access" with a retry option.
6. If the backend can't be reached, the TUI says so and offers retry; it never falls back to Warp
   sign-in.
7. Signing out clears local credentials; the next launch shows the organization sign-in again.

## Configuration

Set by whoever builds or deploys the client (environment variables first; channel config later):

- Backend URL: the existing `SERVER_ROOT_URL` / `WS_SERVER_URL`.
- Entra tenant ID.
- Entra application (client) ID.

## Admin setup (Entra)

- Register an application as a public client with the redirect URI `http://localhost`.
- Enable "Assignment required" on its enterprise application and assign users or groups.
- Give the backend the tenant ID and client ID so it can validate tokens.

## Open questions

- Backend language and repository.
- Whether the backend should also call Microsoft Graph to re-check group membership on refresh, or
  rely on Entra sign-in plus short token lifetimes.
