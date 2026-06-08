# email-cli

`email-cli` is a Rust terminal email client. The installed binary is `email`.

It logs in by email address: Gmail domains use browser OAuth, and supported
non-Gmail domains use IMAP/SMTP presets.

## Install

```sh
cargo install email-cli-lightjunction
```

For local development:

```sh
cargo run -- --help
```

## Login

For Gmail, login with the Gmail address. The CLI detects the domain, opens a
browser, receives the OAuth callback, reads the Gmail profile, and saves the
account automatically:

```sh
email auth login "$GMAIL_ADDRESS"
```

The first Gmail login needs a Google OAuth client. If one is not already saved,
the CLI prompts for the client id and secret, then stores them in the config for
future logins. You can also prefill them with environment variables:

1. Enable the Gmail API:
   `https://console.cloud.google.com/apis/library/gmail.googleapis.com`
2. Create an OAuth client:
   `https://console.cloud.google.com/apis/credentials`
3. Choose `Web application`.
4. Add `http://127.0.0.1:8765/callback` as an authorized redirect URI.

```sh
export GMAIL_CLIENT_ID=...
export GMAIL_CLIENT_SECRET=...
email auth login "$GMAIL_ADDRESS"
```

For common non-Gmail domains, the CLI selects an IMAP/SMTP preset from the
email domain and prompts for the password:

```sh
email auth login "$EMAIL_ADDRESS"
```

For custom domains or providers without a preset, provide the servers:

```sh
email auth login "$EMAIL_ADDRESS" \
  --imap-host imap.example.com \
  --smtp-host smtp.example.com
```

If `--password` is omitted, the CLI prompts for it securely.

## Gmail Errors

When Google returns an error, `email` prints the Google message plus setup links.
Common fixes include:

```text
Enable Gmail API:
https://console.cloud.google.com/apis/library/gmail.googleapis.com

OAuth consent and test users:
https://console.cloud.google.com/apis/credentials/consent

Remove an old OAuth grant before retrying:
https://myaccount.google.com/permissions
```

## Usage

```sh
email accounts
email auth list
email auth whoami
email list --limit 10
email list --query "from:github newer_than:7d"
email read MESSAGE_ID
email send --to "$TO_ADDRESS" --subject "Hello" --body "Hi from email-cli"
printf 'body from stdin\n' | email send --to "$TO_ADDRESS" --subject "Hello"
email use "$EMAIL_ADDRESS"
email auth switch "$EMAIL_ADDRESS"
email auth logout "$EMAIL_ADDRESS"
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
