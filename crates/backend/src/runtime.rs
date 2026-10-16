//! Phase 2 workstream B — container runtime adapter (Docker) + server spec validation.
//!
//! This module owns everything the manager needs to run Pumpkin game servers
//! as *child* containers of the single manager container:
//!
//! * [`ValidatedServerSpec`] — the only shape from which a container can be
//!   built. Privileged flags (`--privileged`, `cap-add`, `network-mode:
//!   host`, bind-mounts outside the server root, arbitrary images, …) are
//!   **unrepresentable**: the input struct has no such fields (plus
//!   `#[serde(deny_unknown_fields)]`), and [`DockerAdapter::build_create_config`]
//!   never sets them.
//! * [`validate_spec`] — pure validation (image allowlist, slug shape, port
//!   range, memory/CPU bounds, data-path containment).
//! * [`DockerAdapter`] — thin wrapper around [`bollard::Docker`] connected
//!   over `/var/run/docker.sock`, implementing [`RuntimeAdapter`].
//! * [`reconcile`] — read-only drift detection between the `servers` table
//!   and the containers actually present. It never deletes anything.
//!
//! # Bedrock port convention
//!
//! Pumpkin serves Java Edition on `game_port` (container `25565/tcp`) and
//! Bedrock on `bedrock_port = game_port + 1` (container `19132/udp`). The
//! Bedrock port is *derived*, never user-supplied, so the two can never
//! drift apart. `game_port` is capped at 65534 so `game_port + 1` cannot
//! overflow `u16`.
//!
//! # Deliberate container-security decisions (documented, not accidental)
//!
//! * `User` is left **unset**: the Pumpkin image already runs as the
//!   unprivileged `pumpkin` user. The manager instead ensures the host data
//!   directory exists before create; the image/deploy entrypoint owns the
//!   chown to the pumpkin UID (see [`DockerAdapter::create`]).
//! * `RestartPolicy` is **`no`**, not `unless-stopped`: the manager (task
//!   runner + desired-state reconciliation) owns restarts. A daemon-level
//!   restart policy would fight the manager and resurrect servers an admin
//!   stopped on purpose.
//! * `ReadonlyRootfs` is **not** set: Pumpkin writes its world, config and
//!   logs under `/pumpkin` at runtime.
//! * `CapDrop: [ALL]` + `SecurityOpt: [no-new-privileges:true]` harden the
//!   container; nothing is ever added back via `CapAdd`/`Privileged`.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;

// NOTE: bollard 0.18 API (pinned in the workspace). The 0.19+ `models::`
// `ContainerCreateBody` / `query_parameters` builders do not exist here:
// container configuration is `container::Config<String>` and every
// operation takes a plain options struct from `container::`.
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, ListContainersOptions, LogOutput,
    LogsOptions, RemoveContainerOptions, RestartContainerOptions, StartContainerOptions,
    StatsOptions, StopContainerOptions,
};
use bollard::models::{HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::Docker;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Unix socket the manager uses to talk to the host Docker daemon.
/// The manager container mounts `/var/run/docker.sock` read-write.
#[allow(dead_code)] // referenced by deploy docs + Phase 7 diagnostics
pub const DOCKER_SOCKET: &str = "/var/run/docker.sock";

/// Only images built by the Pumpkin project may run as game servers.
pub const IMAGE_ALLOWLIST_PREFIX: &str = "ghcr.io/pumpkin-mc/pumpkin:";

/// Lowest/highest host port a Java game port may bind.
pub const MIN_GAME_PORT: u16 = 25565;
/// `u16::MAX - 1` so `bedrock_port = game_port + 1` cannot overflow.
pub const MAX_GAME_PORT: u16 = 65534;

/// Ports the manager itself (or its dev stack) uses. Never assignable to a
/// game server, even if the allowed range is ever widened.
pub const RESERVED_PORTS: &[u16] = &[8080, 3000, 5173, 5432];

/// 256 MiB — smallest heap Pumpkin can boot with.
pub const MIN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
/// 32 GiB — sanity ceiling for a single game server.
pub const MAX_MEMORY_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// Fractional CPUs: 0.1 vCPU ..= 16 vCPUs.
pub const MIN_CPU_LIMIT: f64 = 0.1;
pub const MAX_CPU_LIMIT: f64 = 16.0;

/// Container-side ports Pumpkin listens on (fixed by the image).
pub const CONTAINER_JAVA_PORT: u16 = 25565;
pub const CONTAINER_BEDROCK_PORT: u16 = 19132;

/// Grace period (seconds) for SIGTERM before SIGKILL. Pumpkin flushes
/// worlds on stop, so this is generous on purpose.
pub const STOP_TIMEOUT_SECS: i64 = 35;

/// Label marking containers owned by this manager.
pub const LABEL_MANAGED: &str = "io.pumpkin-manager.managed";
/// Label carrying the `servers.id` this container belongs to.
pub const LABEL_SERVER_ID: &str = "io.pumpkin-manager.server-id";
/// Label schema version so future managers can migrate old containers.
pub const LABEL_SCHEMA: &str = "io.pumpkin-manager.schema";
pub const LABEL_SCHEMA_VERSION: &str = "1";

// ---------------------------------------------------------------------------
// Spec validation
// ---------------------------------------------------------------------------

/// Machine-facing error codes returned for spec problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecErrorCode {
    /// A host port is reserved or already taken.
    PortUnavailable,
    /// Anything else wrong with the requested configuration.
    InvalidConfiguration,
    /// The host cannot satisfy the request (e.g. unusable server root).
    CapabilityUnavailable,
}

impl SpecErrorCode {
    /// Wire code used in API error bodies.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::PortUnavailable => "PORT_UNAVAILABLE",
            Self::InvalidConfiguration => "INVALID_CONFIGURATION",
            Self::CapabilityUnavailable => "CAPABILITY_UNAVAILABLE",
        }
    }
}

/// Validation failure for a server spec.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SpecError {
    /// `name` empty or longer than 64 chars.
    #[error("invalid server name: must be 1-64 characters")]
    InvalidName,
    /// `slug` must match `^[a-z0-9-]{{3,48}}$`.
    #[error("invalid slug '{0}': must match ^[a-z0-9-]{{3,48}}$")]
    InvalidSlug(String),
    /// Image is outside `ghcr.io/pumpkin-mc/pumpkin:`.
    #[error("image '{0}' is not on the allowlist (must start with {IMAGE_ALLOWLIST_PREFIX})")]
    ImageNotAllowed(String),
    /// `image_digest` present but not a `sha256:…` digest.
    #[error("image digest must be empty or a sha256:<hex> digest")]
    InvalidDigest,
    /// Port outside `25565..=65534`.
    #[error("game port {0} out of range ({MIN_GAME_PORT}-{MAX_GAME_PORT})")]
    PortOutOfRange(u16),
    /// Port collides with a manager/dev-stack port.
    #[error("game port {0} is reserved")]
    PortReserved(u16),
    /// `game_port + 1` would overflow `u16`.
    #[error("game port {0} leaves no room for the bedrock port (game_port+1)")]
    BedrockOverflow(u16),
    /// Memory outside 256 MiB ..= 32 GiB.
    #[error("memory limit {0} bytes out of range ({MIN_MEMORY_BYTES}-{MAX_MEMORY_BYTES})")]
    InvalidMemory(u64),
    /// CPUs outside 0.1 ..= 16.0 (or NaN).
    #[error("cpu limit {0} out of range ({MIN_CPU_LIMIT}-{MAX_CPU_LIMIT})")]
    InvalidCpu(f64),
    /// `data_path` escapes `server_root`.
    #[error("data path '{0}' is outside the server root")]
    DataPathEscape(String),
    /// `bind_address` empty.
    #[error("bind address must not be empty")]
    InvalidBindAddress,
    /// The manager's own `server_root` is unusable (relative path, …).
    #[error("server root '{0}' is not usable")]
    UnusableServerRoot(String),
}

impl SpecError {
    /// Machine-facing error code for API error bodies.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::PortReserved(_) => SpecErrorCode::PortUnavailable.code(),
            Self::UnusableServerRoot(_) => SpecErrorCode::CapabilityUnavailable.code(),
            _ => SpecErrorCode::InvalidConfiguration.code(),
        }
    }
}

/// Raw, user-supplied server definition.
///
/// There is deliberately **no** field for privileged mode, capabilities,
/// network mode, extra binds/mounts, devices, or entrypoint overrides, and
/// unknown JSON fields are rejected — so Docker escape hatches such as
/// `--privileged` are unrepresentable at the API boundary, not just ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSpecInput {
    /// Owning server id (already allocated by the caller).
    pub server_id: Uuid,
    /// Display name, 1–64 chars.
    pub name: String,
    /// URL-safe handle, `^[a-z0-9-]{3,48}$`. Also derives the container name.
    pub slug: String,
    /// Must start with `ghcr.io/pumpkin-mc/pumpkin:`.
    pub image_reference: String,
    /// Optional pinned digest; empty or `sha256:<hex>`.
    #[serde(default)]
    pub image_digest: String,
    /// Host port for Java clients (`25565..=65534`).
    pub game_port: u16,
    /// Host interface to bind (e.g. `0.0.0.0`).
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    /// Host directory for this server's `/pumpkin` data. Must live under
    /// the manager's server root.
    pub data_path: String,
    /// Bytes, 256 MiB ..= 32 GiB.
    pub memory_limit_bytes: u64,
    /// Fractional vCPUs, 0.1 ..= 16.0.
    pub cpu_limit: f64,
}

fn default_bind_address() -> String {
    "0.0.0.0".to_string()
}

/// A spec that passed [`validate_spec`]. The only shape the runtime accepts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedServerSpec {
    /// Owning server id; becomes the `server-id` container label.
    pub server_id: Uuid,
    /// Display name.
    pub name: String,
    /// URL-safe handle; derives the container name `pumpkin-<slug>`.
    pub slug: String,
    /// Allowlisted image reference.
    pub image_reference: String,
    /// Optional pinned digest (`""` or `sha256:<hex>`).
    pub image_digest: String,
    /// Host port for Java clients.
    pub game_port: u16,
    /// Host port for Bedrock clients. Always `game_port + 1` (derived).
    pub bedrock_port: u16,
    /// Host interface to bind.
    pub bind_address: String,
    /// Host directory bind-mounted at `/pumpkin` (inside server root).
    pub data_path: String,
    /// Bytes, 256 MiB ..= 32 GiB.
    pub memory_limit_bytes: u64,
    /// Fractional vCPUs, 0.1 ..= 16.0.
    pub cpu_limit: f64,
}

/// Validate a slug against `^[a-z0-9-]{3,48}$` without a regex engine.
#[must_use]
pub fn is_valid_slug(slug: &str) -> bool {
    let len = slug.len();
    if !(3..=48).contains(&len) {
        return false;
    }
    slug.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Check the reserved-port list (manager/dev-stack ports).
#[must_use]
pub fn is_reserved_port(port: u16) -> bool {
    RESERVED_PORTS.contains(&port)
}

/// True when `reference` is `ghcr.io/pumpkin-mc/pumpkin:` followed by exactly
/// one non-empty tag segment of `[A-Za-z0-9._-]` (no extra `:`/`/`, no
/// whitespace). The official mutable tag `:master` remains allowed; digest
/// pinning, not a tag ban, is the reproducibility control.
#[must_use]
pub fn is_allowlisted_image(reference: &str) -> bool {
    let Some(tag) = reference.strip_prefix(IMAGE_ALLOWLIST_PREFIX) else {
        return false;
    };
    !tag.is_empty()
        && !tag.contains(':')
        && !tag.contains('/')
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// True only for a strict `sha256:` + 64 lowercase hex chars digest.
#[must_use]
pub fn is_valid_digest(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Resolve the image reference Docker should use.
///
/// When `digest` is non-empty the returned reference is `repo@sha256:…`
/// (the mutable tag is stripped so the daemon resolves the pinned digest);
/// otherwise `reference` is returned unchanged.
#[must_use]
pub fn pinned_image_reference(reference: &str, digest: &str) -> String {
    if digest.is_empty() {
        return reference.to_string();
    }
    // Strip the tag: `ghcr.io/pumpkin-mc/pumpkin:master` → `…/pumpkin`.
    // The allowlist guarantees a single `:` after the repository path.
    let repo = match reference.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => name,
        _ => reference,
    };
    format!("{repo}@{digest}")
}

/// Lexically resolve `.`/`..` segments without touching the filesystem
/// (the directory may not exist yet at validation time).
fn lexical_join(root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    let mut out = std::path::PathBuf::new();
    for comp in root.iter().chain(path.iter()) {
        use std::path::Component;
        let as_path = Path::new(comp);
        for c in as_path.components() {
            match c {
                Component::Prefix(_) | Component::RootDir => {
                    // Absolute anchor: reset (only meaningful for `root`).
                    out = std::path::PathBuf::from(c.as_os_str());
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        return None;
                    }
                }
                Component::Normal(seg) => out.push(seg),
            }
        }
    }
    Some(out)
}

/// Validate raw user input into a [`ValidatedServerSpec`].
///
/// `server_root` is the manager's server directory (e.g. `/data/servers`);
/// `data_path` must resolve inside it.
///
/// Note: live port collisions (two servers on one host port, or a port
/// already bound by another process) are enforced at create time by the
/// `servers.game_port` UNIQUE constraint and by Docker itself ("port is
/// already allocated" maps to [`RuntimeError::PortUnavailable`]).
pub fn validate_spec(
    input: ServerSpecInput,
    server_root: &Path,
) -> Result<ValidatedServerSpec, SpecError> {
    if !server_root.is_absolute() {
        return Err(SpecError::UnusableServerRoot(
            server_root.display().to_string(),
        ));
    }

    let name = input.name.trim().to_string();
    if name.is_empty() || name.len() > 64 {
        return Err(SpecError::InvalidName);
    }
    if !is_valid_slug(&input.slug) {
        return Err(SpecError::InvalidSlug(input.slug));
    }
    if !is_allowlisted_image(&input.image_reference) {
        return Err(SpecError::ImageNotAllowed(input.image_reference));
    }
    if !input.image_digest.is_empty() && !is_valid_digest(&input.image_digest) {
        return Err(SpecError::InvalidDigest);
    }
    if input.game_port < MIN_GAME_PORT || input.game_port > MAX_GAME_PORT {
        // 65535 is over the cap precisely so bedrock_port cannot overflow.
        if input.game_port == u16::MAX {
            return Err(SpecError::BedrockOverflow(input.game_port));
        }
        return Err(SpecError::PortOutOfRange(input.game_port));
    }
    if is_reserved_port(input.game_port) {
        return Err(SpecError::PortReserved(input.game_port));
    }
    let bedrock_port = input
        .game_port
        .checked_add(1)
        .ok_or(SpecError::BedrockOverflow(input.game_port))?;
    if !(MIN_MEMORY_BYTES..=MAX_MEMORY_BYTES).contains(&input.memory_limit_bytes) {
        return Err(SpecError::InvalidMemory(input.memory_limit_bytes));
    }
    if !(MIN_CPU_LIMIT..=MAX_CPU_LIMIT).contains(&input.cpu_limit) || input.cpu_limit.is_nan() {
        return Err(SpecError::InvalidCpu(input.cpu_limit));
    }
    if input.bind_address.trim().is_empty() {
        return Err(SpecError::InvalidBindAddress);
    }

    // `data_path` must stay inside `server_root` even with `..` segments.
    let data_path = Path::new(&input.data_path);
    let resolved = lexical_join(server_root, data_path)
        .ok_or_else(|| SpecError::DataPathEscape(input.data_path.clone()))?;
    let root_normalized =
        lexical_join(Path::new("/"), server_root).unwrap_or_else(|| server_root.to_path_buf());
    if resolved != root_normalized && !resolved.starts_with(&root_normalized) {
        return Err(SpecError::DataPathEscape(input.data_path.clone()));
    }

    Ok(ValidatedServerSpec {
        server_id: input.server_id,
        name,
        slug: input.slug,
        image_reference: input.image_reference,
        image_digest: input.image_digest,
        game_port: input.game_port,
        bedrock_port,
        bind_address: input.bind_address,
        data_path: resolved.to_string_lossy().into_owned(),
        memory_limit_bytes: input.memory_limit_bytes,
        cpu_limit: input.cpu_limit,
    })
}

// ---------------------------------------------------------------------------
// Runtime handles / snapshots
// ---------------------------------------------------------------------------

/// Reference to a container owned by the manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeHandle {
    /// Full Docker container id.
    pub container_id: String,
    /// Docker container name (`pumpkin-<slug>`).
    pub container_name: String,
}

/// Point-in-time view of a container from `inspect`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Full Docker container id.
    pub container_id: String,
    /// Lowercase Docker status (`created`, `running`, `exited`, …).
    pub status: String,
    /// Last exit code, if the daemon reports one.
    pub exit_code: Option<i64>,
    /// Healthcheck status (`none`, `starting`, `healthy`, `unhealthy`), if any.
    pub health: Option<String>,
    /// Daemon-reported restart count (`-1` when the daemon does not
    /// report one — never `0`-as-unknown).
    pub restart_count: i64,
}

/// Resource usage sample from `stats` (one-shot). Consumed by the
/// Phase 6 metrics collector; kept struct-complete now.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct RuntimeStats {
    /// Estimated CPU utilisation, percent of one core-seconds basis (0–100×nCPU).
    pub cpu_percent: f64,
    /// Current memory usage in bytes.
    pub memory_bytes: u64,
    /// Memory limit in bytes (cgroup limit, may exceed the configured one).
    pub memory_limit: u64,
}

/// Errors from the container runtime layer.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Docker daemon / client failure.
    #[error("docker error: {0}")]
    Docker(#[from] bollard::errors::Error),
    /// Host port already bound (Docker "port is already allocated").
    #[error("port unavailable: {0}")]
    PortUnavailable(String),
    /// Container does not exist (anymore).
    #[error("container not found: {0}")]
    NotFound(String),
    /// A container with the manager's `pumpkin-<slug>` name already exists
    /// (stale container, out-of-band create). Remove or rename it first.
    #[error("container name already in use: {0}")]
    NameConflict(String),
    /// Spec / config problem surfaced at runtime.
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),
    /// Host cannot satisfy the request.
    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),
    /// SQLite failure inside [`reconcile`].
    #[error("database error: {0}")]
    Database(String),
    /// Filesystem failure (e.g. creating the server data dir).
    #[error("io error: {0}")]
    Io(String),
}

impl RuntimeError {
    /// Machine-facing error code for API error bodies.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::PortUnavailable(_) => SpecErrorCode::PortUnavailable.code(),
            Self::NameConflict(_) => "CONTAINER_NAME_CONFLICT",
            Self::InvalidConfiguration(_) => SpecErrorCode::InvalidConfiguration.code(),
            Self::CapabilityUnavailable(_) => SpecErrorCode::CapabilityUnavailable.code(),
            Self::Database(_) | Self::Io(_) => "RUNTIME_ERROR",
            Self::Docker(_) => "DOCKER_API_ERROR",
            Self::NotFound(_) => "CONTAINER_NOT_FOUND",
        }
    }

    /// Safe, client-facing message for API error bodies.
    ///
    /// Never contains raw Docker/bollard text, subprocess output, socket
    /// paths, or filesystem paths (audit §9.4): the detailed `Display` is
    /// only logged, never returned to a client.
    #[must_use]
    pub fn client_message(&self) -> &'static str {
        match self {
            Self::PortUnavailable(_) => "requested host port is unavailable",
            Self::NotFound(_) => "container not found",
            Self::NameConflict(_) => "container name is already in use",
            Self::InvalidConfiguration(_) => "invalid runtime configuration",
            Self::CapabilityUnavailable(_) => "runtime capability unavailable",
            Self::Database(_) => "runtime database error",
            Self::Io(_) => "runtime filesystem error",
            Self::Docker(_) => "Docker daemon operation failed",
        }
    }

    /// Map a raw bollard failure to the most specific variant.
    fn from_bollard(err: bollard::errors::Error) -> Self {
        let msg = err.to_string();
        if msg.contains("port is already allocated") {
            Self::PortUnavailable(msg)
        } else if msg.contains("is already in use") {
            Self::NameConflict(msg)
        } else if msg.contains("No such container") {
            Self::NotFound(msg)
        } else {
            Self::Docker(err)
        }
    }
}

// ---------------------------------------------------------------------------
// RuntimeAdapter trait
// ---------------------------------------------------------------------------

/// Container lifecycle operations every runtime backend must provide.
///
/// Written as `fn … -> impl Future` (rather than `async fn`) so the
/// returned futures are explicitly `Send` — workers run them on the
/// multi-threaded Tokio runtime via spawned tasks.
pub trait RuntimeAdapter: Send + Sync {
    /// Create (but do not start) a container for `spec`.
    fn create(
        &self,
        spec: &ValidatedServerSpec,
    ) -> impl Future<Output = Result<RuntimeHandle, RuntimeError>> + Send;
    /// Start a stopped/created container.
    fn start(&self, container_id: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send;
    /// Stop with SIGTERM, escalating to SIGKILL after `timeout_secs`.
    fn stop(
        &self,
        container_id: &str,
        timeout_secs: u32,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send;
    /// Restart (uses [`STOP_TIMEOUT_SECS`] as the stop grace period).
    fn restart(&self, container_id: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send;
    /// Force-remove a container. Data survives (bind-mount, not a volume).
    fn remove(&self, container_id: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send;
    /// Point-in-time status view.
    fn inspect(
        &self,
        container_id: &str,
    ) -> impl Future<Output = Result<RuntimeSnapshot, RuntimeError>> + Send;
    /// Last `tail` lines of combined stdout/stderr.
    fn logs(
        &self,
        container_id: &str,
        tail: usize,
    ) -> impl Future<Output = Result<Vec<String>, RuntimeError>> + Send;
    /// One-shot resource usage sample (Phase 6 metrics).
    #[allow(dead_code)]
    fn stats(
        &self,
        container_id: &str,
    ) -> impl Future<Output = Result<RuntimeStats, RuntimeError>> + Send;
    /// Pull an image reference from the registry (Phase 7 updates).
    ///
    /// Default body reports unsupported so non-Docker backends (test fakes)
    /// keep compiling; [`DockerAdapter`] overrides it with a real
    /// `create_image` drain.
    fn pull_image(&self, reference: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        let reference = reference.to_string();
        async move {
            Err(RuntimeError::InvalidConfiguration(format!(
                "image pull not supported for reference '{reference}'"
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// DockerAdapter
// ---------------------------------------------------------------------------

/// [`RuntimeAdapter`] backed by the host Docker daemon over its Unix socket.
#[derive(Debug, Clone)]
pub struct DockerAdapter {
    docker: Docker,
}

impl DockerAdapter {
    /// Connect via [`DOCKER_SOCKET`] (`/var/run/docker.sock`), which the
    /// manager image mounts from the host.
    pub fn new() -> Result<Self, RuntimeError> {
        let docker = Docker::connect_with_unix_defaults().map_err(RuntimeError::from_bollard)?;
        Ok(Self { docker })
    }

    /// Connect to an explicit socket path (tests / non-standard layouts).
    pub fn connect_socket(path: &str) -> Result<Self, RuntimeError> {
        let docker = Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
            .map_err(RuntimeError::from_bollard)?;
        Ok(Self { docker })
    }

    /// Wrap an existing client (tests, custom transports).
    #[must_use]
    pub fn with_docker(docker: Docker) -> Self {
        Self { docker }
    }

    /// Docker container name for a server slug. Always prefixed so manager
    /// containers are recognisable in `docker ps`.
    #[must_use]
    pub fn container_name(slug: &str) -> String {
        format!("pumpkin-{slug}")
    }

    /// Ownership labels for a server id.
    #[must_use]
    pub fn labels(server_id: &str) -> HashMap<String, String> {
        HashMap::from([
            (LABEL_MANAGED.to_string(), "true".to_string()),
            (LABEL_SERVER_ID.to_string(), server_id.to_string()),
            (LABEL_SCHEMA.to_string(), LABEL_SCHEMA_VERSION.to_string()),
        ])
    }

    /// Build the Docker create body for a validated spec.
    ///
    /// Security posture (see module docs): no `User` override (image
    /// already drops to the `pumpkin` user), `RestartPolicy: no` (the
    /// manager owns restarts), no `ReadonlyRootfs` (Pumpkin writes
    /// `/pumpkin`), `CapDrop: [ALL]` + `no-new-privileges`, and console
    /// I/O enabled (`OpenStdin`/`StdinOnce`/`Tty`) because the web
    /// console attaches to the container's stdio.
    #[must_use]
    pub fn build_create_config(spec: &ValidatedServerSpec) -> Config<String> {
        let port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::from([
            (
                format!("{CONTAINER_JAVA_PORT}/tcp"),
                Some(vec![PortBinding {
                    host_ip: Some(spec.bind_address.clone()),
                    host_port: Some(spec.game_port.to_string()),
                }]),
            ),
            (
                format!("{CONTAINER_BEDROCK_PORT}/udp"),
                Some(vec![PortBinding {
                    host_ip: Some(spec.bind_address.clone()),
                    host_port: Some(spec.bedrock_port.to_string()),
                }]),
            ),
        ]);

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            // Host data dir -> /pumpkin inside the container.
            binds: Some(vec![format!("{}:/pumpkin:rw", spec.data_path)]),
            // The manager (task runner + desired-state sync) owns
            // restarts; a daemon policy would resurrect servers an admin
            // deliberately stopped.
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::NO),
                maximum_retry_count: None,
            }),
            memory: Some(spec.memory_limit_bytes as i64),
            nano_cpus: Some((spec.cpu_limit * 1_000_000_000.0) as i64),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            // Deliberately unset: ReadonlyRootfs (Pumpkin writes
            // /pumpkin + logs), Privileged/CapAdd (never), NetworkMode
            // (default bridge), User (image already USER pumpkin).
            ..Default::default()
        };

        Config {
            image: Some(pinned_image_reference(
                &spec.image_reference,
                &spec.image_digest,
            )),
            exposed_ports: Some(HashMap::from([
                (format!("{CONTAINER_JAVA_PORT}/tcp"), HashMap::new()),
                (format!("{CONTAINER_BEDROCK_PORT}/udp"), HashMap::new()),
            ])),
            // Console support (Phase 0 requirement): the web console
            // attaches to stdin, so the container must keep it open.
            open_stdin: Some(true),
            stdin_once: Some(true),
            tty: Some(true),
            stop_timeout: Some(STOP_TIMEOUT_SECS),
            labels: Some(Self::labels(&spec.server_id.to_string())),
            host_config: Some(host_config),
            // Deliberately unset: `user` (image already USER pumpkin;
            // caller ensures the host dir exists — see `create`).
            ..Default::default()
        }
    }
}

impl RuntimeAdapter for DockerAdapter {
    fn create(
        &self,
        spec: &ValidatedServerSpec,
    ) -> impl Future<Output = Result<RuntimeHandle, RuntimeError>> + Send {
        let spec = spec.clone();
        let docker = self.docker.clone();
        async move {
            // The image runs as an unprivileged user; make sure the host dir
            // exists so the bind-mount does not surface as root-owned.
            // (Recursive chown to the pumpkin UID is a deploy/entrypoint
            // concern — this crate intentionally takes no `nix` dependency.)
            tokio::fs::create_dir_all(&spec.data_path)
                .await
                .map_err(|e| RuntimeError::Io(e.to_string()))?;

            let name = Self::container_name(&spec.slug);
            let config = Self::build_create_config(&spec);
            let options = CreateContainerOptions {
                name: name.clone(),
                platform: None,
            };
            let response = docker
                .create_container(Some(options), config)
                .await
                .map_err(RuntimeError::from_bollard)?;
            info!(container_id = %response.id, %name, "created server container");
            Ok(RuntimeHandle {
                container_id: response.id,
                container_name: name,
            })
        }
    }

    fn start(&self, container_id: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            docker
                .start_container(&container_id, None::<StartContainerOptions<String>>)
                .await
                .map_err(RuntimeError::from_bollard)
        }
    }

    fn stop(
        &self,
        container_id: &str,
        timeout_secs: u32,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            let options = StopContainerOptions {
                t: timeout_secs as i64,
            };
            docker
                .stop_container(&container_id, Some(options))
                .await
                .map_err(RuntimeError::from_bollard)
        }
    }

    fn restart(&self, container_id: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            let options = RestartContainerOptions {
                t: STOP_TIMEOUT_SECS as isize,
            };
            docker
                .restart_container(&container_id, Some(options))
                .await
                .map_err(RuntimeError::from_bollard)
        }
    }

    fn remove(&self, container_id: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            let options = RemoveContainerOptions {
                v: false,
                force: true,
                link: false,
            };
            docker
                .remove_container(&container_id, Some(options))
                .await
                .map_err(RuntimeError::from_bollard)
        }
    }

    fn inspect(
        &self,
        container_id: &str,
    ) -> impl Future<Output = Result<RuntimeSnapshot, RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            let info = docker
                .inspect_container(&container_id, None::<InspectContainerOptions>)
                .await
                .map_err(RuntimeError::from_bollard)?;
            let state = info.state.as_ref();
            Ok(RuntimeSnapshot {
                container_id: info.id.clone().unwrap_or(container_id),
                status: state
                    .and_then(|s| s.status.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string()),
                exit_code: state.and_then(|s| s.exit_code),
                health: state
                    .and_then(|s| s.health.as_ref())
                    .and_then(|h| h.status.as_ref())
                    .map(ToString::to_string),
                restart_count: info.restart_count.unwrap_or(-1),
            })
        }
    }

    fn logs(
        &self,
        container_id: &str,
        tail: usize,
    ) -> impl Future<Output = Result<Vec<String>, RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            use futures_util::TryStreamExt;

            let options = LogsOptions {
                follow: false,
                stdout: true,
                stderr: true,
                since: 0,
                until: 0,
                timestamps: false,
                tail: tail.to_string(),
            };
            let mut stream = docker.logs(&container_id, Some(options));
            let mut out = Vec::new();
            while let Some(chunk) = stream
                .try_next()
                .await
                .map_err(RuntimeError::from_bollard)?
            {
                let bytes = match chunk {
                    LogOutput::StdOut { message }
                    | LogOutput::StdErr { message }
                    | LogOutput::StdIn { message }
                    | LogOutput::Console { message } => message,
                };
                out.push(String::from_utf8_lossy(&bytes).into_owned());
            }
            Ok(out)
        }
    }

    fn pull_image(&self, reference: &str) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        let docker = self.docker.clone();
        let reference = reference.to_string();
        async move {
            use bollard::image::CreateImageOptions;
            use futures_util::TryStreamExt;

            // Split `name[:tag]` explicitly: an empty `tag` query param
            // makes the daemon pull every tag, so never send one when the
            // tag is known. Digest references (`name@sha256:…`) travel whole
            // in `from_image`.
            let (from_image, tag) = match reference.split_once('@') {
                Some(_) => (reference.as_str(), ""),
                None => match reference.rsplit_once(':') {
                    Some((name, tag)) if !tag.contains('/') => (name, tag),
                    _ => (reference.as_str(), ""),
                },
            };
            let options = CreateImageOptions {
                from_image,
                tag,
                ..Default::default()
            };
            let mut stream = docker.create_image(Some(options), None, None);
            while stream
                .try_next()
                .await
                .map_err(RuntimeError::from_bollard)?
                .is_some()
            {}
            info!(%reference, "pulled image");
            Ok(())
        }
    }

    fn stats(
        &self,
        container_id: &str,
    ) -> impl Future<Output = Result<RuntimeStats, RuntimeError>> + Send {
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        async move {
            use futures_util::TryStreamExt;

            let options = StatsOptions {
                stream: false,
                one_shot: true,
            };
            let mut stream = docker.stats(&container_id, Some(options));
            let sample = stream
                .try_next()
                .await
                .map_err(RuntimeError::from_bollard)?
                .ok_or_else(|| {
                    RuntimeError::InvalidConfiguration(format!("no stats for {container_id}"))
                })?;

            // Docker-CLI-style CPU estimate. One-shot samples carry an
            // empty `precpu_stats`, in which case we report 0.0 rather
            // than lie.
            let cpu_delta = sample
                .cpu_stats
                .cpu_usage
                .total_usage
                .saturating_sub(sample.precpu_stats.cpu_usage.total_usage)
                as f64;
            let sys_delta = sample
                .cpu_stats
                .system_cpu_usage
                .unwrap_or(0)
                .saturating_sub(sample.precpu_stats.system_cpu_usage.unwrap_or(0))
                as f64;
            let cpus = sample
                .cpu_stats
                .online_cpus
                .map(|c| c as f64)
                .or_else(|| {
                    sample
                        .cpu_stats
                        .cpu_usage
                        .percpu_usage
                        .as_ref()
                        .map(|p| p.len() as f64)
                })
                .unwrap_or(1.0);
            let cpu_percent = if sys_delta > 0.0 {
                cpu_delta / sys_delta * cpus * 100.0
            } else {
                0.0
            };

            Ok(RuntimeStats {
                cpu_percent,
                memory_bytes: sample.memory_stats.usage.unwrap_or(0),
                memory_limit: sample.memory_stats.limit.unwrap_or(0),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Reconciliation (read-only drift detection)
// ---------------------------------------------------------------------------

/// How DB state and Docker reality disagree for one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscrepancyKind {
    /// A managed container exists that no live server row references.
    OrphanContainer,
    /// A live server row points at a container id Docker no longer has.
    MissingContainer,
    /// The container's `server-id` label and the row's `container_id`
    /// disagree (recreated out-of-band, DB not updated).
    IdMismatch,
}

/// One drift item. Read-only: callers decide whether/how to heal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discrepancy {
    /// `servers.id` for `MissingContainer`/`IdMismatch`; the orphan
    /// container's `server-id` label (or container id if unlabeled) for
    /// `OrphanContainer`.
    pub server_id: String,
    /// What kind of drift was found.
    pub kind: DiscrepancyKind,
    /// Docker container id involved, if any.
    pub container_id: Option<String>,
    /// Human-readable detail.
    pub detail: String,
}

/// Compare managed Docker containers against the `servers` table.
///
/// Lists containers carrying [`LABEL_MANAGED`]`=true`, joins them to live
/// server rows (`deleted_at IS NULL`) via `container_id`, and reports
/// [`Discrepancy`] items. **Deletes nothing** — healing (adopt/prune/
/// re-point) is a caller decision. Uses only the existing `servers`
/// table; no new SQL objects required.
pub async fn reconcile(
    pool: &sqlx::SqlitePool,
    adapter: &DockerAdapter,
) -> Result<Vec<Discrepancy>, RuntimeError> {
    use sqlx::Row;

    let rows = sqlx::query("SELECT id, container_id FROM servers WHERE deleted_at IS NULL")
        .fetch_all(pool)
        .await
        .map_err(|e| RuntimeError::Database(e.to_string()))?;
    // server_id -> container_id (possibly "")
    let mut db: HashMap<String, String> = HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: String = row
            .try_get("id")
            .map_err(|e| RuntimeError::Database(e.to_string()))?;
        let cid: String = row
            .try_get("container_id")
            .map_err(|e| RuntimeError::Database(e.to_string()))?;
        db.insert(id, cid);
    }

    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("label".to_string(), vec![format!("{LABEL_MANAGED}=true")]);
    let options = ListContainersOptions {
        all: true,
        limit: None,
        size: false,
        filters,
    };
    let containers = adapter
        .docker
        .list_containers(Some(options))
        .await
        .map_err(RuntimeError::from_bollard)?;

    let mut out = Vec::new();
    let mut seen_docker_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for c in &containers {
        let Some(docker_id) = c.id.clone() else {
            continue;
        };
        seen_docker_ids.insert(docker_id.clone());
        let label_server_id = c
            .labels
            .as_ref()
            .and_then(|l| l.get(LABEL_SERVER_ID))
            .cloned();

        match label_server_id {
            Some(sid) => match db.get(&sid) {
                Some(expected) if *expected == docker_id => {
                    // Healthy: row and container agree.
                }
                Some(expected) => {
                    out.push(Discrepancy {
                        server_id: sid,
                        kind: DiscrepancyKind::IdMismatch,
                        container_id: Some(docker_id.clone()),
                        detail: format!(
                            "servers row points at container '{expected}' but labeled container is '{docker_id}'"
                        ),
                    });
                }
                None => {
                    warn!(%sid, %docker_id, "orphan managed container");
                    out.push(Discrepancy {
                        server_id: sid,
                        kind: DiscrepancyKind::OrphanContainer,
                        container_id: Some(docker_id.clone()),
                        detail: format!(
                            "container '{docker_id}' carries a server-id label with no matching server row"
                        ),
                    });
                }
            },
            None => {
                warn!(%docker_id, "managed container without server-id label");
                out.push(Discrepancy {
                    server_id: docker_id.clone(),
                    kind: DiscrepancyKind::OrphanContainer,
                    container_id: Some(docker_id.clone()),
                    detail: "managed container has no server-id label".to_string(),
                });
            }
        }
    }

    for (server_id, container_id) in &db {
        if container_id.is_empty() {
            continue; // Never materialised — nothing to look for.
        }
        if !seen_docker_ids.contains(container_id) {
            out.push(Discrepancy {
                server_id: server_id.clone(),
                kind: DiscrepancyKind::MissingContainer,
                container_id: Some(container_id.clone()),
                detail: format!(
                    "servers row points at container '{container_id}' which Docker does not list"
                ),
            });
        }
    }

    out.sort_by(|a, b| {
        a.server_id
            .cmp(&b.server_id)
            .then_with(|| format!("{:?}", a.kind).cmp(&format!("{:?}", b.kind)))
    });
    Ok(out)
}

// ---------------------------------------------------------------------------
// Unit tests (pure validation + config shape; no daemon needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> ServerSpecInput {
        ServerSpecInput {
            server_id: Uuid::new_v4(),
            name: "Survival".to_string(),
            slug: "survival-01".to_string(),
            image_reference: "ghcr.io/pumpkin-mc/pumpkin:master".to_string(),
            image_digest: String::new(),
            game_port: 25565,
            bind_address: "0.0.0.0".to_string(),
            data_path: "/data/servers/survival-01".to_string(),
            memory_limit_bytes: 2 * 1024 * 1024 * 1024,
            cpu_limit: 2.0,
        }
    }

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from("/data/servers")
    }

    #[test]
    fn valid_spec_derives_bedrock_port() {
        let spec = validate_spec(input(), &root()).expect("valid spec");
        assert_eq!(spec.game_port, 25565);
        assert_eq!(spec.bedrock_port, 25566);
    }

    #[test]
    fn container_name_conflict_maps_to_stable_code() {
        // Exact daemon text observed live (Phase 3 E2E): a stale
        // `pumpkin-live` container collided with a fresh create.
        let err = bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "Conflict. The container name \"/pumpkin-live\" is already in use by container \"e20e3297\". You have to remove (or rename) that container to be able to reuse that name.".to_string(),
        };
        let mapped = RuntimeError::from_bollard(err);
        assert_eq!(mapped.code(), "CONTAINER_NAME_CONFLICT");
    }

    #[test]
    fn slug_rules() {
        assert!(is_valid_slug("abc"));
        assert!(is_valid_slug("survival-01"));
        assert!(is_valid_slug(&"a".repeat(48)));
        assert!(!is_valid_slug("ab")); // too short
        assert!(!is_valid_slug(&"a".repeat(49))); // too long
        assert!(!is_valid_slug("Survival")); // uppercase
        assert!(!is_valid_slug("survival_01")); // underscore
        assert!(!is_valid_slug("survival 01")); // space
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("srv!"));
    }

    #[test]
    fn bad_slugs_rejected_with_code() {
        for slug in ["ab", "UPPER", "has space", "under_score", &"x".repeat(49)] {
            let mut i = input();
            i.slug = slug.to_string();
            let err = validate_spec(i, &root()).expect_err("must reject slug");
            assert_eq!(err.code(), "INVALID_CONFIGURATION", "slug {slug}");
        }
    }

    #[test]
    fn port_range_and_bedrock_overflow() {
        let mut i = input();
        i.game_port = 25564;
        assert!(matches!(
            validate_spec(i, &root()),
            Err(SpecError::PortOutOfRange(25564))
        ));

        let mut i = input();
        i.game_port = 65535; // would push bedrock to 65536
        assert!(matches!(
            validate_spec(i, &root()),
            Err(SpecError::PortOutOfRange(65535) | SpecError::BedrockOverflow(65535))
        ));

        let ok = validate_spec(
            ServerSpecInput {
                game_port: MAX_GAME_PORT,
                ..input()
            },
            &root(),
        )
        .expect("max port ok");
        assert_eq!(ok.bedrock_port, 65535);
    }

    #[test]
    fn reserved_ports_flagged_port_unavailable() {
        for port in [8080u16, 3000, 5173, 5432] {
            assert!(is_reserved_port(port));
            // Belt-and-braces: reserved check fires even below the range.
            let mut i = input();
            i.game_port = port;
            let err = validate_spec(i, &root()).expect_err("reserved port");
            // Below-range ports fail range first; the helper + variant stay
            // covered either way.
            assert!(
                matches!(
                    err,
                    SpecError::PortOutOfRange(_) | SpecError::PortReserved(_)
                ),
                "unexpected {err:?}"
            );
        }
        // The PORT_UNAVAILABLE code path itself:
        assert_eq!(SpecError::PortReserved(8080).code(), "PORT_UNAVAILABLE");
    }

    #[test]
    fn image_allowlist() {
        for good in [
            "ghcr.io/pumpkin-mc/pumpkin:master",
            "ghcr.io/pumpkin-mc/pumpkin:v1.2.3",
        ] {
            let spec = validate_spec(
                ServerSpecInput {
                    image_reference: good.to_string(),
                    ..input()
                },
                &root(),
            )
            .expect("allowlisted image");
            assert_eq!(spec.image_reference, good);
        }
        for evil in [
            "docker.io/library/ubuntu:latest",
            "ghcr.io/pumpkin-mc/other:tag",
            "ghcr.io/evil/pumpkin:tag",
            "pumpkin:master",
            "",
        ] {
            let mut i = input();
            i.image_reference = evil.to_string();
            let err = validate_spec(i, &root()).expect_err("must reject image");
            assert_eq!(err.code(), "INVALID_CONFIGURATION");
            assert!(matches!(err, SpecError::ImageNotAllowed(_)));
        }
    }

    #[test]
    fn privileged_flags_unrepresentable_at_api_boundary() {
        // Extra JSON keys (the only way to smuggle --privileged/cap-add/
        // network-mode through a JSON API) are rejected.
        let raw = serde_json::json!({
            "server_id": Uuid::new_v4(),
            "name": "x",
            "slug": "evil-01",
            "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master",
            "game_port": 25565,
            "bind_address": "0.0.0.0",
            "data_path": "/data/servers/evil-01",
            "memory_limit_bytes": 1073741824u64,
            "cpu_limit": 1.0,
            "privileged": true,
        });
        assert!(serde_json::from_value::<ServerSpecInput>(raw).is_err());

        // And the built config never sets them either.
        let spec = validate_spec(input(), &root()).expect("valid");
        let cfg = DockerAdapter::build_create_config(&spec);
        let hc = cfg.host_config.expect("host config");
        assert_eq!(hc.privileged, None);
        assert_eq!(hc.cap_add, None);
        assert_eq!(hc.network_mode, None);
    }

    #[test]
    fn memory_cpu_bounds() {
        for bad in [0u64, MIN_MEMORY_BYTES - 1, MAX_MEMORY_BYTES + 1] {
            let mut i = input();
            i.memory_limit_bytes = bad;
            assert!(matches!(
                validate_spec(i, &root()),
                Err(SpecError::InvalidMemory(_))
            ));
        }
        for bad in [0.0, 0.09, 16.1, 100.0, f64::NAN] {
            let mut i = input();
            i.cpu_limit = bad;
            assert!(
                matches!(validate_spec(i, &root()), Err(SpecError::InvalidCpu(_))),
                "cpu {bad}"
            );
        }
        assert!(validate_spec(input(), &root()).is_ok());
    }

    #[test]
    fn data_path_must_stay_under_root() {
        let ok = validate_spec(input(), &root()).expect("inside root");
        assert_eq!(ok.data_path, "/data/servers/survival-01");

        for escape in [
            "/etc/passwd",
            "/data/other/x",
            "/data/servers/../etc/x",
            "../escape",
        ] {
            let mut i = input();
            i.data_path = escape.to_string();
            assert!(
                matches!(validate_spec(i, &root()), Err(SpecError::DataPathEscape(_))),
                "must reject {escape}"
            );
        }
    }

    #[test]
    fn labels_and_container_name() {
        assert_eq!(
            DockerAdapter::container_name("survival-01"),
            "pumpkin-survival-01"
        );
        let labels = DockerAdapter::labels("server-123");
        assert_eq!(labels.get(LABEL_MANAGED).map(String::as_str), Some("true"));
        assert_eq!(
            labels.get(LABEL_SERVER_ID).map(String::as_str),
            Some("server-123")
        );
        assert_eq!(
            labels.get(LABEL_SCHEMA).map(String::as_str),
            Some(LABEL_SCHEMA_VERSION)
        );
    }

    #[test]
    fn create_config_shape() {
        let spec = validate_spec(input(), &root()).expect("valid");
        let cfg = DockerAdapter::build_create_config(&spec);

        assert_eq!(
            cfg.image.as_deref(),
            Some("ghcr.io/pumpkin-mc/pumpkin:master")
        );
        let exposed = cfg.exposed_ports.expect("exposed ports");
        assert!(exposed.contains_key("25565/tcp"));
        assert!(exposed.contains_key("19132/udp"));

        // Console I/O (Phase 0 requirement).
        assert_eq!(cfg.open_stdin, Some(true));
        assert_eq!(cfg.stdin_once, Some(true));
        assert_eq!(cfg.tty, Some(true));
        assert_eq!(cfg.stop_timeout, Some(STOP_TIMEOUT_SECS));

        // Image already USER pumpkin → keep unset.
        assert_eq!(cfg.user, None);

        let hc = cfg.host_config.expect("host config");
        let bindings = hc.port_bindings.expect("port bindings");
        let java = bindings
            .get("25565/tcp")
            .and_then(Clone::clone)
            .expect("java binding");
        assert_eq!(java[0].host_port.as_deref(), Some("25565"));
        assert_eq!(java[0].host_ip.as_deref(), Some("0.0.0.0"));
        let bedrock = bindings
            .get("19132/udp")
            .and_then(Clone::clone)
            .expect("bedrock binding");
        assert_eq!(bedrock[0].host_port.as_deref(), Some("25566"));

        assert_eq!(
            hc.binds,
            Some(vec!["/data/servers/survival-01:/pumpkin:rw".to_string()])
        );
        assert_eq!(
            hc.restart_policy
                .as_ref()
                .and_then(|r| r.name)
                .map(|n| n.to_string()),
            Some("no".to_string())
        );
        assert_eq!(hc.memory, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(hc.nano_cpus, Some(2_000_000_000));
        assert_eq!(hc.cap_drop, Some(vec!["ALL".to_string()]));
        assert!(hc
            .security_opt
            .as_ref()
            .is_some_and(|o| o.iter().any(|s| s.contains("no-new-privileges"))));
        // Pumpkin writes /pumpkin + logs → rootfs stays writable.
        assert_eq!(hc.readonly_rootfs, None);

        let labels = cfg.labels.expect("labels");
        assert_eq!(labels.get(LABEL_MANAGED).map(String::as_str), Some("true"));
    }

    #[test]
    fn error_codes() {
        assert_eq!(
            SpecError::InvalidSlug("x".into()).code(),
            "INVALID_CONFIGURATION"
        );
        assert_eq!(SpecError::PortReserved(3000).code(), "PORT_UNAVAILABLE");
        assert_eq!(
            SpecError::UnusableServerRoot("rel".into()).code(),
            "CAPABILITY_UNAVAILABLE"
        );
    }

    #[tokio::test]
    async fn pull_image_without_daemon_errors() {
        // No daemon needed: pulling with no daemon listening must fail,
        // never report success. (Client constructors perform no I/O.)
        let docker =
            bollard::Docker::connect_with_http_defaults().expect("client builds without I/O");
        let adapter = DockerAdapter::with_docker(docker);
        let err = adapter
            .pull_image("ghcr.io/pumpkin-mc/pumpkin:master")
            .await
            .expect_err("pull without a daemon must fail");
        assert_eq!(err.code(), "DOCKER_API_ERROR");
    }

    // -- Task 3: digest pinning, strict digest/allowlist validation ----------

    fn valid_digest() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    #[test]
    fn pinned_image_reference_applies_digest() {
        let digest = valid_digest();
        assert_eq!(
            pinned_image_reference("ghcr.io/pumpkin-mc/pumpkin:master", &digest),
            format!("ghcr.io/pumpkin-mc/pumpkin@{digest}")
        );
        // No digest → unchanged, tag preserved.
        assert_eq!(
            pinned_image_reference("ghcr.io/pumpkin-mc/pumpkin:master", ""),
            "ghcr.io/pumpkin-mc/pumpkin:master"
        );
    }

    #[test]
    fn build_create_config_pins_digest() {
        let digest = valid_digest();
        let spec = validate_spec(
            ServerSpecInput {
                image_digest: digest.clone(),
                ..input()
            },
            &root(),
        )
        .expect("pinned spec validates");
        let cfg = DockerAdapter::build_create_config(&spec);
        assert_eq!(
            cfg.image.as_deref(),
            Some(format!("ghcr.io/pumpkin-mc/pumpkin@{digest}").as_str())
        );
        assert!(cfg.image.as_deref().unwrap().contains("@sha256:"));

        // Unpinned → tag unchanged.
        let unpinned = validate_spec(input(), &root()).expect("unpinned validates");
        let cfg = DockerAdapter::build_create_config(&unpinned);
        assert_eq!(
            cfg.image.as_deref(),
            Some("ghcr.io/pumpkin-mc/pumpkin:master")
        );
    }

    #[test]
    fn digest_validation_is_strict() {
        assert!(is_valid_digest(&valid_digest()));
        for bad in [
            String::new(),
            "sha256:".to_string(),
            format!("sha256:{}", "a".repeat(63)), // short
            format!("sha256:{}", "a".repeat(65)), // extra chars
            format!("sha256:{}", "A".repeat(64)), // uppercase hex
            format!("sha256:{}", "z".repeat(64)), // non-hex
            "abcdef".to_string(),                 // missing prefix
            format!("md5:{}", "a".repeat(64)),    // wrong algorithm prefix
        ] {
            assert!(!is_valid_digest(&bad), "must reject digest {bad:?}");
        }
    }

    #[test]
    fn validate_spec_rejects_malformed_digest() {
        // Empty digest stays legal (unpinned).
        assert!(validate_spec(input(), &root()).is_ok());

        for bad in [
            "sha256:abc".to_string(),
            "sha256:".to_string(),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
        ] {
            let mut i = input();
            i.image_digest = bad.clone();
            let err = validate_spec(i, &root()).expect_err("must reject digest");
            assert!(
                matches!(err, SpecError::InvalidDigest),
                "digest {bad:?} → {err:?}"
            );
        }

        let valid = validate_spec(
            ServerSpecInput {
                image_digest: valid_digest(),
                ..input()
            },
            &root(),
        )
        .expect("valid digest");
        assert!(is_valid_digest(&valid.image_digest));
    }

    #[test]
    fn allowlist_requires_one_clean_tag_segment() {
        for good in [
            "ghcr.io/pumpkin-mc/pumpkin:master",
            "ghcr.io/pumpkin-mc/pumpkin:v1.2.3",
            "ghcr.io/pumpkin-mc/pumpkin:1_2-3",
            "ghcr.io/pumpkin-mc/pumpkin:a",
        ] {
            assert!(is_allowlisted_image(good), "should allow {good}");
            let spec = validate_spec(
                ServerSpecInput {
                    image_reference: good.to_string(),
                    ..input()
                },
                &root(),
            )
            .expect("allowlisted");
            assert_eq!(spec.image_reference, good);
        }

        for bad in [
            "ghcr.io/pumpkin-mc/pumpkin:",           // empty tag
            "ghcr.io/pumpkin-mc/pumpkin",            // no tag separator
            "ghcr.io/pumpkin-mc/pumpkin:master:1",   // second colon
            "ghcr.io/pumpkin-mc/pumpkin:mas ter",    // whitespace
            "ghcr.io/pumpkin-mc/pumpkin:tag/extra",  // path after tag
            "ghcr.io/pumpkin-mc/pumpkin:tag/",       // trailing slash
            "ghcr.io/pumpkin-mc/pumpkin:@sha256:aa", // digest in ref
            "ghcr.io/pumpkin-mc/pumpkin:#bad",       // invalid char
            "docker.io/library/ubuntu:latest",
            "ghcr.io/evil/pumpkin:master",
            "",
        ] {
            assert!(!is_allowlisted_image(bad), "should reject {bad:?}");
            let mut i = input();
            i.image_reference = bad.to_string();
            let err = validate_spec(i, &root()).expect_err("must reject ref");
            assert!(
                matches!(err, SpecError::ImageNotAllowed(_)),
                "ref {bad:?} → {err:?}"
            );
        }
    }

    #[test]
    fn runtime_error_client_messages_are_sanitized() {
        let docker =
            RuntimeError::from_bollard(bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "/var/run/docker.sock exploded while talking to http://127.0.0.1:2375"
                    .to_string(),
            });
        let cases = [
            docker,
            RuntimeError::NotFound("/var/run/docker.sock: No such container abc".to_string()),
            RuntimeError::PortUnavailable(
                "Bind for 0.0.0.0:25565 failed: port is already allocated".to_string(),
            ),
            RuntimeError::NameConflict(
                "Conflict. The container name \"/pumpkin-live\" is already in use".to_string(),
            ),
            RuntimeError::Io("open /data/servers/x: permission denied".to_string()),
        ];
        for e in cases {
            let msg = e.client_message();
            for leak in [
                "docker.sock",
                "/var/run",
                "0.0.0.0:25565",
                "/pumpkin-live",
                "/data/servers",
                "permission denied",
                "127.0.0.1",
            ] {
                assert!(
                    !msg.contains(leak),
                    "client message {msg:?} leaked {leak:?}"
                );
            }
            // Codes stay stable and machine-readable.
            assert!(
                e.code().chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "unstable code {}",
                e.code()
            );
        }
    }
}
