//! Authentication, session tokens, and RBAC (spec sections 12.1–12.2).
//!
//! - Passwords: Argon2id hashes (`hash_password` / `verify_password`).
//! - Sessions: opaque 256-bit bearer tokens; only `SHA-256(token)` hex is
//!   stored (`hash_token`). The raw token is returned to the client once.
//! - Requests authenticate via `Authorization: Bearer <token>` (preferred)
//!   or the `pm_session=<token>` cookie. Failures yield 401
//!   `{error_code:"AUTH_REQUIRED"}`; missing permissions yield 403
//!   `{error_code:"PERMISSION_DENIED"}`.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        OnceLock,
    },
};

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

pub const SESSION_COOKIE_NAME: &str = "pm_session";

/// Readable by JS on purpose: double-submit CSRF pairs this cookie with the
/// `X-CSRF-Token` header. Never marked `HttpOnly`.
pub const CSRF_COOKIE_NAME: &str = "pm_csrf";

/// Request header the SPA copies the `pm_csrf` cookie into.
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";

/// Counts dummy Argon2id verifications run for unknown usernames. Test-only
/// seam proving the timing-equalizing path executed (never gates behavior).
pub(crate) static DUMMY_HASH_CHECKS: AtomicUsize = AtomicUsize::new(0);

/// Which credential carrier `extract_token_with_source` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// `Authorization: Bearer <token>` — immune to CSRF (not auto-sent).
    Bearer,
    /// `pm_session` cookie — auto-sent by browsers, so mutations need CSRF.
    Cookie,
}

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

/// A fixed, valid Argon2id hash used to equalize login timing for unknown
/// usernames. Computed once per process from a throwaway password; the value
/// never matches a real credential.
pub fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password("timing-equalizer-not-a-real-password").unwrap_or_else(|_| String::new())
    })
}

/// Burn the same Argon2id work a real verification costs, without revealing
/// whether the account exists. The `DUMMY_HASH_CHECKS` counter is the seam
/// tests use instead of flaky wall-clock assertions.
pub fn verify_dummy(password: &str) {
    DUMMY_HASH_CHECKS.fetch_add(1, Ordering::Relaxed);
    let _ = verify_password(password, dummy_hash());
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
    extract_token_with_source(headers).map(|(token, _)| token)
}

/// Like [`extract_token`], but reports which carrier supplied the token so the
/// middleware can decide whether CSRF applies.
pub fn extract_token_with_source(headers: &HeaderMap) -> Option<(String, TokenSource)> {
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            let token = token.trim().to_owned();
            if !token.is_empty() {
                return Some((token, TokenSource::Bearer));
            }
        }
    }
    cookie_value(headers, SESSION_COOKIE_NAME).map(|token| (token, TokenSource::Cookie))
}

/// Read one cookie value out of a `Cookie` header (first match wins).
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                if k.trim() == name {
                    let v = v.trim().to_owned();
                    (!v.is_empty()).then_some(v)
                } else {
                    None
                }
            })
        })
}

/// Double-submit CSRF: the `pm_csrf` cookie must equal the `X-CSRF-Token`
/// header. Both must be non-empty. Compared in constant time.
pub fn csrf_ok(headers: &HeaderMap) -> bool {
    let Some(cookie) = cookie_value(headers, CSRF_COOKIE_NAME) else {
        return false;
    };
    let Some(header) = headers
        .get(CSRF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    else {
        return false;
    };
    !header.is_empty() && constant_time_eq(cookie.as_bytes(), header.as_bytes())
}

/// `true` for methods that change state and therefore need CSRF protection.
pub fn is_unsafe_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Test-only helpers
// ---------------------------------------------------------------------------

/// Extract a cookie's value from a response's `Set-Cookie` headers (test-only:
/// the token is no longer returned in the response body).
#[cfg(test)]
pub fn session_token_from_set_cookie(headers: &HeaderMap) -> Option<String> {
    set_cookie_value(headers, SESSION_COOKIE_NAME)
}

/// Extract the `pm_csrf` cookie from a response's `Set-Cookie` headers.
#[cfg(test)]
pub fn csrf_token_from_set_cookie(headers: &HeaderMap) -> Option<String> {
    set_cookie_value(headers, CSRF_COOKIE_NAME)
}

#[cfg(test)]
fn set_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|raw| {
            let (pair, _attrs) = raw.split_once(';').unwrap_or((raw, ""));
            let (k, v) = pair.split_once('=')?;
            (k.trim() == name && !v.trim().is_empty()).then(|| v.trim().to_owned())
        })
}

/// Validates the session against the DB: unknown, revoked, or expired
/// sessions and inactive users are rejected. Refreshes `last_seen_at`
/// best-effort. Times in `sessions` are RFC 3339 UTC strings.
#[allow(dead_code)] // thin wrapper kept for callers that don't need the source
pub async fn authenticate(pool: &SqlitePool, headers: &HeaderMap) -> Option<AuthUser> {
    authenticate_with_source(pool, headers)
        .await
        .map(|(user, _)| user)
}

/// Like [`authenticate`], but also reports the credential carrier so callers
/// can enforce CSRF for cookie-authenticated mutations.
pub async fn authenticate_with_source(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Option<(AuthUser, TokenSource)> {
    let (token, source) = extract_token_with_source(headers)?;
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
    Some((user, source))
}

fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Axum middleware: attaches [`AuthUser`] or rejects 401. Cookie-authenticated
/// unsafe methods must also present a matching `X-CSRF-Token` header, else 403
/// `CSRF_FAILED` (bearer tokens are exempt: browsers do not auto-send them).
/// Usage: `.route_layer(axum::middleware::from_fn_with_state(pool, auth_middleware))`.
pub async fn auth_middleware(
    State(pool): State<SqlitePool>,
    mut req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let csrf_ok = csrf_ok(req.headers());
    match authenticate_with_source(&pool, req.headers()).await {
        Some((_user, TokenSource::Cookie)) if is_unsafe_method(&method) && !csrf_ok => {
            csrf_failed()
        }
        Some((user, _)) => {
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        None => unauthorized(),
    }
}

fn csrf_failed() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error_code": "CSRF_FAILED" })),
    )
        .into_response()
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

/// True when first-run setup has not completed and no users exist.
///
/// `setup_complete` (persisted in `app_settings`) is authoritative: once set,
/// setup stays disabled even if the `users` table is later emptied.
pub async fn needs_setup(pool: &SqlitePool) -> Result<bool, AuthError> {
    if setup_complete(pool).await? {
        return Ok(false);
    }
    let row = sqlx::query("SELECT COUNT(*) AS count FROM users")
        .fetch_one(pool)
        .await?;
    use sqlx::Row as _;
    Ok(row.get::<i64, _>("count") == 0)
}

/// True once the one-shot `setup_complete` flag is persisted.
pub async fn setup_complete(pool: &SqlitePool) -> Result<bool, AuthError> {
    let row = sqlx::query("SELECT 1 FROM app_settings WHERE key = 'setup_complete' LIMIT 1")
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
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

    #[test]
    fn extract_token_reports_source() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE_NAME}=cookie-token")
                .parse()
                .unwrap(),
        );
        assert_eq!(
            extract_token_with_source(&headers),
            Some(("cookie-token".to_string(), TokenSource::Cookie))
        );
        headers.insert(
            header::AUTHORIZATION,
            "Bearer bearer-token".parse().unwrap(),
        );
        assert_eq!(
            extract_token_with_source(&headers),
            Some(("bearer-token".to_string(), TokenSource::Bearer))
        );
    }

    #[test]
    fn csrf_ok_requires_matching_header_and_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{CSRF_COOKIE_NAME}=abc123").parse().unwrap(),
        );
        assert!(!csrf_ok(&headers), "header absent → not ok");
        headers.insert(CSRF_HEADER_NAME, "wrong".parse().unwrap());
        assert!(!csrf_ok(&headers), "header mismatch → not ok");
        headers.insert(CSRF_HEADER_NAME, "abc123".parse().unwrap());
        assert!(csrf_ok(&headers), "matching header → ok");

        let mut no_cookie = HeaderMap::new();
        no_cookie.insert(CSRF_HEADER_NAME, "abc123".parse().unwrap());
        assert!(!csrf_ok(&no_cookie), "cookie absent → not ok");
    }

    #[test]
    fn unsafe_methods_are_flagged() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(is_unsafe_method(&method), "{method} is unsafe");
        }
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert!(!is_unsafe_method(&method), "{method} is safe");
        }
    }

    #[test]
    fn set_cookie_value_reads_named_cookie() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::SET_COOKIE,
            format!("{SESSION_COOKIE_NAME}=tok; HttpOnly; Secure")
                .parse()
                .unwrap(),
        );
        headers.append(
            header::SET_COOKIE,
            format!("{CSRF_COOKIE_NAME}=csrfvalue; Path=/")
                .parse()
                .unwrap(),
        );
        assert_eq!(
            session_token_from_set_cookie(&headers).as_deref(),
            Some("tok")
        );
        assert_eq!(
            csrf_token_from_set_cookie(&headers).as_deref(),
            Some("csrfvalue")
        );
        assert_eq!(session_token_from_set_cookie(&HeaderMap::new()), None);
    }
}
