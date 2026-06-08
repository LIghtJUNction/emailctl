use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use clap::{Args, Parser, Subcommand};
use imap::types::Fetch;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mailparse::MailHeaderMap;
use native_tls::TlsConnector;
use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tiny_http::{Response, Server};
use url::Url;

const CONFIG_DIR: &str = "email-cli";
const CONFIG_FILE: &str = "config.json";
const GMAIL_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GMAIL_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GMAIL_API_ROOT: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const GMAIL_SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send";

#[derive(Parser, Debug)]
#[command(name = "email")]
#[command(about = "Read and send email from the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Authenticate email accounts.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// List recent messages.
    List(ListArgs),
    /// Read a message by provider id or IMAP UID.
    Read(ReadArgs),
    /// Send an email.
    Send(SendArgs),
    /// List configured accounts.
    Accounts,
    /// Make an account the default for commands.
    Use(AccountArg),
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    /// Login to an account by email address.
    Login(LoginArgs),
    /// List configured accounts.
    List,
    /// Make an account the default for commands.
    Switch(AccountArg),
    /// Show the active account.
    Whoami,
    /// Remove an account.
    Logout(AccountArg),
}

#[derive(Args, Debug)]
struct LoginArgs {
    /// Email address. The domain selects Gmail OAuth or an IMAP/SMTP provider preset.
    email: String,
    /// Google OAuth desktop/web client id. Used for Gmail accounts.
    #[arg(long, env = "GMAIL_CLIENT_ID")]
    client_id: Option<String>,
    /// Google OAuth client secret. Used for Gmail accounts.
    #[arg(long, env = "GMAIL_CLIENT_SECRET")]
    client_secret: Option<String>,
    /// Local callback port used during Gmail OAuth.
    #[arg(long, default_value_t = 8765)]
    port: u16,
    /// Print the Gmail login URL instead of opening a browser.
    #[arg(long)]
    no_browser: bool,
    /// Login username. Defaults to --email.
    #[arg(long)]
    username: Option<String>,
    /// Login password. If omitted, the CLI prompts securely.
    #[arg(long)]
    password: Option<String>,
    /// Override or provide IMAP host, for example imap.example.com.
    #[arg(long)]
    imap_host: Option<String>,
    /// Override or provide IMAP TLS port.
    #[arg(long)]
    imap_port: Option<u16>,
    /// Override or provide SMTP host, for example smtp.example.com.
    #[arg(long)]
    smtp_host: Option<String>,
    /// Override or provide SMTP STARTTLS port.
    #[arg(long)]
    smtp_port: Option<u16>,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// Account email. Defaults to the active account.
    #[arg(long)]
    account: Option<String>,
    /// Maximum number of messages.
    #[arg(short, long, default_value_t = 10)]
    limit: usize,
    /// Gmail search query. Ignored for generic IMAP accounts.
    #[arg(short, long)]
    query: Option<String>,
}

#[derive(Args, Debug)]
struct ReadArgs {
    /// Provider message id for Gmail, IMAP UID for generic accounts.
    id: String,
    /// Account email. Defaults to the active account.
    #[arg(long)]
    account: Option<String>,
}

#[derive(Args, Debug)]
struct SendArgs {
    /// Recipient email address.
    #[arg(long)]
    to: String,
    /// Email subject.
    #[arg(long)]
    subject: String,
    /// Body text. If omitted, stdin is used.
    #[arg(long)]
    body: Option<String>,
    /// Account email. Defaults to the active account.
    #[arg(long)]
    account: Option<String>,
}

#[derive(Args, Debug)]
struct AccountArg {
    /// Account email.
    account: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    active_account: Option<String>,
    #[serde(default)]
    gmail_oauth: Option<GmailOAuthConfig>,
    accounts: BTreeMap<String, AccountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GmailOAuthConfig {
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
enum AccountConfig {
    Gmail(GmailAccount),
    Generic(GenericAccount),
}

#[derive(Debug, Serialize, Deserialize)]
struct GmailAccount {
    email: String,
    client_id: String,
    client_secret: String,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct GenericAccount {
    email: String,
    username: String,
    password: String,
    imap_host: String,
    imap_port: u16,
    smtp_host: String,
    smtp_port: u16,
}

#[derive(Debug, Clone, Copy)]
struct ProviderPreset {
    imap_host: &'static str,
    imap_port: u16,
    smtp_host: &'static str,
    smtp_port: u16,
}

#[derive(Debug, Clone, Copy)]
enum LoginProvider {
    Gmail,
    Generic(Option<ProviderPreset>),
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GmailProfile {
    #[serde(rename = "emailAddress")]
    email_address: String,
}

#[derive(Debug, Deserialize)]
struct GmailListResponse {
    messages: Option<Vec<GmailMessageId>>,
}

#[derive(Debug, Deserialize)]
struct GmailMessageId {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GmailMessage {
    id: String,
    snippet: Option<String>,
    payload: Option<GmailPayload>,
}

#[derive(Debug, Deserialize)]
struct GmailPayload {
    headers: Option<Vec<GmailHeader>>,
    body: Option<GmailBody>,
    parts: Option<Vec<GmailPayload>>,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GmailHeader {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct GmailBody {
    data: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Auth { command } => handle_auth(command),
        Command::List(args) => list_messages(args),
        Command::Read(args) => read_message(args),
        Command::Send(args) => send_message(args),
        Command::Accounts => show_accounts(),
        Command::Use(arg) => use_account(arg),
    }
}

fn handle_auth(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login(args) => login_account(args),
        AuthCommand::List => show_accounts(),
        AuthCommand::Switch(arg) => use_account(arg),
        AuthCommand::Whoami => {
            let config = load_config()?;
            match config.active_account {
                Some(account) => println!("{account}"),
                None => bail!("no active account; run `email auth login <email>`"),
            }
            Ok(())
        }
        AuthCommand::Logout(arg) => logout_account(arg),
    }
}

fn login_account(args: LoginArgs) -> Result<()> {
    match provider_for_email(&args.email)? {
        LoginProvider::Gmail => login_gmail(args),
        LoginProvider::Generic(preset) => login_generic(args, preset),
    }
}

fn provider_for_email(email: &str) -> Result<LoginProvider> {
    let domain = email
        .split_once('@')
        .map(|(_, domain)| domain.to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
        .with_context(|| format!("invalid email address: {email}"))?;

    let provider = match domain.as_str() {
        "gmail.com" | "googlemail.com" => LoginProvider::Gmail,
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => {
            LoginProvider::Generic(Some(ProviderPreset {
                imap_host: "outlook.office365.com",
                imap_port: 993,
                smtp_host: "smtp.office365.com",
                smtp_port: 587,
            }))
        }
        "yahoo.com" | "ymail.com" | "rocketmail.com" => {
            LoginProvider::Generic(Some(ProviderPreset {
                imap_host: "imap.mail.yahoo.com",
                imap_port: 993,
                smtp_host: "smtp.mail.yahoo.com",
                smtp_port: 587,
            }))
        }
        "icloud.com" | "me.com" | "mac.com" => LoginProvider::Generic(Some(ProviderPreset {
            imap_host: "imap.mail.me.com",
            imap_port: 993,
            smtp_host: "smtp.mail.me.com",
            smtp_port: 587,
        })),
        "qq.com" => LoginProvider::Generic(Some(ProviderPreset {
            imap_host: "imap.qq.com",
            imap_port: 993,
            smtp_host: "smtp.qq.com",
            smtp_port: 587,
        })),
        "163.com" => LoginProvider::Generic(Some(ProviderPreset {
            imap_host: "imap.163.com",
            imap_port: 993,
            smtp_host: "smtp.163.com",
            smtp_port: 587,
        })),
        "126.com" => LoginProvider::Generic(Some(ProviderPreset {
            imap_host: "imap.126.com",
            imap_port: 993,
            smtp_host: "smtp.126.com",
            smtp_port: 587,
        })),
        _ => LoginProvider::Generic(None),
    };
    Ok(provider)
}

fn login_gmail(args: LoginArgs) -> Result<()> {
    let mut config = load_config()?;
    let oauth = resolve_gmail_oauth(&args, &mut config)?;
    save_config(&config)?;

    let redirect_uri = format!("http://127.0.0.1:{}/callback", args.port);
    let state = oauth_state();
    let mut auth_url = Url::parse(GMAIL_AUTH_URL)?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &oauth.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", GMAIL_SCOPES)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", &state);

    let server = Server::http(("127.0.0.1", args.port))
        .map_err(|err| anyhow!("failed to bind OAuth callback on port {}: {err}", args.port))?;

    println!("Open this URL to login:\n{auth_url}\n");
    if !args.no_browser {
        let _ = webbrowser::open(auth_url.as_str());
    }

    let code = wait_for_oauth_code(&server, &state)?;

    let client = Client::new();
    let token = google_json::<TokenResponse>(
        client
            .post(GMAIL_TOKEN_URL)
            .form(&[
                ("client_id", oauth.client_id.as_str()),
                ("client_secret", oauth.client_secret.as_str()),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .context("failed to exchange OAuth code")?,
        "Google OAuth token exchange",
    )?;

    let profile_email = gmail_profile_email(&client, &token.access_token)?;
    if !args.email.eq_ignore_ascii_case(&profile_email) {
        bail!(
            "browser logged in as {profile_email}, but the command requested {}; choose the matching Google account",
            args.email
        );
    }
    let email = profile_email;
    let refresh_token = token.refresh_token.context(
        "Google did not return a refresh token; retry with a fresh consent prompt or revoke the app first",
    )?;
    let expires_at = now_ts() + token.expires_in.unwrap_or(3600) - 60;

    config.accounts.insert(
        email.clone(),
        AccountConfig::Gmail(GmailAccount {
            email: email.clone(),
            client_id: oauth.client_id,
            client_secret: oauth.client_secret,
            access_token: token.access_token,
            refresh_token,
            expires_at,
        }),
    );
    config.active_account = Some(email.clone());
    save_config(&config)?;
    println!("Logged in Gmail account: {email}");
    Ok(())
}

fn resolve_gmail_oauth(args: &LoginArgs, config: &mut Config) -> Result<GmailOAuthConfig> {
    let mut client_id = args.client_id.clone().or_else(|| {
        config
            .gmail_oauth
            .as_ref()
            .map(|oauth| oauth.client_id.clone())
    });
    let mut client_secret = args.client_secret.clone().or_else(|| {
        config
            .gmail_oauth
            .as_ref()
            .map(|oauth| oauth.client_secret.clone())
    });

    if client_id.is_none() || client_secret.is_none() {
        println!("Gmail needs a Google OAuth client the first time.");
        println!("1. Enable the Gmail API:");
        println!("   https://console.cloud.google.com/apis/library/gmail.googleapis.com");
        println!("2. Create an OAuth client:");
        println!("   https://console.cloud.google.com/apis/credentials");
        println!("3. Choose type: Web application");
        println!("4. Add this Authorized redirect URI:");
        println!("   http://127.0.0.1:{}/callback\n", args.port);

        if client_id.is_none() {
            client_id = Some(prompt_nonempty("Gmail OAuth Client ID: ")?);
        }
        if client_secret.is_none() {
            client_secret = Some(rpassword::prompt_password("Gmail OAuth Client Secret: ")?);
            if client_secret
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                bail!("Gmail OAuth Client Secret cannot be empty");
            }
        }
    }

    let oauth = GmailOAuthConfig {
        client_id: client_id.context("Gmail OAuth Client ID is required")?,
        client_secret: client_secret.context("Gmail OAuth Client Secret is required")?,
    };
    config.gmail_oauth = Some(oauth.clone());
    Ok(oauth)
}

fn prompt_nonempty(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("{prompt} cannot be empty");
    }
    Ok(value)
}

fn wait_for_oauth_code(server: &Server, expected_state: &str) -> Result<String> {
    println!("Waiting for browser callback...");
    let request = server
        .recv()
        .context("failed to receive OAuth callback request")?;
    let callback = Url::parse(&format!("http://127.0.0.1{}", request.url()))
        .context("failed to parse OAuth callback URL")?;
    let pairs = callback.query_pairs();
    let code = pairs
        .clone()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string());
    let state = pairs
        .clone()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.to_string());
    let error = pairs
        .clone()
        .find(|(key, _)| key == "error")
        .map(|(_, value)| value.to_string());

    let body = if code.is_some() && state.as_deref() == Some(expected_state) {
        "Login complete. You can close this tab."
    } else {
        "Login failed. Return to the terminal for details."
    };
    let _ = request.respond(Response::from_string(body));

    if let Some(error) = error {
        bail!("OAuth error: {error}");
    }
    if state.as_deref() != Some(expected_state) {
        bail!("OAuth state mismatch");
    }
    code.context("OAuth callback did not include a code")
}

fn login_generic(args: LoginArgs, preset: Option<ProviderPreset>) -> Result<()> {
    let username = args.username.unwrap_or_else(|| args.email.clone());
    let password = match args.password {
        Some(password) => password,
        None => rpassword::prompt_password("Password: ")?,
    };

    let imap_host = args
        .imap_host
        .or_else(|| preset.map(|preset| preset.imap_host.to_string()))
        .context("unknown email provider; pass --imap-host and --smtp-host")?;
    let smtp_host = args
        .smtp_host
        .or_else(|| preset.map(|preset| preset.smtp_host.to_string()))
        .context("unknown email provider; pass --imap-host and --smtp-host")?;
    let imap_port = args
        .imap_port
        .or_else(|| preset.map(|preset| preset.imap_port))
        .unwrap_or(993);
    let smtp_port = args
        .smtp_port
        .or_else(|| preset.map(|preset| preset.smtp_port))
        .unwrap_or(587);

    let account = GenericAccount {
        email: args.email.clone(),
        username,
        password,
        imap_host,
        imap_port,
        smtp_host,
        smtp_port,
    };
    test_generic_login(&account)?;

    let mut config = load_config()?;
    config
        .accounts
        .insert(args.email.clone(), AccountConfig::Generic(account));
    config.active_account = Some(args.email.clone());
    save_config(&config)?;
    println!("Logged in generic account: {}", args.email);
    Ok(())
}

fn list_messages(args: ListArgs) -> Result<()> {
    let mut config = load_config()?;
    let account_id = resolve_account(&config, args.account)?;
    match config
        .accounts
        .get_mut(&account_id)
        .context("account not found")?
    {
        AccountConfig::Gmail(account) => list_gmail(account, args.limit, args.query),
        AccountConfig::Generic(account) => list_generic(account, args.limit),
    }
}

fn read_message(args: ReadArgs) -> Result<()> {
    let mut config = load_config()?;
    let account_id = resolve_account(&config, args.account)?;
    match config
        .accounts
        .get_mut(&account_id)
        .context("account not found")?
    {
        AccountConfig::Gmail(account) => read_gmail(account, &args.id),
        AccountConfig::Generic(account) => read_generic(account, &args.id),
    }
}

fn send_message(args: SendArgs) -> Result<()> {
    let mut body = args.body.unwrap_or_default();
    if body.is_empty() {
        std::io::read_to_string(std::io::stdin())
            .context("failed to read message body from stdin")
            .map(|stdin_body| body = stdin_body)?;
    }

    let mut config = load_config()?;
    let account_id = resolve_account(&config, args.account)?;
    match config
        .accounts
        .get_mut(&account_id)
        .context("account not found")?
    {
        AccountConfig::Gmail(account) => send_gmail(account, &args.to, &args.subject, &body),
        AccountConfig::Generic(account) => send_generic(account, &args.to, &args.subject, &body),
    }
}

fn show_accounts() -> Result<()> {
    let config = load_config()?;
    if config.accounts.is_empty() {
        println!("No accounts configured.");
        return Ok(());
    }

    for (email, account) in &config.accounts {
        let active = if config.active_account.as_ref() == Some(email) {
            "*"
        } else {
            " "
        };
        let provider = match account {
            AccountConfig::Gmail(_) => "gmail",
            AccountConfig::Generic(_) => "generic",
        };
        println!("{active} {email} ({provider})");
    }
    Ok(())
}

fn use_account(arg: AccountArg) -> Result<()> {
    let mut config = load_config()?;
    if !config.accounts.contains_key(&arg.account) {
        bail!("account not found: {}", arg.account);
    }
    config.active_account = Some(arg.account.clone());
    save_config(&config)?;
    println!("Active account: {}", arg.account);
    Ok(())
}

fn logout_account(arg: AccountArg) -> Result<()> {
    let mut config = load_config()?;
    if config.accounts.remove(&arg.account).is_none() {
        bail!("account not found: {}", arg.account);
    }
    if config.active_account.as_deref() == Some(&arg.account) {
        config.active_account = config.accounts.keys().next().cloned();
    }
    save_config(&config)?;
    println!("Removed account: {}", arg.account);
    Ok(())
}

fn list_gmail(account: &mut GmailAccount, limit: usize, query: Option<String>) -> Result<()> {
    let token = gmail_access_token(account)?;
    let client = Client::new();
    let mut request = client
        .get(format!("{GMAIL_API_ROOT}/messages"))
        .bearer_auth(&token)
        .query(&[("maxResults", limit.min(100).to_string())]);
    if let Some(query) = query {
        request = request.query(&[("q", query)]);
    }

    let list = google_json::<GmailListResponse>(
        request.send().context("failed to list Gmail messages")?,
        "Gmail list request",
    )?;

    let Some(messages) = list.messages else {
        println!("No messages.");
        return Ok(());
    };

    println!("{:<24}  {:<22}  {:<35}  Subject", "ID", "Date", "From");
    for item in messages {
        let message = gmail_message_metadata(&client, &token, &item.id)?;
        let headers = gmail_headers(message.payload.as_ref());
        println!(
            "{:<24}  {:<22}  {:<35}  {}",
            message.id,
            header_value(&headers, "Date").unwrap_or_default(),
            truncate(&header_value(&headers, "From").unwrap_or_default(), 35),
            header_value(&headers, "Subject")
                .unwrap_or_else(|| message.snippet.unwrap_or_default())
        );
    }
    Ok(())
}

fn read_gmail(account: &mut GmailAccount, id: &str) -> Result<()> {
    let token = gmail_access_token(account)?;
    let client = Client::new();
    let message = google_json::<GmailMessage>(
        client
            .get(format!("{GMAIL_API_ROOT}/messages/{id}"))
            .bearer_auth(&token)
            .query(&[("format", "full")])
            .send()
            .context("failed to read Gmail message")?,
        "Gmail read request",
    )?;

    let headers = gmail_headers(message.payload.as_ref());
    print_header("From", header_value(&headers, "From"));
    print_header("To", header_value(&headers, "To"));
    print_header("Date", header_value(&headers, "Date"));
    print_header("Subject", header_value(&headers, "Subject"));
    println!();
    if let Some(payload) = &message.payload {
        let body = gmail_body(payload).unwrap_or_else(|| message.snippet.unwrap_or_default());
        println!("{}", body.trim());
    }
    Ok(())
}

fn send_gmail(account: &mut GmailAccount, to: &str, subject: &str, body: &str) -> Result<()> {
    let token = gmail_access_token(account)?;
    let raw_message = format!(
        "From: {}\r\nTo: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}",
        account.email
    );
    let raw = URL_SAFE_NO_PAD.encode(raw_message.as_bytes());
    let response = google_json::<serde_json::Value>(
        Client::new()
            .post(format!("{GMAIL_API_ROOT}/messages/send"))
            .bearer_auth(&token)
            .json(&json!({ "raw": raw }))
            .send()
            .context("failed to send Gmail message")?,
        "Gmail send request",
    )?;

    println!(
        "Sent Gmail message: {}",
        response
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("<unknown id>")
    );
    Ok(())
}

fn gmail_message_metadata(client: &Client, token: &str, id: &str) -> Result<GmailMessage> {
    google_json::<GmailMessage>(
        client
            .get(format!("{GMAIL_API_ROOT}/messages/{id}"))
            .bearer_auth(token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "Subject"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Date"),
            ])
            .send()
            .context("failed to read Gmail metadata")?,
        "Gmail metadata request",
    )
}

fn gmail_profile_email(client: &Client, token: &str) -> Result<String> {
    let profile = google_json::<GmailProfile>(
        client
            .get(format!("{GMAIL_API_ROOT}/profile"))
            .bearer_auth(token)
            .send()
            .context("failed to read Gmail profile")?,
        "Gmail profile request",
    )?;
    Ok(profile.email_address)
}

fn google_json<T: DeserializeOwned>(
    response: reqwest::blocking::Response,
    label: &str,
) -> Result<T> {
    let status = response.status();
    let url = response.url().clone();
    let body = response
        .text()
        .with_context(|| format!("failed to read {label} response body"))?;

    if !status.is_success() {
        bail!(
            "{label} failed: {status} for {url}\n{}",
            format_google_error(&body)
        );
    }

    serde_json::from_str(&body).with_context(|| format!("failed to decode {label} response"))
}

fn format_google_error(body: &str) -> String {
    let trimmed = body.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return format!("Response body: {}", truncate(trimmed, 1200));
    };

    let error = value.get("error").unwrap_or(&value);
    let mut lines = Vec::new();
    let message = error.get("message").and_then(|value| value.as_str());
    let status = error.get("status").and_then(|value| value.as_str());
    let reason = error
        .get("errors")
        .and_then(|value| value.as_array())
        .and_then(|errors| errors.first())
        .and_then(|error| error.get("reason"))
        .and_then(|value| value.as_str());

    if let Some(message) = message {
        lines.push(format!("Google message: {message}"));
    }
    if let Some(status) = status {
        lines.push(format!("Google status: {status}"));
    }
    if let Some(reason) = reason {
        lines.push(format!("Google reason: {reason}"));
    }

    let details = [message, status, reason]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    let lower = details.to_ascii_lowercase();
    if lower.contains("api has not been used")
        || lower.contains("disabled")
        || lower.contains("accessnotconfigured")
        || lower.contains("service_disabled")
    {
        lines.extend(gmail_api_enable_steps());
    } else if lower.contains("insufficient")
        || lower.contains("permission")
        || lower.contains("scope")
    {
        lines.extend(gmail_scope_fix_steps());
    } else if lower.contains("forbidden") || lower.contains("gmail") {
        lines.extend(gmail_forbidden_fix_steps());
    }

    if lines.is_empty() {
        format!("Response body: {}", truncate(trimmed, 1200))
    } else {
        lines.join("\n")
    }
}

fn gmail_api_enable_steps() -> Vec<String> {
    vec![
        "Fix: enable Gmail API for the same Google Cloud project as this OAuth client.".to_string(),
        "1. Open: https://console.cloud.google.com/apis/library/gmail.googleapis.com".to_string(),
        "2. Select the project that owns your OAuth Client ID.".to_string(),
        "3. Click Enable, wait 1-2 minutes, then run `email auth login <gmail-address>` again."
            .to_string(),
    ]
}

fn gmail_scope_fix_steps() -> Vec<String> {
    vec![
        "Fix: refresh the Gmail OAuth grant with the required read/send scopes.".to_string(),
        "1. Open: https://myaccount.google.com/permissions".to_string(),
        "2. Remove this OAuth app if it is already listed.".to_string(),
        "3. Run `email auth login <gmail-address>` again and approve Gmail read/send access."
            .to_string(),
        "4. If your OAuth app is in Testing, also add your Gmail as a test user: https://console.cloud.google.com/apis/credentials/consent".to_string(),
    ]
}

fn gmail_forbidden_fix_steps() -> Vec<String> {
    vec![
        "Fix: check Gmail account access and OAuth consent configuration.".to_string(),
        "1. Confirm the signed-in Google account has Gmail enabled: https://mail.google.com/".to_string(),
        "2. If the OAuth app is in Testing, add this Gmail as a test user: https://console.cloud.google.com/apis/credentials/consent".to_string(),
        "3. Confirm Gmail API is enabled in the same project: https://console.cloud.google.com/apis/library/gmail.googleapis.com".to_string(),
    ]
}

fn gmail_access_token(account: &mut GmailAccount) -> Result<String> {
    if account.expires_at > now_ts() {
        return Ok(account.access_token.clone());
    }

    let token = google_json::<TokenResponse>(
        Client::new()
            .post(GMAIL_TOKEN_URL)
            .form(&[
                ("client_id", account.client_id.as_str()),
                ("client_secret", account.client_secret.as_str()),
                ("refresh_token", account.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .context("failed to refresh Gmail access token")?,
        "Gmail token refresh",
    )?;

    account.access_token = token.access_token.clone();
    account.expires_at = now_ts() + token.expires_in.unwrap_or(3600) - 60;

    let mut config = load_config()?;
    if let Some(AccountConfig::Gmail(saved)) = config.accounts.get_mut(&account.email) {
        saved.access_token = account.access_token.clone();
        saved.expires_at = account.expires_at;
        save_config(&config)?;
    }
    Ok(account.access_token.clone())
}

fn gmail_headers(payload: Option<&GmailPayload>) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    if let Some(payload) = payload
        && let Some(headers) = &payload.headers
    {
        for header in headers {
            result.insert(header.name.to_ascii_lowercase(), header.value.clone());
        }
    }
    result
}

fn gmail_body(payload: &GmailPayload) -> Option<String> {
    if payload.mime_type.as_deref() == Some("text/plain")
        && let Some(data) = payload.body.as_ref().and_then(|body| body.data.as_ref())
    {
        return decode_gmail_body(data).ok();
    }
    if let Some(parts) = &payload.parts {
        for part in parts {
            if let Some(body) = gmail_body(part) {
                return Some(body);
            }
        }
        for part in parts {
            if part.mime_type.as_deref() == Some("text/html")
                && let Some(data) = part.body.as_ref().and_then(|body| body.data.as_ref())
            {
                return decode_gmail_body(data).ok();
            }
        }
    }
    payload
        .body
        .as_ref()
        .and_then(|body| body.data.as_ref())
        .and_then(|data| decode_gmail_body(data).ok())
}

fn decode_gmail_body(data: &str) -> Result<String> {
    let decoded = URL_SAFE_NO_PAD
        .decode(data)
        .or_else(|_| STANDARD.decode(data))
        .context("failed to base64 decode Gmail body")?;
    Ok(String::from_utf8_lossy(&decoded).to_string())
}

fn test_generic_login(account: &GenericAccount) -> Result<()> {
    let tls = TlsConnector::builder().build()?;
    let client = imap::connect(
        (account.imap_host.as_str(), account.imap_port),
        account.imap_host.as_str(),
        &tls,
    )
    .context("failed to connect to IMAP server")?;
    let mut session = client
        .login(&account.username, &account.password)
        .map_err(|err| anyhow!("IMAP login failed: {}", err.0))?;
    session.logout().ok();
    Ok(())
}

fn list_generic(account: &GenericAccount, limit: usize) -> Result<()> {
    let mut session = generic_imap_session(account)?;
    session.select("INBOX").context("failed to select INBOX")?;
    let mut uids: Vec<u32> = session
        .uid_search("ALL")
        .context("failed to search IMAP mailbox")?
        .into_iter()
        .collect();
    uids.sort_unstable();
    uids.reverse();
    uids.truncate(limit);

    if uids.is_empty() {
        println!("No messages.");
        session.logout().ok();
        return Ok(());
    }

    let uid_set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let messages = session
        .uid_fetch(uid_set, "RFC822.HEADER")
        .context("failed to fetch IMAP headers")?;
    println!("{:<12}  {:<22}  {:<35}  Subject", "UID", "Date", "From");
    for fetch in messages.iter() {
        print_imap_summary(fetch)?;
    }
    session.logout().ok();
    Ok(())
}

fn read_generic(account: &GenericAccount, uid: &str) -> Result<()> {
    let mut session = generic_imap_session(account)?;
    session.select("INBOX").context("failed to select INBOX")?;
    let messages = session
        .uid_fetch(uid, "RFC822")
        .context("failed to fetch IMAP message")?;
    let fetch = messages
        .iter()
        .next()
        .context("message not found by IMAP UID")?;
    let body = fetch
        .body()
        .context("IMAP response did not include a body")?;
    let parsed = mailparse::parse_mail(body).context("failed to parse email")?;
    print_header("From", parsed.headers.get_first_value("From"));
    print_header("To", parsed.headers.get_first_value("To"));
    print_header("Date", parsed.headers.get_first_value("Date"));
    print_header("Subject", parsed.headers.get_first_value("Subject"));
    println!();
    println!("{}", parsed_body(&parsed).trim());
    session.logout().ok();
    Ok(())
}

fn send_generic(account: &GenericAccount, to: &str, subject: &str, body: &str) -> Result<()> {
    let from: Mailbox = account.email.parse().context("invalid from email")?;
    let to: Mailbox = to.parse().context("invalid recipient email")?;
    let message = Message::builder()
        .from(from)
        .to(to)
        .subject(subject)
        .body(body.to_string())
        .context("failed to build SMTP message")?;
    let credentials = Credentials::new(account.username.clone(), account.password.clone());
    let mailer = SmtpTransport::starttls_relay(&account.smtp_host)
        .context("failed to configure SMTP relay")?
        .port(account.smtp_port)
        .credentials(credentials)
        .build();
    mailer
        .send(&message)
        .context("failed to send message through SMTP")?;
    println!("Sent message via {}", account.smtp_host);
    Ok(())
}

fn generic_imap_session(
    account: &GenericAccount,
) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>> {
    let tls = TlsConnector::builder().build()?;
    let client = imap::connect(
        (account.imap_host.as_str(), account.imap_port),
        account.imap_host.as_str(),
        &tls,
    )
    .context("failed to connect to IMAP server")?;
    client
        .login(&account.username, &account.password)
        .map_err(|err| anyhow!("IMAP login failed: {}", err.0))
}

fn print_imap_summary(fetch: &Fetch) -> Result<()> {
    let uid = fetch.uid.context("IMAP response did not include UID")?;
    let body = fetch
        .body()
        .context("IMAP response did not include message headers")?;
    let parsed = mailparse::parse_mail(body).context("failed to parse IMAP headers")?;
    println!(
        "{:<12}  {:<22}  {:<35}  {}",
        uid,
        parsed.headers.get_first_value("Date").unwrap_or_default(),
        truncate(
            &parsed.headers.get_first_value("From").unwrap_or_default(),
            35
        ),
        parsed
            .headers
            .get_first_value("Subject")
            .unwrap_or_default()
    );
    Ok(())
}

fn parsed_body(mail: &mailparse::ParsedMail<'_>) -> String {
    if mail.subparts.is_empty() {
        return mail.get_body().unwrap_or_default();
    }

    for part in &mail.subparts {
        if part.ctype.mimetype.eq_ignore_ascii_case("text/plain") {
            return part.get_body().unwrap_or_default();
        }
    }
    for part in &mail.subparts {
        let body = parsed_body(part);
        if !body.trim().is_empty() {
            return body;
        }
    }
    String::new()
}

fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_config(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir {}", parent.display()))?;
    }
    let contents = serde_json::to_string_pretty(config)?;
    write_private_file(&path, contents.as_bytes())?;
    Ok(())
}

#[cfg(unix)]
fn write_private_file(path: &PathBuf, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to write config {}", path.display()))?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &PathBuf, contents: &[u8]) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write config {}", path.display()))
}

fn config_path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("could not find user config directory")?;
    Ok(base.join(CONFIG_DIR).join(CONFIG_FILE))
}

fn resolve_account(config: &Config, account: Option<String>) -> Result<String> {
    match account.or_else(|| config.active_account.clone()) {
        Some(account) => Ok(account),
        None => {
            bail!("no active account; run `email auth login <email>`")
        }
    }
}

fn header_value(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    headers.get(&name.to_ascii_lowercase()).cloned()
}

fn print_header(name: &str, value: Option<String>) {
    if let Some(value) = value {
        println!("{name}: {value}");
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn oauth_state() -> String {
    format!("email-cli-{}", now_ts())
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_domains_use_browser_oauth() {
        assert!(matches!(
            provider_for_email(&format!("user{}gmail.com", '@')).unwrap(),
            LoginProvider::Gmail
        ));
        assert!(matches!(
            provider_for_email(&format!("user{}googlemail.com", '@')).unwrap(),
            LoginProvider::Gmail
        ));
    }

    #[test]
    fn common_domains_use_presets() {
        let LoginProvider::Generic(Some(preset)) =
            provider_for_email(&format!("user{}outlook.com", '@')).unwrap()
        else {
            panic!("outlook should use a preset");
        };
        assert_eq!(preset.imap_host, "outlook.office365.com");
        assert_eq!(preset.smtp_host, "smtp.office365.com");
    }

    #[test]
    fn custom_domains_need_manual_servers() {
        assert!(matches!(
            provider_for_email(&format!("user{}example.invalid", '@')).unwrap(),
            LoginProvider::Generic(None)
        ));
    }

    #[test]
    fn invalid_email_is_rejected() {
        assert!(provider_for_email("not-an-email").is_err());
    }

    #[test]
    fn google_errors_show_reason_and_setup_link() {
        let error = format_google_error(
            r#"{
              "error": {
                "code": 403,
                "message": "Gmail API has not been used in project 123 before or it is disabled.",
                "status": "PERMISSION_DENIED",
                "errors": [
                  { "reason": "accessNotConfigured" }
                ]
              }
            }"#,
        );

        assert!(error.contains("Gmail API has not been used"));
        assert!(error.contains("accessNotConfigured"));
        assert!(
            error.contains("https://console.cloud.google.com/apis/library/gmail.googleapis.com")
        );
        assert!(error.contains("Click Enable"));
    }

    #[test]
    fn scope_errors_show_reauth_link() {
        let error = format_google_error(
            r#"{
              "error": {
                "code": 403,
                "message": "Request had insufficient authentication scopes.",
                "status": "PERMISSION_DENIED"
              }
            }"#,
        );

        assert!(error.contains("https://myaccount.google.com/permissions"));
        assert!(error.contains("approve Gmail read/send access"));
    }
}
