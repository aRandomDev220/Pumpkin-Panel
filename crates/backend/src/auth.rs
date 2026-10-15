//! Authentication, session tokens, and RBAC (spec sections 12.1–12.2).
//!
//! - Passwords: Argon2id hashes (`hash_password` / `verify_password`).
//! - Sessions: opaque 256-bit bearer tokens; only `SHA-256(token)` hex is
//!   stored (`hash_token`). The raw token is returned to the client once.
//! - Requests authenticate via `Authorization: Bearer <token>` (preferred)
//!   or the `pm_session=<token>` cookie. Failures yield 401
//!   `{error_code:"AUTH_REQUIRED"}`; missing permissions yield 403
//!   `{error_code:"PERMISSION_DENIED"}`.

use std::{future::Future, pin::Pin};

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

pub const SESSION_COOKIE_NAME: &str = "pm_session";

// ---------------------------------------------------------------------------
// Roles & permissions (spec 12.2)
// ---------------------------------------------------------------------------

/// Global roles. Stored lowercase in `users.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // `as_str` used by Phase 7 user-admin UI
pub enum Role {
    Owner,
    Admin,
    Viewer,
}

impl Role {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    #[allow(dead_code)] // consumed by login/setup responses (Phase 2) + Phase 7 UI
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Viewer => "viewer",
        }
    }
}

/// Permissions from spec 12.2. Config/files/backup variants are enforced
/// by their workstreams (Phases 4-5); listed here so the matrix is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum Permission {
    ServerRead,
    ServerCreate,
    ServerLifecycle,
    ServerDelete,
    ServerConfigRead,
    ServerConfigWrite,
    ServerFilesRead,
    ServerFilesWrite,
    ServerBackupCreate,
    ServerBackupRestore,
    ServerUpdate,
    UsersManage,
    SettingsManage,
    AuditRead,
}

impl Permission {
    #[allow(dead_code)] // surfaced by Phase 7 audit UI
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServerRead => "server.read",
            Self::ServerCreate => "server.create",
            Self::ServerLifecycle => "server.lifecycle",
            Self::ServerDelete => "server.delete",
            Self::ServerConfigRead => "server.config.read",
            Self::ServerConfigWrite => "server.config.write",
            Self::ServerFilesRead => "server.files.read",
            Self::ServerFilesWrite => "server.files.write",
            Self::ServerBackupCreate => "server.backup.create",
            Self::ServerBackupRestore => "server.backup.restore",
            Self::ServerUpdate => "server.update",
            Self::UsersManage => "users.manage",
            Self::SettingsManage => "settings.manage",
            Self::AuditRead => "audit.read",
        }
    }

    #[allow(dead_code)] // surfaced by Phase 7 audit UI
    pub fn all() -> &'static [Self] {
        use Permission as P;
        &[
            P::ServerRead,
            P::ServerCreate,
            P::ServerLifecycle,
            P::ServerDelete,
            P::ServerConfigRead,
            P::ServerConfigWrite,
            P::ServerFilesRead,
            P::ServerFilesWrite,
            P::ServerBackupCreate,
            P::ServerBackupRestore,
            P::ServerUpdate,
            P::UsersManage,
            P::SettingsManage,
            P::AuditRead,
        ]
    }
}

/// Central policy check: owner = all; admin = all except
/// `users.manage`/`settings.manage`; viewer = read-only
/// (`server.read`, `server.config.read`, `server.files.read`).
pub fn has_permission(role: Role, permission: Permission) -> bool {
    match role {
        Role::Owner => true,
        Role::Admin => !matches!(
            permission,
            Permission::UsersManage | Permission::SettingsManage
        ),
        Role::Viewer => matches!(
            permission,
            Permission::ServerRead | Permission::ServerConfigRead | Permission::ServerFilesRead
        ),
    }
}

// ---------------------------------------------------------------------------
// Password hashing (Argon2id)
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("password hashing failed")]
    HashFailed,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// Hash a password with Argon2id + fresh 128-bit salt. Returns PHC string.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let mut salt_bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| AuthError::HashFailed)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::HashFailed)
}

/// Verify a password against a stored PHC hash. Unknown hash format = false.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::HashFailed)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

// ---------------------------------------------------------------------------
// Session tokens (opaque bearer, SHA-256 at rest)
// ---------------------------------------------------------------------------

/// Generate a raw 256-bit session token, hex-encoded (64 chars).
pub fn generate_session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex_encode(&bytes)
}

/// SHA-256 hex digest of a raw token — the value stored in the DB.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_digit(b >> 4));
        out.push(hex_digit(b & 0x0f));
    }
    out
}

fn hex_digit(n: u8) -> char {
    (b"0123456789abcdef"[n as usize]) as char
}

// ---------------------------------------------------------------------------
// Authenticated identity
// ---------------------------------------------------------------------------

/// Identity injected into request extensions by [`auth_middleware`].
#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: Role,
}

impl AuthUser {
    pub fn can(&self, permission: Permission) -> bool {
        has_permission(self.role, permission)
    }
}

// Direct extractor: `async fn handler(user: AuthUser)`. Rejects 401 when the
// middleware did not attach an identity (e.g. route missing the layer).
#[axum::async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(unauthorized)
    }
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error_code": "AUTH_REQUIRED" })),
    )
        .into_response()
}

fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error_code": "PERMISSION_DENIED" })),
    )
        .into_response()
}

/// Pulls the bearer token: `Authorization: Bearer` first, then cookie.
pub fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            let token = token.trim().to_owned();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                if k.trim() == SESSION_COOKIE_NAME {
                    let v = v.trim().to_owned();
                    (!v.is_empty()).then_some(v)
                } else {
                    None
                }
            })
        })
}

/// Validates the session against the DB: unknown, revoked, or expired
/// sessions and inactive users are rejected. Refreshes `last_seen_at`
/// best-effort. Times in `sessions` are RFC 3339 UTC strings.
pub async fn authenticate(pool: &SqlitePool, headers: &HeaderMap) -> Option<AuthUser> {
    let token = extract_token(headers)?;
    let token_hash = hash_token(&token);
    let row = sqlx::query(
        "SELECT s.user_id, s.expires_at, s.revoked_at,
                u.username, u.role, u.is_active
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;
    use sqlx::Row as _;
    let revoked_at: Option<String> = row.get("revoked_at");
    if revoked_at.is_some() {
        return None;
    }
    let expires_at: String = row.get("expires_at");
    if parse_time(&expires_at).is_none_or(|t| t <= chrono::Utc::now()) {
        return None;
    }
    let is_active: i64 = row.get("is_active");
    if is_active == 0 {
        return None;
    }
    let role: String = row.get("role");
    let role = Role::from_str(&role)?;
    let user = AuthUser {
        id: row.get("user_id"),
        username: row.get("username"),
        role,
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _ = sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_hash = ?")
        .bind(&now)
        .bind(&token_hash)
        .execute(pool)
        .await;
    tracing::debug!(user_id = %user.id, "authenticated request");
    Some(user)
}

fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Axum middleware: attaches [`AuthUser`] or rejects 401.
/// Usage: `.route_layer(axum::middleware::from_fn_with_state(pool, auth_middleware))`.
pub async fn auth_middleware(
    State(pool): State<SqlitePool>,
    mut req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    match authenticate(&pool, req.headers()).await {
        Some(user) => {
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        None => unauthorized(),
    }
}

/// Middleware factory enforcing one permission (after [`auth_middleware`]).
/// Missing identity or denied permission yields 401 / 403 respectively.
/// Usage: `.route_layer(axum::middleware::from_fn(require_permission(Permission::ServerCreate)))`.
pub fn require_permission(
    permission: Permission,
) -> impl Clone
       + Send
       + Sync
       + 'static
       + Fn(
    axum::http::Request<axum::body::Body>,
    Next,
) -> Pin<Box<dyn Send + Future<Output = Response>>> {
    move |req: axum::http::Request<axum::body::Body>, next: Next| {
        Box::pin(async move { check_permission(req, next, permission).await })
            as Pin<Box<dyn Send + Future<Output = Response>>>
    }
}

async fn check_permission(
    req: axum::http::Request<axum::body::Body>,
    next: Next,
    permission: Permission,
) -> Response {
    let authed = req.extensions().get::<AuthUser>().is_some();
    let allowed = req
        .extensions()
        .get::<AuthUser>()
        .is_some_and(|u| u.can(permission));
    if !authed {
        return unauthorized();
    }
    if !allowed {
        return forbidden();
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------
// First-run setup guard
// ---------------------------------------------------------------------------

/// True when no users exist yet (fresh install → allow `/auth/setup`).
pub async fn needs_setup(pool: &SqlitePool) -> Result<bool, AuthError> {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM users")
        .fetch_one(pool)
        .await?;
    use sqlx::Row as _;
    Ok(row.get::<i64, _>("count") == 0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_has_all_permissions() {
        for p in Permission::all() {
            assert!(
                has_permission(Role::Owner, *p),
                "owner lacks {}",
                p.as_str()
            );
        }
    }

    #[test]
    fn admin_has_all_except_user_and_settings_management() {
        for p in Permission::all() {
            let expected = !matches!(p, Permission::UsersManage | Permission::SettingsManage);
            assert_eq!(
                has_permission(Role::Admin, *p),
                expected,
                "admin permission {}",
                p.as_str()
            );
        }
    }

    #[test]
    fn viewer_is_read_only() {
        let allowed = [
            Permission::ServerRead,
            Permission::ServerConfigRead,
            Permission::ServerFilesRead,
        ];
        for p in Permission::all() {
            assert_eq!(
                has_permission(Role::Viewer, *p),
                allowed.contains(p),
                "viewer permission {}",
                p.as_str()
            );
        }
    }

    #[test]
    fn token_hash_roundtrip() {
        let t1 = generate_session_token();
        let t2 = generate_session_token();
        assert_eq!(t1.len(), 64, "256-bit token as hex");
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t1, t2, "tokens must be unique");
        // Deterministic: same input → same stored hash.
        assert_eq!(hash_token(&t1), hash_token(&t1));
        // Distinct tokens → distinct hashes; raw token never stored as-is.
        assert_ne!(hash_token(&t1), hash_token(&t2));
        assert_ne!(hash_token(&t1), t1);
        assert_eq!(hash_token("abc").len(), 64);
    }

    #[test]
    fn password_hash_verify_roundtrip() {
        let hash = hash_password("correct horse 9!").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse 9!", &hash).expect("verify"));
        assert!(!verify_password("wrong password", &hash).expect("verify"));
        // Garbage hash must not verify (error, not panic).
        assert!(verify_password("x", "not-a-hash").is_err());
    }

    #[test]
    fn extract_token_prefers_bearer_over_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE_NAME}=cookie-token")
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_token(&headers).as_deref(), Some("cookie-token"));

        headers.insert(
            header::AUTHORIZATION,
            "Bearer bearer-token".parse().unwrap(),
        );
        assert_eq!(extract_token(&headers).as_deref(), Some("bearer-token"));

        let empty = HeaderMap::new();
        assert_eq!(extract_token(&empty), None);
    }
}
