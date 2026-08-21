//! Codex subscription transport for the desktop chat.
//!
//! This intentionally only replaces vcad's hosted model transport. The
//! frontend still owns the CAD system prompt, tool schemas, tool execution,
//! and the tool-result loop. Authentication is reused from the installed
//! Codex CLI, in the same spirit as OpenCode's ChatGPT subscription adapter:
//! access tokens never enter the webview and requests are sent directly from
//! the native process to the Codex Responses endpoint.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::State;
use tokio::sync::{oneshot, Mutex};

const CODEX_API_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const OAUTH_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct CodexState {
    active: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

impl CodexState {
    pub fn new() -> Self {
        Self {
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for CodexState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAuthStatus {
    available: bool,
    logged_in: bool,
    account_id: Option<String>,
    message: Option<String>,
}

#[derive(Clone)]
struct CodexCredentials {
    access_token: String,
    refresh_token: String,
    account_id: String,
    residency: Option<String>,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum CodexChatEvent {
    Line { line: String },
    Done,
    Error { message: String },
}

fn codex_auth_path() -> Result<PathBuf, String> {
    if let Some(root) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(root).join("auth.json"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "cannot locate Codex credentials: HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".codex").join("auth.json"))
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn account_id_from_claims(claims: &Value) -> Option<String> {
    claims
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(Value::as_array)
                .and_then(|orgs| orgs.first())
                .and_then(|org| org.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn residency_from_claims(claims: &Value) -> Option<String> {
    let residency = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_compute_residency")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .get("chatgpt_compute_residency")
                .and_then(Value::as_str)
        })?;
    (residency != "no_constraint").then(|| residency.to_owned())
}

fn load_credentials() -> Result<(PathBuf, Value, CodexCredentials), String> {
    let path = codex_auth_path()?;
    let text = fs::read_to_string(&path).map_err(|_| {
        format!(
            "Codex is not signed in. Run `codex login` and restart vcad (looked for {}).",
            path.display()
        )
    })?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|e| format!("could not read Codex credentials: {e}"))?;
    if document.get("auth_mode").and_then(Value::as_str) != Some("chatgpt") {
        return Err("Codex is not using ChatGPT authentication. Run `codex login`.".into());
    }
    let tokens = document
        .get("tokens")
        .and_then(Value::as_object)
        .ok_or_else(|| "Codex credentials do not contain ChatGPT tokens".to_string())?;
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Codex credentials are missing an access token".to_string())?
        .to_owned();
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Codex credentials are missing a refresh token".to_string())?
        .to_owned();
    let claims = jwt_claims(&access_token);
    let account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| claims.as_ref().and_then(account_id_from_claims))
        .ok_or_else(|| "Codex credentials are missing a ChatGPT account id".to_string())?;
    let residency = claims.as_ref().and_then(residency_from_claims);

    Ok((
        path,
        document,
        CodexCredentials {
            access_token,
            refresh_token,
            account_id,
            residency,
        },
    ))
}

#[tauri::command]
pub fn codex_auth_status() -> CodexAuthStatus {
    match load_credentials() {
        Ok((_, _, credentials)) => CodexAuthStatus {
            available: true,
            logged_in: true,
            account_id: Some(credentials.account_id),
            message: None,
        },
        Err(message) => CodexAuthStatus {
            available: codex_auth_path().is_ok_and(|path| path.exists()),
            logged_in: false,
            account_id: None,
            message: Some(message),
        },
    }
}

fn write_auth_document(path: &Path, document: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "invalid Codex credential path".to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(
        ".auth.json.vcad.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let bytes = serde_json::to_vec_pretty(document)
        .map_err(|e| format!("could not serialize refreshed Codex credentials: {e}"))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|e| format!("could not stage refreshed Codex credentials: {e}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("could not write refreshed Codex credentials: {e}"))?;
    fs::rename(&temp, path)
        .map_err(|e| format!("could not save refreshed Codex credentials: {e}"))?;
    Ok(())
}

async fn refresh_credentials(
    client: &reqwest::Client,
    path: &Path,
    mut document: Value,
    previous: &CodexCredentials,
) -> Result<CodexCredentials, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", &previous.refresh_token)
        .append_pair("client_id", CODEX_CLIENT_ID)
        .finish();
    let response = client
        .post(OAUTH_TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Codex token refresh failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Codex login expired (token refresh returned {}). Run `codex login`.",
            response.status()
        ));
    }
    let refreshed: RefreshResponse = response
        .json()
        .await
        .map_err(|e| format!("invalid Codex token refresh response: {e}"))?;
    let refresh_token = refreshed
        .refresh_token
        .unwrap_or_else(|| previous.refresh_token.clone());
    let claims = jwt_claims(&refreshed.access_token);
    let account_id = claims
        .as_ref()
        .and_then(account_id_from_claims)
        .unwrap_or_else(|| previous.account_id.clone());
    let residency = claims
        .as_ref()
        .and_then(residency_from_claims)
        .or_else(|| previous.residency.clone());

    let tokens = document
        .get_mut("tokens")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "Codex credential document lost its tokens object".to_string())?;
    tokens.insert(
        "access_token".into(),
        Value::String(refreshed.access_token.clone()),
    );
    tokens.insert("refresh_token".into(), Value::String(refresh_token.clone()));
    tokens.insert("account_id".into(), Value::String(account_id.clone()));
    if let Some(id_token) = refreshed.id_token {
        tokens.insert("id_token".into(), Value::String(id_token));
    }
    if let Some(root) = document.as_object_mut() {
        root.insert(
            "last_refresh".into(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
    }
    write_auth_document(path, &document)?;

    Ok(CodexCredentials {
        access_token: refreshed.access_token,
        refresh_token,
        account_id,
        residency,
    })
}

fn request_builder(
    client: &reqwest::Client,
    body: &Value,
    session_id: &str,
    credentials: &CodexCredentials,
) -> reqwest::RequestBuilder {
    let mut request = client
        .post(CODEX_API_ENDPOINT)
        .bearer_auth(&credentials.access_token)
        .header("ChatGPT-Account-Id", &credentials.account_id)
        .header("originator", "vcad")
        .header(
            "User-Agent",
            format!(
                "vcad/{} ({})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS
            ),
        )
        .header("session_id", session_id)
        .header("Accept", "text/event-stream")
        .json(body);
    if let Some(residency) = &credentials.residency {
        request = request.header("x-openai-internal-codex-residency", residency);
    }
    request
}

async fn response_error(response: reqwest::Response) -> String {
    let status = response.status();
    let mut text = response.text().await.unwrap_or_default();
    if text.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    if text.trim().is_empty() {
        format!("Codex request failed with HTTP {status}")
    } else {
        format!("Codex request failed with HTTP {status}: {text}")
    }
}

#[tauri::command]
pub async fn codex_chat_cancel(
    request_id: String,
    state: State<'_, CodexState>,
) -> Result<(), String> {
    if let Some(cancel) = state.active.lock().await.remove(&request_id) {
        let _ = cancel.send(());
    }
    Ok(())
}

#[tauri::command]
pub async fn codex_chat_stream(
    request_id: String,
    session_id: String,
    mut body: Value,
    on_event: Channel<CodexChatEvent>,
    state: State<'_, CodexState>,
) -> Result<(), String> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err("invalid Codex request id".into());
    }
    if session_id.is_empty() || session_id.len() > 128 {
        return Err("invalid Codex session id".into());
    }
    let object = body
        .as_object_mut()
        .ok_or_else(|| "Codex request body must be an object".to_string())?;
    object.insert("stream".into(), Value::Bool(true));
    object.insert("store".into(), Value::Bool(false));
    let request_size = serde_json::to_vec(&body)
        .map_err(|e| format!("invalid Codex request body: {e}"))?
        .len();
    if request_size > MAX_REQUEST_BYTES {
        return Err(format!(
            "Codex request is too large ({request_size} bytes; max {MAX_REQUEST_BYTES})"
        ));
    }

    let (auth_path, auth_document, mut credentials) = load_credentials()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10 * 60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = request_builder(&client, &body, &session_id, &credentials)
        .send()
        .await
        .map_err(|e| format!("could not reach Codex: {e}"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        credentials = refresh_credentials(&client, &auth_path, auth_document, &credentials).await?;
        response = request_builder(&client, &body, &session_id, &credentials)
            .send()
            .await
            .map_err(|e| format!("could not reach Codex after refreshing login: {e}"))?;
    }
    if !response.status().is_success() {
        let message = response_error(response).await;
        let _ = on_event.send(CodexChatEvent::Error {
            message: message.clone(),
        });
        return Err(message);
    }

    let (cancel_tx, mut cancel_rx) = oneshot::channel();
    {
        let mut active = state.active.lock().await;
        if active.insert(request_id.clone(), cancel_tx).is_some() {
            return Err("duplicate Codex request id".into());
        }
    }

    let mut buffer = Vec::<u8>::new();
    let result = 'stream: loop {
        tokio::select! {
            _ = &mut cancel_rx => break Ok(()),
            chunk = response.chunk() => {
                match chunk {
                    Ok(Some(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                            let mut line = buffer.drain(..=newline).collect::<Vec<_>>();
                            if line.last() == Some(&b'\n') { line.pop(); }
                            if line.last() == Some(&b'\r') { line.pop(); }
                            if line.is_empty() { continue; }
                            let line = String::from_utf8(line)
                                .map_err(|_| "Codex stream contained invalid UTF-8".to_string());
                            match line {
                                Ok(line) => { let _ = on_event.send(CodexChatEvent::Line { line }); }
                                Err(message) => break 'stream Err(message),
                            }
                        }
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(format!("Codex stream failed: {error}")),
                }
            }
        }
    };

    state.active.lock().await.remove(&request_id);
    match result {
        Ok(()) => {
            let _ = on_event.send(CodexChatEvent::Done);
            Ok(())
        }
        Err(message) => {
            let _ = on_event.send(CodexChatEvent::Error {
                message: message.clone(),
            });
            Err(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{account_id_from_claims, residency_from_claims};
    use serde_json::json;

    #[test]
    fn extracts_nested_chatgpt_claims() {
        let claims = json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_123",
                "chatgpt_compute_residency": "eu"
            }
        });
        assert_eq!(account_id_from_claims(&claims).as_deref(), Some("acct_123"));
        assert_eq!(residency_from_claims(&claims).as_deref(), Some("eu"));
    }

    #[test]
    fn ignores_unconstrained_residency() {
        let claims = json!({ "chatgpt_compute_residency": "no_constraint" });
        assert_eq!(residency_from_claims(&claims), None);
    }
}
