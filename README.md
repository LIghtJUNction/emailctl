# emailctl

[![Crates.io](https://img.shields.io/crates/v/emailctl.svg)](https://crates.io/crates/emailctl)
[![License](https://img.shields.io/crates/l/emailctl.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

`emailctl` is a small, scriptable terminal email client for people who want to
read and send mail without leaving the shell. Install the crate, run the `email`
binary, and log in with the address you already use.

Gmail accounts use browser-based OAuth. Common non-Gmail providers use IMAP/SMTP
presets, and custom domains can pass their servers explicitly.

## Highlights

- One command to add an account: `email auth login you@example.com`
- Gmail OAuth flow with a local callback server and token refresh
- IMAP inbox listing and message reading for generic providers
- SMTP sending with `--body` or stdin
- Multiple accounts with an active default and per-command `--account` override
- Plain text output that works well in scripts, pipes, and terminals

## Install

```sh
cargo install emailctl
```

The installed command is `email`:

```sh
email --help
```

From a checkout:

```sh
cargo run -- --help
```

## Quickstart

```sh
email auth login "$EMAIL_ADDRESS"
email list --limit 10
email read MESSAGE_ID_OR_UID
email send --to "$TO_ADDRESS" --subject "Hello" --body "Sent from email"
```

For Gmail, `MESSAGE_ID_OR_UID` is the Gmail API message id printed by
`email list`. For IMAP accounts, it is the IMAP UID.

## Gmail Setup

Gmail requires a Google OAuth client the first time you log in. `email` stores
the client id and secret after the first successful setup, so future Gmail
accounts can reuse them.

1. Enable the Gmail API:
   <https://console.cloud.google.com/apis/library/gmail.googleapis.com>
2. Create an OAuth client:
   <https://console.cloud.google.com/apis/credentials>
3. Choose `Web application`.
4. Add this authorized redirect URI:
   `http://127.0.0.1:8765/callback`

Then log in:

```sh
export GMAIL_CLIENT_ID=...
export GMAIL_CLIENT_SECRET=...
email auth login "$GMAIL_ADDRESS"
```

If you are working over SSH or another browserless environment, print the URL
instead of opening it automatically:

```sh
email auth login "$GMAIL_ADDRESS" --no-browser
```

## IMAP/SMTP Accounts

For supported domains, `email` selects IMAP and SMTP hosts from the address and
prompts for the password securely:

```sh
email auth login "$EMAIL_ADDRESS"
```

Built-in presets currently cover:

| Provider family | Domains |
| --- | --- |
| Outlook | `outlook.com`, `hotmail.com`, `live.com`, `msn.com` |
| Yahoo | `yahoo.com`, `ymail.com`, `rocketmail.com` |
| iCloud | `icloud.com`, `me.com`, `mac.com` |
| QQ Mail | `qq.com` |
| NetEase | `163.com`, `126.com` |

For custom domains, pass the servers yourself:

```sh
email auth login "$EMAIL_ADDRESS" \
  --imap-host imap.example.com \
  --smtp-host smtp.example.com
```

Optional overrides are available for username and ports:

```sh
email auth login "$EMAIL_ADDRESS" \
  --username "$LOGIN_NAME" \
  --imap-host imap.example.com \
  --imap-port 993 \
  --smtp-host smtp.example.com \
  --smtp-port 587
```

## Commands

| Command | Purpose |
| --- | --- |
| `email auth login <email>` | Add a Gmail or IMAP/SMTP account |
| `email accounts` | List configured accounts |
| `email auth whoami` | Print the active account |
| `email use <email>` | Set the active account |
| `email list --limit 10` | List recent messages |
| `email list --query "from:github newer_than:7d"` | Search Gmail messages |
| `email read <id-or-uid>` | Read a message |
| `email send --to <email> --subject <subject> --body <text>` | Send a message |
| `email auth logout <email>` | Remove an account |

`email send` reads stdin when `--body` is omitted:

```sh
printf 'body from stdin\n' | email send --to "$TO_ADDRESS" --subject "Hello"
```

Use a non-default account for a single command:

```sh
email list --account "$EMAIL_ADDRESS"
email send --account "$EMAIL_ADDRESS" --to "$TO_ADDRESS" --subject "Hi" --body "Hello"
```

## Troubleshooting

When Google returns an error, `email` prints the Google message plus the setup
links that usually fix it.

Common Gmail fixes:

- Enable the Gmail API in the same Google Cloud project as the OAuth client:
  <https://console.cloud.google.com/apis/library/gmail.googleapis.com>
- Add yourself as a test user if the OAuth app is in testing mode:
  <https://console.cloud.google.com/apis/credentials/consent>
- Remove an old OAuth grant before retrying:
  <https://myaccount.google.com/permissions>

For IMAP/SMTP accounts, many providers require an app password rather than your
normal account password. Generate one in your provider's security settings if
regular login fails.

## Configuration And Security

Accounts are saved at:

```text
~/.config/emailctl/config.json
```

On Unix systems the file is written with `0600` permissions. This early release
stores Gmail OAuth tokens and generic account passwords in that config file, so
keep it private and avoid syncing it to untrusted machines.

## Project Status

`emailctl` is intentionally small and currently focused on plain-text reading
and sending. It does not yet provide attachment handling, rich HTML rendering,
mailbox management, or encrypted credential storage.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.
