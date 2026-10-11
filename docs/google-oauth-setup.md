# Google sign-in for Familiar: the one-time setup

"Connect Google" (Integrations → Accounts) needs one Google OAuth client for the whole app. Release builds carry
Familiar's; you only do this if you build Familiar yourself or run your own release pipeline. It takes about ten minutes.
No server is involved: tokens stay on each user's computer.

Don't want a Google Cloud project at all? Use **Gmail (App Password)** in the same place instead (mail only, no
Calendar). See [the end of this page](#gmail-with-an-app-password-no-google-cloud).

## 1. Project and APIs
1. Open <https://console.cloud.google.com/> and sign in.
2. Top bar → project picker → **New project** → name `Familiar` → **Create**. Make sure it is selected.
3. **APIs & Services → Library** → search **Gmail API** → **Enable**.
4. Back to **Library** → search **Google Calendar API** → **Enable**.

## 2. Consent screen ("Google Auth Platform")
1. **APIs & Services → OAuth consent screen** → **Get started**.
2. App name `Familiar`, user support email: yours → **Next**.
3. Audience: **External** → **Next**. Contact email: yours → **Next** → agree → **Create**.
4. **Data Access** → **Add or remove scopes** → paste these three into "Manually add scopes" → **Add to table** →
   **Update** → **Save**:
   ```
   https://www.googleapis.com/auth/gmail.readonly
   https://www.googleapis.com/auth/gmail.compose
   https://www.googleapis.com/auth/calendar.events
   ```
5. **Audience**, pick one:
   - **Test users** → **Add users** → the Google accounts that will connect (up to 100). Simple, but Google ends a
     test app's sign-ins after 7 days (Familiar then shows "Reconnect").
   - **Publish app** → **Confirm** (status "In production", unverified). Sign-ins last; each user sees "Google hasn't
     verified this app" once → **Advanced** → **Go to Familiar (unsafe)** → **Continue**. Up to 100 users until Google
     verifies the app (privacy policy page and, for Gmail scopes, a security assessment).

## 3. The client
1. **APIs & Services → Credentials** (or **Clients**) → **Create credentials → OAuth client ID**.
2. Application type **Desktop app**, name `Familiar desktop` → **Create**.
3. Copy the **Client ID** and **Client secret**. A desktop client's secret is not confidential (Google says so: it ships
   inside the app), but keep it out of chats and screenshots anyway.

No redirect URI to add: desktop clients accept `http://127.0.0.1:<any port>/`, which is what Familiar listens on.

## 4. Where to put them
- **Release builds (GitHub Actions):** repository → **Settings → Secrets and variables → Actions → Variables** tab →
  **New repository variable** twice: `FAMILIAR_GOOGLE_CLIENT_ID` and `FAMILIAR_GOOGLE_CLIENT_SECRET`. Variables, not
  secrets (they end up in the app anyway). `release.yml` passes them to both builds; if they are missing, the app
  shows this setup note instead of "Connect Google".
- **Your own build:** either set the same two environment variables when you run `cargo build`, or add to
  `~/.familiar/config.toml` (no rebuild needed):
  ```toml
  [google]
  client_id = "1234567890-abc.apps.googleusercontent.com"
  client_secret = "GOCSPX-…"
  ```
  Then restart Familiar.

## Gmail with an App Password (no Google Cloud)
1. The Google account needs **2-Step Verification** on (<https://myaccount.google.com/signinoptions/two-step-verification>).
2. Open <https://myaccount.google.com/apppasswords>, name it `Familiar`, **Create**, copy the 16 letters.
3. Familiar → Integrations → Accounts → **Gmail (App Password)** → your address and the 16 letters → **Connect**.

If the page says app passwords aren't available: 2-Step Verification is off, the account uses only security keys
(Advanced Protection), or a Google Workspace admin turned them off (ask them, or use Connect Google). Familiar uses
IMAP and SMTP over TLS (`imap.gmail.com:993`, `smtp.gmail.com:465`); revoke the password on the same page to cut it off.
