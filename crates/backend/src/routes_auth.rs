//! Auth HTTP handlers: setup-status, setup, login, logout, me,
//! change-password.
//!
//! Session transport (Task 2):
//! - The `pm_session` cookie is the sole session carrier returned to the
//!   browser. It is `HttpOnly` and, unless `PUMPKIN_INSECURE_COOKIES=1`,
//!   `Secure`. The raw token is **not** included in the response body.
//! - A readable `pm_csrf` cookie is issued alongside it; unsafe cookie-
//!   authenticated mutations must echo it in `X-CSRF-Token` (enforced in
//!   [`crate::auth::auth_middleware`]).
//! - `/auth/setup` and `/auth/login` are public (no session yet) so they use a
//!   same-origin `Origin`/`Referer` check plus an in-process token-bucket
//!   limiter instead.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{AppendHeaders, IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::auth::{self, AuthUser};

const SESSION_COOKIE: &str = "pm_session";
const CSRF_COOKIE: &str = "pm_csrf";

const MIN_PASSWORD_LEN: usize = 8;
const MAX_PASSWORD_LEN: usize = 1024;
const MAX_USERNAME_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Request/response models
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Serialize)]
pub struct SetupStatusResponse {
    /// False once the first owner exists. Frontend gates /setup on this.
    pub needs_setup: bool,
}

#[derive(Debug, Serialize)]
pub struct SessionUser {
    pub id: String,
    pub username: String,
    pub role: String,
}

/// Login/setup 200 body. Deliberately carries no token: the session lives in
/// the `pm_session` cookie (frozen contract for the SPA, Task 10).
#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub user: SessionUser,
}

// ---------------------------------------------------------------------------
// Peer address (rate-limit fallback key)
// ---------------------------------------------------------------------------

/// Optional peer address pulled from `ConnectInfo`, which the production
/// server installs via `into_make_service_with_connect_info`. Absent in tests
/// and behind some proxies, where the key falls back to a shared bucket.
pub struct PeerAddr(pub Option<SocketAddr>);

#[axum::async_trait]
impl<S> FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let addr = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        Ok(PeerAddr(addr))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/auth/setup-status — public, reports first-run state.
#[allow(dead_code)]
pub async fn setup_status(State(app): State<crate::AppState>) -> impl IntoResponse {
    let pool: SqlitePool = app.pool.clone();
    let needs_setup = auth::needs_setup(&pool).await.unwrap_or(false);
    Json(SetupStatusResponse { needs_setup })
}

/// POST /api/v1/auth/setup — creates the first `owner` account.
///
/// One-shot and atomic: the insert only fires while `users` is empty **and**
/// `setup_complete` is unset, inside a transaction that also persists the flag.
/// Only the caller whose insert affects one row becomes owner; everyone else
/// gets `409 SETUP_ALREADY_COMPLETE`.
#[allow(dead_code)]
pub async fn setup(
    State(app): State<crate::AppState>,
    PeerAddr(peer): PeerAddr,
    headers: HeaderMap,
    Json(body): Json<SetupRequest>,
) -> Response {
    if !same_origin_ok(&headers) {
        return error(StatusCode::FORBIDDEN, "CSRF_FAILED");
    }
    // Rate-limit first: junk payloads must consume a token too.
    if let Err(retry) = check_rate_limit(
        &app,
        &[client_key(peer.as_ref()), user_key("setup", &body.username)],
    ) {
        return rate_limited(retry);
    }
    let username = match validate_username(&body.username) {
        Ok(u) => u,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    if let Err(code) = validate_password(&body.password, true) {
        return error(StatusCode::BAD_REQUEST, code);
    }

    let pool: SqlitePool = app.pool.clone();
    if !auth::needs_setup(&pool).await.unwrap_or(false) {
        return error(StatusCode::CONFLICT, "SETUP_ALREADY_COMPLETE");
    }
    let password_hash = match auth::hash_password(&body.password) {
        Ok(h) => h,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();

    // Single transaction: the guarded INSERT and the persistent flag either
    // both land or neither does. A concurrent loser inserts zero rows.
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    };
    let inserted = sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, is_active, created_at, updated_at)
         SELECT ?, ?, ?, 'owner', 1, ?, ?
         WHERE NOT EXISTS (SELECT 1 FROM users)
           AND NOT EXISTS (SELECT 1 FROM app_settings WHERE key = 'setup_complete')",
    )
    .bind(&id)
    .bind(&username)
    .bind(&password_hash)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await;
    let inserted = match inserted {
        Ok(r) => r,
        Err(_) => {
            let _ = tx.rollback().await;
            return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL");
        }
    };
    if inserted.rows_affected() != 1 {
        let _ = tx.rollback().await;
        return error(StatusCode::CONFLICT, "SETUP_ALREADY_COMPLETE");
    }
    let flag = sqlx::query(
        "INSERT INTO app_settings (key, value, updated_at) VALUES ('setup_complete', '1', ?)
         ON CONFLICT(key) DO NOTHING",
    )
    .bind(&now)
    .execute(&mut *tx)
    .await;
    if flag.is_err() || tx.commit().await.is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL");
    }

    match issue_session(&pool, &id, app.session_ttl_secs).await {
        Ok(token) => {
            let user = SessionUser {
                id: id.clone(),
                username: username.clone(),
                role: "owner".to_string(),
            };
            session_response(app.session_ttl_secs, app.secure_cookies, &token, user)
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    }
}

/// POST /api/v1/auth/login — verifies credentials and issues a session.
///
/// Throttled per client and per submitted username. Unknown usernames run the
/// dummy Argon2id hash so response timing does not reveal existence.
#[allow(dead_code)]
pub async fn login(
    State(app): State<crate::AppState>,
    PeerAddr(peer): PeerAddr,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    if !same_origin_ok(&headers) {
        return error(StatusCode::FORBIDDEN, "CSRF_FAILED");
    }
    // Rate-limit first: junk payloads must consume a token too.
    if let Err(retry) = check_rate_limit(
        &app,
        &[client_key(peer.as_ref()), user_key("login", &body.username)],
    ) {
        return rate_limited(retry);
    }
    let username = match validate_username(&body.username) {
        Ok(u) => u,
        Err(_) => return error(StatusCode::BAD_REQUEST, "INVALID_CREDENTIALS"),
    };
    if validate_password(&body.password, false).is_err() {
        return error(StatusCode::BAD_REQUEST, "INVALID_CREDENTIALS");
    }

    let pool: SqlitePool = app.pool.clone();
    let row = sqlx::query("SELECT id, password_hash, is_active FROM users WHERE username = ?")
        .bind(&username)
        .fetch_optional(&pool)
        .await;
    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            // Equalize timing with a real verification before the generic error.
            auth::verify_dummy(&body.password);
            return error(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS");
        }
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    };
    use sqlx::Row as _;
    let user_id: String = row.get("id");
    let stored_hash: String = row.get("password_hash");
    let is_active: i64 = row.get("is_active");
    if is_active == 0 {
        return error(StatusCode::FORBIDDEN, "ACCOUNT_DISABLED");
    }
    if !auth::verify_password(&body.password, &stored_hash).unwrap_or(false) {
        return error(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS");
    }
    // A legitimate login clears the username bucket (typos forgiven).
    reset_rate_limit(&app, &user_key("login", &username));
    let user_row: Option<(String, String)> =
        sqlx::query_as("SELECT username, role FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_optional(&pool)
            .await
            .unwrap_or(None);
    let Some((username, role)) = user_row else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL");
    };
    let now = now_rfc3339();
    let _ = sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&user_id)
        .execute(&pool)
        .await;
    match issue_session(&pool, &user_id, app.session_ttl_secs).await {
        Ok(token) => {
            let user = SessionUser {
                id: user_id.clone(),
                username,
                role,
            };
            session_response(app.session_ttl_secs, app.secure_cookies, &token, user)
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    }
}

/// POST /api/v1/auth/logout — revokes the calling session and clears both
/// session cookies.
#[allow(dead_code)]
pub async fn logout(State(app): State<crate::AppState>, headers: HeaderMap) -> Response {
    let pool: SqlitePool = app.pool.clone();
    let Some(token) = auth::extract_token(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };
    let token_hash = auth::hash_token(&token);
    let now = now_rfc3339();
    let _ = sqlx::query(
        "UPDATE sessions SET revoked_at = ? WHERE token_hash = ? AND revoked_at IS NULL",
    )
    .bind(&now)
    .bind(&token_hash)
    .execute(&pool)
    .await;
    clear_session_cookies(app.secure_cookies)
}

/// GET /api/v1/auth/me — returns the authenticated user.
#[allow(dead_code)]
pub async fn me(auth_user: AuthUser) -> impl IntoResponse {
    Json(auth_user)
}

/// POST /api/v1/auth/change-password — verifies current password, rotates
/// to the new hash, and revokes all other sessions for the account.
#[allow(dead_code)]
pub async fn change_password(
    State(app): State<crate::AppState>,
    auth_user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordRequest>,
) -> Response {
    let pool: SqlitePool = app.pool.clone();
    if body.new_password.len() < MIN_PASSWORD_LEN || body.new_password.len() > MAX_PASSWORD_LEN {
        return error(StatusCode::BAD_REQUEST, "WEAK_PASSWORD");
    }
    if body.current_password.len() > MAX_PASSWORD_LEN {
        return error(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS");
    }
    let row = sqlx::query("SELECT password_hash FROM users WHERE id = ?")
        .bind(&auth_user.id)
        .fetch_optional(&pool)
        .await;
    use sqlx::Row as _;
    let stored_hash: Option<String> = match row {
        Ok(Some(r)) => Some(r.get("password_hash")),
        Ok(None) => None,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    };
    let Some(stored_hash) = stored_hash else {
        return error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED");
    };
    if !auth::verify_password(&body.current_password, &stored_hash).unwrap_or(false) {
        return error(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS");
    }
    let new_hash = match auth::hash_password(&body.new_password) {
        Ok(h) => h,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    };
    let now = now_rfc3339();
    let updated = sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(&now)
        .bind(&auth_user.id)
        .execute(&pool)
        .await;
    if updated.is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL");
    }
    // Revoke every session except the one making this call.
    let current_hash = auth::extract_token(&headers).map(|t| auth::hash_token(&t));
    let revoked = match current_hash {
        Some(h) => {
            sqlx::query(
                "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND token_hash != ? AND revoked_at IS NULL",
            )
            .bind(&now)
            .bind(&auth_user.id)
            .bind(&h)
            .execute(&pool)
            .await
        }
        None => {
            sqlx::query(
                "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
            )
            .bind(&now)
            .bind(&auth_user.id)
            .execute(&pool)
            .await
        }
    };
    if revoked.is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL");
    }
    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Inserts a session row (TTL from config) and returns the raw token.
async fn issue_session(
    pool: &SqlitePool,
    user_id: &str,
    ttl_secs: i64,
) -> Result<String, sqlx::Error> {
    let token = auth::generate_session_token();
    let token_hash = auth::hash_token(&token);
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let expires_at = (now + chrono::Duration::seconds(ttl_secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(&token_hash)
    .bind(&created_at)
    .bind(&expires_at)
    .execute(pool)
    .await?;
    Ok(token)
}

/// 200 JSON `{user}` plus `pm_session` (HttpOnly) and `pm_csrf` cookies.
fn session_response(ttl_secs: i64, secure: bool, token: &str, user: SessionUser) -> Response {
    let csrf = auth::generate_session_token();
    (
        StatusCode::OK,
        AppendHeaders([
            (
                header::SET_COOKIE,
                build_cookie(SESSION_COOKIE, token, ttl_secs, secure, true),
            ),
            (
                header::SET_COOKIE,
                build_cookie(CSRF_COOKIE, &csrf, ttl_secs, secure, false),
            ),
        ]),
        Json(SessionResponse { user }),
    )
        .into_response()
}

/// 204 plus expired `pm_session` / `pm_csrf` cookies.
fn clear_session_cookies(secure: bool) -> Response {
    (
        StatusCode::NO_CONTENT,
        AppendHeaders([
            (
                header::SET_COOKIE,
                clear_cookie(SESSION_COOKIE, secure, true),
            ),
            (header::SET_COOKIE, clear_cookie(CSRF_COOKIE, secure, false)),
        ]),
    )
        .into_response()
}

fn build_cookie(name: &str, value: &str, max_age: i64, secure: bool, http_only: bool) -> String {
    let mut cookie = format!("{name}={value}; Path=/; SameSite=Lax; Max-Age={max_age}");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn clear_cookie(name: &str, secure: bool, http_only: bool) -> String {
    let mut cookie =
        format!("{name}=; Path=/; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// Reject cross-origin setup/login when a browser supplies `Origin` (or, as a
/// fallback, `Referer`). Requests without either (non-browser clients) pass:
/// they carry no ambient cookie to abuse.
fn same_origin_ok(headers: &HeaderMap) -> bool {
    let source = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get(header::REFERER).and_then(|v| v.to_str().ok()));
    let Some(source) = source else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(authority) = origin_authority(source) else {
        return false;
    };
    if authority.eq_ignore_ascii_case(host) {
        return true;
    }

    // A reverse proxy or Vite dev proxy can terminate the browser origin
    // before forwarding the request to this backend. Permit only explicitly
    // configured authorities; never infer trust from arbitrary Origin values.
    std::env::var("PUMPKIN_ALLOWED_ORIGINS")
        .ok()
        .is_some_and(|allowed| {
            allowed.split(',').any(|candidate| {
                origin_authority(candidate.trim())
                    .is_some_and(|configured| configured.eq_ignore_ascii_case(authority))
            })
        })
}

/// `https://host:port/path` → `host:port`; scheme-relative `//host` honored.
fn origin_authority(url: &str) -> Option<&str> {
    let rest = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .or_else(|| url.strip_prefix("//"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    (!authority.is_empty()).then_some(authority)
}

/// Rate-limit key for the calling client. Only the transport peer is trusted;
/// accepting a client-supplied `X-Forwarded-For` here would let an attacker
/// mint a new bucket for every login attempt. When the manager sits behind a
/// proxy without a trusted-proxy configuration, all forwarded requests share
/// the fallback bucket rather than bypassing the limiter.
fn client_key(peer: Option<&SocketAddr>) -> String {
    match peer {
        Some(addr) => format!("ip:{addr}"),
        None => "ip:global".to_string(),
    }
}

/// Username bucket key. Takes the *raw* submitted name (rate limiting runs
/// before validation) but lowercases and truncates it so junk input cannot
/// create oversized keys.
fn user_key(endpoint: &str, username: &str) -> String {
    let bounded: String = username.trim().chars().take(MAX_USERNAME_LEN).collect();
    format!("{endpoint}:user:{}", bounded.to_lowercase())
}

/// Consume one token for each key. Returns `Err(retry_after_secs)` if any key
/// is exhausted (the largest wait among them). Pruning/eviction is handled
/// inside the limiter at a bounded rate.
fn check_rate_limit(app: &crate::AppState, keys: &[String]) -> Result<(), u64> {
    let mut limiter = lock_limiter(app);
    let mut retry = 0u64;
    for key in keys {
        if let Err(secs) = limiter.check(key) {
            retry = retry.max(secs);
        }
    }
    if retry > 0 {
        Err(retry)
    } else {
        Ok(())
    }
}

fn reset_rate_limit(app: &crate::AppState, key: &str) {
    lock_limiter(app).reset(key);
}

fn lock_limiter(app: &crate::AppState) -> std::sync::MutexGuard<'_, crate::ratelimit::RateLimiter> {
    app.rate_limiter
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn rate_limited(retry_after_secs: u64) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        headers.insert(header::RETRY_AFTER, value);
    }
    (
        StatusCode::TOO_MANY_REQUESTS,
        headers,
        Json(serde_json::json!({ "error_code": "RATE_LIMITED" })),
    )
        .into_response()
}

fn validate_username(raw: &str) -> Result<String, &'static str> {
    let username = raw.trim().to_owned();
    if username.is_empty()
        || username.chars().count() > MAX_USERNAME_LEN
        || username.chars().any(char::is_control)
    {
        return Err("INVALID_USERNAME");
    }
    Ok(username)
}

fn validate_password(password: &str, enforce_min: bool) -> Result<(), &'static str> {
    if password.len() > MAX_PASSWORD_LEN || (enforce_min && password.len() < MIN_PASSWORD_LEN) {
        return Err("INVALID_CREDENTIALS");
    }
    Ok(())
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error_code": code }))).into_response()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
