//! Auth HTTP handlers: setup-status, setup, login, logout, me,
//! change-password. Standalone functions; router wiring (including
//! `auth_middleware` layering and rate limits) lands in a later workstream.

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::auth::{self, AuthUser};

// Expected `sessions` table (to be created by migration `0002_sessions.sql`):
//
// ```sql
// CREATE TABLE sessions (
//     id TEXT PRIMARY KEY,
//     user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
//     token_hash TEXT NOT NULL UNIQUE,
//     created_at TEXT NOT NULL
//         DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
//     expires_at TEXT NOT NULL,
//     last_seen_at TEXT,
//     revoked_at TEXT
// );
// CREATE INDEX idx_sessions_user ON sessions(user_id);
// ```
//
// `token_hash` holds `auth::hash_token(raw_token)` (SHA-256 hex).
// Times are RFC 3339 UTC strings, matching `migrations/0001_init.sql`
// conventions.

// Sessions live 12h by default; router wiring should pass the configured
// `session_ttl_minutes` here in a later workstream.
pub(crate) const SESSION_TTL_SECS: i64 = 12 * 60 * 60;

const SESSION_COOKIE: &str = "pm_session";

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

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub token: String,
    pub user: SessionUser,
}

// ---------------------------------------------------------------------------
// Handlers (wired to routes by a later workstream)
// ---------------------------------------------------------------------------

/// GET /api/v1/auth/setup-status — public, reports first-run state.
#[allow(dead_code)]
pub async fn setup_status(State(app): State<crate::AppState>) -> impl IntoResponse {
    let pool: SqlitePool = app.pool.clone();
    let needs_setup = auth::needs_setup(&pool).await.unwrap_or(false);
    Json(SetupStatusResponse { needs_setup })
}

/// POST /api/v1/auth/setup — creates the first `owner` account.
/// Only valid when no users exist; otherwise 409.
#[allow(dead_code)]
pub async fn setup(State(app): State<crate::AppState>, Json(body): Json<SetupRequest>) -> Response {
    let pool: SqlitePool = app.pool.clone();
    if !auth::needs_setup(&pool).await.unwrap_or(false) {
        return error(StatusCode::CONFLICT, "SETUP_ALREADY_COMPLETE");
    }
    let username = body.username.trim().to_owned();
    if username.is_empty() || body.password.len() < 8 {
        return error(StatusCode::BAD_REQUEST, "INVALID_CREDENTIALS");
    }
    let password_hash = match auth::hash_password(&body.password) {
        Ok(h) => h,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();
    let created = sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, is_active, created_at, updated_at)
         VALUES (?, ?, ?, 'owner', 1, ?, ?)",
    )
    .bind(&id)
    .bind(&username)
    .bind(&password_hash)
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await;
    if created.is_err() {
        // Race with a concurrent setup call: re-check before failing.
        if !auth::needs_setup(&pool).await.unwrap_or(false) {
            return error(StatusCode::CONFLICT, "SETUP_ALREADY_COMPLETE");
        }
        return error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL");
    }
    match issue_session(&pool, &id).await {
        Ok(token) => {
            let user = SessionUser {
                id: id.clone(),
                username: username.clone(),
                role: "owner".to_string(),
            };
            session_response(&token, user)
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    }
}

/// POST /api/v1/auth/login — verifies credentials and issues a session.
///
/// NOTE: brute-force throttling is enforced by a rate-limit layer applied
/// at router wiring time (per-IP + per-account); this handler stays
/// dependency-free so it can be unit-tested in isolation.
#[allow(dead_code)]
pub async fn login(State(app): State<crate::AppState>, Json(body): Json<LoginRequest>) -> Response {
    let pool: SqlitePool = app.pool.clone();
    let row = sqlx::query("SELECT id, password_hash, is_active FROM users WHERE username = ?")
        .bind(body.username.trim())
        .fetch_optional(&pool)
        .await;
    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) | Err(_) => {
            // Same code as bad password: no user enumeration.
            return error(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS");
        }
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
    match issue_session(&pool, &user_id).await {
        Ok(token) => {
            let user = SessionUser {
                id: user_id.clone(),
                username,
                role,
            };
            session_response(&token, user)
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL"),
    }
}

/// POST /api/v1/auth/logout — revokes the calling session.
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
    StatusCode::NO_CONTENT.into_response()
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
    if body.new_password.len() < 8 {
        return error(StatusCode::BAD_REQUEST, "WEAK_PASSWORD");
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

/// Inserts a session row and returns the raw token (shown only once).
async fn issue_session(pool: &SqlitePool, user_id: &str) -> Result<String, sqlx::Error> {
    let token = auth::generate_session_token();
    let token_hash = auth::hash_token(&token);
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let expires_at = (now + chrono::Duration::seconds(SESSION_TTL_SECS))
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

/// 200 JSON `{token, user}` plus `Set-Cookie: pm_session=...`.
fn session_response(token: &str, user: SessionUser) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={SESSION_TTL_SECS}"
    );
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(SessionResponse {
            token: token.to_owned(),
            user,
        }),
    )
        .into_response()
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error_code": code }))).into_response()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
