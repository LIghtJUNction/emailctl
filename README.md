# email-cli

`email-cli` is a Rust terminal email client. The installed binary is `email`.

It prioritizes Gmail through browser OAuth login, then falls back to a generic
IMAP/SMTP login for other providers.

## Install

```sh
cargo install email-cli-lightjunction
```

For local development:

```sh
cargo run -- --help
```

## Gmail login

Create a Google OAuth client and allow `http://127.0.0.1:8765/callback` as a
redirect URI. Then login:

```sh
email auth login gmail \
  --email you@gmail.com \
  --client-id "$GMAIL_CLIENT_ID" \
  --client-secret "$GMAIL_CLIENT_SECRET"
```

The client id and secret can also be supplied through environment variables:

```sh
export GMAIL_CLIENT_ID=...
export GMAIL_CLIENT_SECRET=...
email auth login gmail --email you@gmail.com
```

## Generic IMAP/SMTP login

```sh
email auth login generic \
  --email you@example.com \
  --imap-host imap.example.com \
  --smtp-host smtp.example.com
```

If `--password` is omitted, the CLI prompts for it securely.

## Usage

```sh
email accounts
email auth whoami
email list --limit 10
email list --query "from:github newer_than:7d"
email read MESSAGE_ID
email send --to friend@example.com --subject "Hello" --body "Hi from email-cli"
printf 'body from stdin\n' | email send --to friend@example.com --subject "Hello"
email use you@example.com
email auth logout you@example.com
```

For Gmail, `MESSAGE_ID` is the Gmail API message id. For generic accounts, it is
the IMAP UID printed by `email list`.

## Config

Accounts are saved at:

```text
~/.config/email-cli/config.json
```

On Unix systems the file is written with `0600` permissions. This first version
stores OAuth tokens and generic account passwords in that config file, so keep
the file private.
