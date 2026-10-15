# Pumpkin Manager

## Complete Implementation Plan and Engineering Specification

**Document status:** Implementation blueprint\
**Product:** Pumpkin Manager\
**Target deployment:** Self-hosted Linux, Docker Compose,
CasaOS-friendly\
**Backend:** Rust\
**Frontend:** React + TypeScript\
**Primary runtime:** Pumpkin Minecraft server, one container per managed
server\
**Initial database:** SQLite\
**Document purpose:** Define the product, architecture, implementation
sequence, contracts, security boundaries, testing strategy, operational
behavior, and release criteria needed to build a production-minded
Crafty Controller alternative focused on Pumpkin MC.

------------------------------------------------------------------------

## 1. Executive Summary

Pumpkin Manager is a self-hosted web application for creating,
operating, monitoring, configuring, backing up, and maintaining one or
more Pumpkin Minecraft servers. It should offer the convenience of a
game-server control panel while remaining explicit about Pumpkin's
evolving capabilities and safe-by-default in its handling of containers,
files, credentials, and world data.

The application consists of:

1.  A responsive web UI.
2.  A Rust API and background worker.
3.  A persistence layer for users, server metadata, schedules, tasks,
    and audit events.
4.  A narrowly scoped Docker runtime adapter that manages approved
    Pumpkin containers.
5.  A version-aware Pumpkin integration layer for configuration and
    supported control features.
6.  A sandboxed file service.
7.  A backup/restore service.
8.  Monitoring and event delivery.
9.  A Docker Compose deployment designed for a small home server and
    documented for CasaOS.

The manager must not require modifications to Pumpkin itself. It should
manage Pumpkin as an external runtime through documented, tested
interfaces. Where Pumpkin does not provide a stable control interface,
the manager must expose a clearly limited capability rather than
pretending the feature exists.

### 1.1 Product principles

-   **Data safety first:** world and configuration data must survive
    manager upgrades, container recreation, and recoverable failures.
-   **Least privilege:** browser clients never receive Docker
    credentials or direct Docker socket access.
-   **Truthful status:** container existence is not equivalent to a
    ready Minecraft server.
-   **Explicit compatibility:** features are gated by verified runtime
    capabilities and version profiles.
-   **Recoverable operations:** destructive actions require confirmation
    and create recovery points where practical.
-   **Operational simplicity:** a single-node deployment should work
    without Kubernetes, Redis, or a multi-service distributed
    architecture.
-   **Observable behavior:** operations produce structured logs, status
    transitions, progress events, and audit records.
-   **Incremental delivery:** every phase ends with demonstrable, tested
    functionality.

### 1.2 Non-goals for the first release

-   Hosting a public multi-tenant server provider.
-   Kubernetes orchestration.
-   Automatic compatibility with every Paper, Bukkit, Spigot, Fabric, or
    Forge plugin.
-   Modifying or forking Pumpkin to add missing APIs.
-   Guaranteeing zero-downtime updates for a single server process.
-   Arbitrary host shell access through the web UI.
-   Cloud account integration or mandatory external services.
-   Promising player counts, TPS, RCON, or command execution unless
    verified for the selected Pumpkin build.

------------------------------------------------------------------------

## 2. Repository and Runtime Reconnaissance

This is a required engineering phase, not optional preliminary research.
Pumpkin is evolving software, and assumptions about its command
interface, configuration schema, image tags, or graceful shutdown
behavior must not be embedded throughout the application.

### 2.1 Official starting points

Inspect and record findings from:

-   Pumpkin source: https://github.com/Pumpkin-MC/Pumpkin
-   Official documentation: https://docs.pumpkinmc.org/
-   Configuration documentation: https://docs.pumpkinmc.org/config/basic
-   Official Docker guidance linked from the documentation/repository.

Treat upstream documentation and source as the authoritative references.
Issue comments, forum posts, and community examples may help with
discovery but should not override verified behavior.

### 2.2 Produce a compatibility report

Create `docs/compatibility/pumpkin-runtime.md` containing:

-   Repository URL and inspection date.
-   Tested commit or release.
-   Tested container image and immutable digest, if available.
-   Supported CPU architecture(s).
-   Exact startup command and working directory.
-   Required environment variables and mounts.
-   Default and configurable ports.
-   Configuration file path and format.
-   How the process receives shutdown signals.
-   Whether graceful shutdown works and how long it takes.
-   Log destination and whether stdout/stderr are usable.
-   Whether the image includes a health check.
-   Whether Minecraft status ping works.
-   Whether player counts can be obtained reliably.
-   Whether RCON exists, is enabled, and is documented for that build.
-   Whether commands can be sent through stdin or another documented
    interface.
-   Whether runtime version can be queried.
-   Known incompatibilities and test evidence.

For each capability use one of: `verified`, `verified-with-limitations`,
`not-supported`, `unknown`. Never convert `unknown` into `supported` in
the UI.

### 2.3 Compatibility smoke tests

Run tests against a disposable instance:

1.  Pull the selected image.
2.  Start with a clean data directory.
3.  Confirm configuration generation and default behavior.
4.  Confirm the process remains alive and logs are available.
5.  Test protocol status ping from outside and inside the Docker
    network.
6.  Stop gracefully and record exit code and elapsed time.
7.  Kill the process deliberately and verify recovery behavior.
8.  Recreate the container while preserving its data mount.
9.  Confirm world/configuration persistence.
10. Test invalid configuration behavior.
11. Test container startup on the target architecture.
12. Verify port and volume assumptions.
13. Test whether any command interface is genuinely usable.
14. Record image digest and reproducible commands.

Do not run destructive tests against a real world. Use a disposable test
volume.

### 2.4 Version policy

-   Do not rely on a mutable `master` tag for a production deployment.
-   Support an explicit image reference, preferably pinned to a release
    and/or digest.
-   Make the configured image visible to the owner.
-   Store the image reference used by each server.
-   Keep a known-good image reference for rollback.
-   Do not automatically upgrade all instances when the manager updates.
-   Show a warning when a configured image has not passed the
    compatibility suite.
-   Maintain capability profiles by tested Pumpkin version or image
    digest.

The exact Pumpkin image and version must be selected during Phase 0
after testing upstream's current guidance.

------------------------------------------------------------------------

## 3. Product Scope

### 3.1 Required capabilities

**Server lifecycle** - Create and configure a server instance. - Start,
stop, restart, and inspect a server. - Track desired state separately
from observed state. - Recover state after manager restart. - Display
useful failure diagnostics. - Support safe deletion with separate
options for retaining or deleting data.

**Dashboard** - Show total, running, stopped, starting, unhealthy, and
failed instances. - Display uptime, container state, resource usage,
port, image/version, and last operation. - Show warnings for disk
pressure, failed backups, incompatible images, and repeated crashes. -
Provide search, filtering, and sorting.

**Logs and console** - Stream stdout/stderr logs. - Filter by time and
text. - Fetch recent log history. - Support copy and download. - Provide
command input only when a verified Pumpkin command interface exists. -
Clearly label read-only mode otherwise.

**Configuration** - Edit `pumpkin.toml` and other explicitly supported
configuration files. - Provide a form for known settings and a raw TOML
editor. - Validate syntax before saving. - Preserve unknown keys. - Show
diffs. - Save versioned pre-change copies. - Indicate when a restart is
required.

**File manager** - Browse, upload, download, rename, create, and delete
files inside a server's assigned data root. - Display file sizes and
modification times. - Support safe world import/export. - Enforce path
and size limits.

**Backups** - Manual and scheduled backups. - Configurable retention. -
Progress and failure reporting. - Integrity checks. - Download and
restore. - Restore preview and pre-restore backup. - Clear consistency
guarantees.

**Monitoring** - Container CPU and memory usage. - Host memory, CPU,
disk capacity and free space. - Uptime, restart count, readiness and
protocol status. - Player count only when supported. - Historical
metrics with a bounded retention policy.

**Users and security** - Owner, administrator, and viewer roles. -
Server-level permissions where appropriate. - Secure authentication and
session handling. - Audit trail for sensitive actions. - Rate limiting
and login throttling.

**Deployment and maintenance** - Docker Compose deployment. - CasaOS
installation guide. - Persistent volumes and documented port mappings. -
Health checks. - Backup/restore of the manager's own database and
settings. - Upgrade and rollback procedures.

### 3.2 Later enhancements

-   Notifications through configurable providers.
-   Server templates and one-click creation.
-   Import of existing Pumpkin directories.
-   Read-only sharing of status pages.
-   More runtime adapters.
-   Remote worker nodes, only if there is a demonstrated requirement.
-   Optional player allowlist/ban management if Pumpkin exposes a stable
    supported mechanism.
-   Optional plugin/mod management only where compatible with Pumpkin
    and backed by verified behavior.

### 3.3 Explicitly unsupported unless verified

Do not advertise any of the following as available until tested against
the selected Pumpkin version:

-   RCON.
-   Arbitrary console commands.
-   Player management APIs.
-   TPS metrics.
-   Plugin installation or Bukkit compatibility.
-   Hot reload of configuration.
-   Graceful shutdown through a particular signal.
-   Automatic update compatibility.
-   A specific health endpoint or status command.

The UI should use capability flags rather than guessing from the runtime
name.

------------------------------------------------------------------------

## 4. User Personas and Main Workflows

### 4.1 Home server owner

Needs to create a server, share its address, monitor uptime, update
configuration, and recover from problems without SSH.

Primary workflow: 1. Open dashboard. 2. Create a server with a name,
version/image, port, memory limit, and initial configuration. 3. Review
the generated configuration and network exposure. 4. Start the server.
5. Confirm readiness. 6. Copy the connection address. 7. Schedule
backups. 8. Receive clear warnings when an operation fails.

### 4.2 Server administrator

Needs to inspect logs, change settings, manage files, restore a backup,
and control several instances.

Primary workflow: 1. Select a server. 2. Inspect current state and
recent events. 3. Edit configuration. 4. Review a diff and validation
result. 5. Save and restart if needed. 6. Verify the server becomes
ready. 7. Review the audit event.

### 4.3 Read-only viewer

Can inspect status, metrics, and logs but cannot mutate files, settings,
users, or server lifecycle.

### 4.4 Owner/operator

Can manage users, global settings, Docker runtime configuration, backup
storage, image policies, and application recovery.

------------------------------------------------------------------------

## 5. System Architecture

### 5.1 Logical architecture

``` mermaid
flowchart TD
    Browser[React + TypeScript UI]
    Proxy[Optional HTTPS reverse proxy]
    API[Rust Axum API]
    Auth[Authentication and RBAC]
    DB[(SQLite)]
    Lifecycle[Lifecycle coordinator]
    Runtime[Docker runtime adapter]
    Pumpkin[Pumpkin containers]
    Files[Sandboxed file service]
    Config[Version-aware config service]
    Backup[Backup and restore workers]
    Metrics[Metrics collector]
    Events[Event broker / WebSockets]
    Audit[Audit event writer]

    Browser <--> Proxy
    Proxy <--> API
    API --> Auth
    API --> DB
    API --> Lifecycle
    API --> Files
    API --> Config
    API --> Backup
    API --> Metrics
    API <--> Events
    Lifecycle --> Runtime
    Runtime --> Pumpkin
    Files --> Volumes[Per-server data volumes]
    Config --> Volumes
    Backup --> Volumes
    Metrics --> Runtime
    API --> Audit
    Audit --> DB
```

### 5.2 Deployment model

The manager runs as a control-plane container. Each managed Pumpkin
server runs in a separate container with its own persistent data mount,
resource limits, network configuration, and lifecycle.

Benefits: - A server crash does not crash the manager. - Each instance
has independent resource limits. - Server upgrades and rollbacks can be
isolated. - Backups and data deletion can be scoped per instance. -
Runtime adapters remain replaceable.

Do not run multiple unrelated Pumpkin processes inside one managed
container for the initial release.

### 5.3 Trust boundaries

1.  **Browser:** untrusted; can only call authenticated application
    endpoints.
2.  **API:** enforces authorization, validation, rate limits, and audit.
3.  **Runtime adapter:** privileged subsystem with a narrow operation
    surface.
4.  **Docker daemon:** host-level authority; treat access as equivalent
    to root.
5.  **Server container:** untrusted workload from the manager's
    perspective.
6.  **Persistent data:** user-controlled content; never execute uploaded
    content on the host.
7.  **Backup storage:** potentially sensitive and large; apply
    permissions, retention and capacity controls.

### 5.4 Docker socket security

The Docker Engine API can control the host. Therefore:

-   Never expose the Docker socket to a browser.
-   Never expose an unrestricted Docker proxy endpoint.
-   Do not let clients submit arbitrary Docker API paths, container
    create options, mounts, devices, capabilities, or host networking
    flags.
-   Construct container definitions from validated application models.
-   Allow only images and runtime options approved by server policy.
-   Use a dedicated network for control-plane communication.
-   Avoid privileged containers and unnecessary capabilities.
-   Do not mount the host root filesystem into the manager.
-   If a Docker socket proxy is used, allow only the API operations
    needed by the manager.
-   Document that Docker socket access remains a major trust boundary
    even with a proxy.

For a single-host home deployment, a direct socket mount may be
operationally simplest, but it must be isolated inside the backend and
clearly documented as privileged. Prefer a restricted proxy if its
maintenance cost is acceptable.

### 5.5 Process and task model

Use Tokio for async I/O, but do not let long-running operations live
only inside an HTTP request handler.

Represent operations such as create, start, stop, backup, restore, and
update as tasks: - `queued` - `running` - `succeeded` - `failed` -
`cancel_requested` - `cancelled`

Each task has an ID, target server, actor, start/end times, progress,
and structured error. Persist task state so the UI can recover after
refresh or API restart.

Use a bounded worker/semaphore strategy so multiple backups or image
pulls cannot overwhelm the host.

------------------------------------------------------------------------

## 6. Technology Choices

### 6.1 Frontend

-   React + TypeScript.
-   Vite build system.
-   Tailwind CSS and a consistent component system such as shadcn/ui.
-   TanStack Query for server state and cache invalidation.
-   React Router for navigation.
-   A typed API client generated from OpenAPI where practical.
-   WebSocket client for events and logs.
-   A tested TOML editor component with a raw-edit fallback.
-   Accessible forms, focus states, keyboard navigation, and responsive
    layouts.

Design direction: modern, minimalist dark interface with restrained
green accents, clear status labels, and no excessive neon/glow effects.

### 6.2 Backend

-   Rust stable toolchain, pinned through `rust-toolchain.toml`.
-   Axum for HTTP routes.
-   Tokio for async execution.
-   Serde for request/response serialization.
-   SQLx with SQLite and migrations.
-   `tracing` and structured JSON logs.
-   `tower` middleware for timeouts, request IDs, CORS where needed, and
    rate limits.
-   A maintained Docker Engine API client, wrapped behind an internal
    trait.
-   TOML parsing/serialization library selected after compatibility
    tests.
-   Password hashing using Argon2id or another well-maintained password
    hashing implementation.
-   Session tokens generated from a cryptographically secure source.
-   OpenAPI generation or a maintained API schema.

Pin dependency versions and use automated dependency auditing. Avoid
writing custom cryptography.

### 6.3 Database

SQLite is suitable for the first single-host release. Enable foreign
keys, use transactions, define migration policy, and test backup/restore
of the database. Use WAL mode only after verifying filesystem and backup
behavior for the chosen persistent volume.

Keep the database separate from server world directories.

### 6.4 Container runtime

Use a `RuntimeAdapter` trait so the domain layer does not depend
directly on Docker-specific types. Initial implementation: Docker Engine
adapter. Future option: native process adapter.

### 6.5 Metrics

Use Docker's container statistics and host filesystem/memory sources.
Clearly distinguish: - unavailable, - stale, - zero, - unsupported.

Store metrics at a configurable interval and enforce retention. Avoid an
always-growing SQLite table.

------------------------------------------------------------------------

## 7. Suggested Repository Layout

``` text
pumpkin-manager/
├── apps/
│   └── web/
│       ├── src/
│       │   ├── app/
│       │   ├── components/
│       │   ├── features/
│       │   │   ├── dashboard/
│       │   │   ├── servers/
│       │   │   ├── console/
│       │   │   ├── files/
│       │   │   ├── config/
│       │   │   ├── backups/
│       │   │   ├── monitoring/
│       │   │   └── users/
│       │   ├── lib/
│       │   └── routes/
│       ├── tests/
│       └── package.json
├── crates/
│   ├── api/
│   ├── app-core/
│   ├── domain/
│   ├── runtime/
│   ├── runtime-docker/
│   ├── pumpkin-adapter/
│   ├── storage/
│   ├── auth/
│   ├── file-service/
│   ├── config-service/
│   ├── backup-service/
│   ├── scheduler/
│   ├── metrics/
│   ├── events/
│   └── audit/
├── migrations/
├── tests/
│   ├── integration/
│   ├── fixtures/
│   └── e2e/
├── deploy/
│   ├── docker-compose.yml
│   ├── docker-compose.dev.yml
│   ├── Dockerfile.backend
│   ├── Dockerfile.web
│   └── .env.example
├── docs/
│   ├── architecture.md
│   ├── compatibility/
│   │   └── pumpkin-runtime.md
│   ├── security.md
│   ├── api.md
│   ├── installation-casaos.md
│   ├── backup-restore.md
│   ├── operations.md
│   └── development.md
├── scripts/
├── rust-toolchain.toml
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE
└── .github/
    └── workflows/
```

Keep the number of crates proportionate to real boundaries. It is
acceptable to begin with fewer crates and split only when a module has a
stable contract. Do not create a distributed system disguised as a
workspace.

------------------------------------------------------------------------

## 8. Domain Model and Database Schema

Use UUIDs or similarly robust opaque IDs for application-owned entities.
Use UTC timestamps in storage and ISO 8601 in the API.

### 8.1 `users`

-   `id`
-   `username` (unique, normalized)
-   `password_hash`
-   `role`
-   `is_active`
-   `created_at`
-   `updated_at`
-   `last_login_at`

Never store plaintext passwords.

### 8.2 `sessions`

-   `id`
-   `user_id`
-   `token_hash` or another server-side session identifier
-   `created_at`
-   `expires_at`
-   `last_seen_at`
-   `revoked_at`
-   optional user-agent metadata, subject to privacy policy

Prefer an opaque cookie-based session for a same-origin web app. Use
`HttpOnly`, `Secure` when HTTPS is active, and an appropriate `SameSite`
policy. Do not store long-lived authentication secrets in localStorage.

### 8.3 `servers`

-   `id`
-   `name`
-   `slug`
-   `description`
-   `runtime_type`
-   `runtime_version`
-   `image_reference`
-   `image_digest`
-   `container_id`
-   `data_path`
-   `game_port`
-   `bind_address`
-   `desired_state`
-   `observed_state`
-   `created_by`
-   `created_at`
-   `updated_at`
-   `deleted_at` for soft deletion if needed

Enforce uniqueness for server names/slugs as appropriate and for
allocated host ports.

### 8.4 `server_settings`

-   `server_id`
-   `memory_limit_bytes`
-   `cpu_limit`
-   `restart_policy`
-   `auto_start`
-   `backup_policy_id`
-   `configuration_revision`
-   `created_at`
-   `updated_at`

Validate limits before creating containers. Do not accept unbounded
values from the browser.

### 8.5 `tasks`

-   `id`
-   `type`
-   `server_id`
-   `requested_by`
-   `state`
-   `progress_percent`
-   `message`
-   `error_code`
-   `error_summary`
-   `created_at`
-   `started_at`
-   `finished_at`
-   `cancel_requested_at`

Do not store secret values or full sensitive configuration in error
messages.

### 8.6 `backups`

-   `id`
-   `server_id`
-   `type`
-   `state`
-   `storage_path`
-   `size_bytes`
-   `checksum_algorithm`
-   `checksum`
-   `consistency_method`
-   `created_at`
-   `completed_at`
-   `created_by`
-   `error_summary`

### 8.7 `backup_policies`

-   `id`
-   `name`
-   `schedule_expression`
-   `timezone`
-   `retention_count`
-   `max_age_days`
-   `include_world`
-   `include_config`
-   `enabled`
-   `created_at`
-   `updated_at`

Define what happens when retention count and max age conflict. Never
delete the only verified backup because a policy was misconfigured.

### 8.8 `scheduled_tasks`

-   `id`
-   `server_id`
-   `type`
-   `schedule`
-   `timezone`
-   `enabled`
-   `next_run_at`
-   `last_run_at`
-   `created_by`

Use a maintained cron parser, validate expressions and define
daylight-saving behavior. Run schedules in UTC internally after
resolving the selected timezone.

### 8.9 `audit_events`

-   `id`
-   `actor_user_id`
-   `action`
-   `target_type`
-   `target_id`
-   `outcome`
-   `request_id`
-   `created_at`
-   redacted structured metadata

Audit logins, permission changes, lifecycle operations, configuration
changes, file mutations, restores, updates and deletions. Avoid logging
secrets or full file contents.

### 8.10 `metrics_samples`

-   `id`
-   `server_id`
-   `sampled_at`
-   `cpu_percent`
-   `memory_usage_bytes`
-   `memory_limit_bytes`
-   `network_rx_bytes`
-   `network_tx_bytes`
-   `restart_count`
-   optional player count with an explicit source/capability marker

Use bounded retention and batched inserts. Consider periodic compaction
or aggregation for longer history.

### 8.11 `app_settings`

Store non-secret application settings such as log retention, backup
defaults, session duration and metric sampling interval. Store secrets
separately with restricted filesystem permissions or a dedicated secret
mechanism.

### 8.12 Migration and integrity policy

-   Every schema change is a numbered migration.
-   Migrations run before serving requests.
-   Make backup recommendations before destructive migrations.
-   Use foreign keys and appropriate indexes.
-   Test migrations from the previous released schema.
-   Define rollback strategy; not all data migrations can be
    automatically reversed.
-   Do not edit released migrations in place.

------------------------------------------------------------------------

## 9. Backend Architecture and Contracts

### 9.1 Layering

**HTTP/API layer** - Parses and validates input. - Authenticates the
actor. - Authorizes the action. - Calls application services. - Maps
domain errors to stable HTTP responses.

**Application services** - Coordinate use cases and transactions. -
Create tasks for long-running work. - Emit audit and event records. -
Enforce invariants across repositories and runtime adapters.

**Domain layer** - Server lifecycle state machine. - Capability model. -
Resource and port allocation rules. - Backup/restore invariants. -
Permission rules. - Domain errors.

**Adapters** - Docker Engine. - SQLite. - Filesystem. - Pumpkin
configuration. - Metrics sources. - WebSocket/event delivery.

### 9.2 Runtime adapter interface

Define a narrow interface conceptually equivalent to:

``` rust
trait RuntimeAdapter {
    async fn create(&self, spec: ValidatedServerSpec) -> Result<RuntimeHandle>;
    async fn start(&self, id: &RuntimeId) -> Result<()>;
    async fn stop(&self, id: &RuntimeId, timeout: Duration) -> Result<()>;
    async fn restart(&self, id: &RuntimeId, timeout: Duration) -> Result<()>;
    async fn remove(&self, id: &RuntimeId) -> Result<()>;
    async fn inspect(&self, id: &RuntimeId) -> Result<RuntimeSnapshot>;
    async fn logs(&self, id: &RuntimeId, opts: LogOptions) -> Result<LogStream>;
    async fn stats(&self, id: &RuntimeId) -> Result<RuntimeStats>;
}
```

This is illustrative rather than a promise of exact compilable syntax;
adapt lifetimes, streams, object safety and async-trait requirements to
the selected Rust libraries. Keep Docker types out of domain models.

### 9.3 Pumpkin capability interface

Maintain a capability document/profile such as:

``` json
{
  "runtime": "pumpkin",
  "profile_version": 1,
  "capabilities": {
    "stdout_logs": "verified",
    "minecraft_status_ping": "verified",
    "toml_config": "verified",
    "remote_console": "unknown",
    "graceful_shutdown": "verified-with-limitations",
    "player_count": "verified-with-limitations"
  }
}
```

The values above are examples of the schema, not claims about a
particular Pumpkin build. Actual values must be populated from
compatibility tests.

Capabilities drive API behavior and UI affordances. A disabled feature
should explain why it is unavailable.

### 9.4 Error model

Use stable machine-readable codes, for example:

-   `AUTH_REQUIRED`
-   `PERMISSION_DENIED`
-   `SERVER_NOT_FOUND`
-   `SERVER_BUSY`
-   `INVALID_CONFIGURATION`
-   `PORT_UNAVAILABLE`
-   `RUNTIME_UNAVAILABLE`
-   `RUNTIME_OPERATION_FAILED`
-   `BACKUP_FAILED`
-   `RESTORE_REQUIRES_STOPPED_SERVER`
-   `PATH_OUTSIDE_SERVER_ROOT`
-   `CAPABILITY_UNAVAILABLE`
-   `RATE_LIMITED`

Responses should include a request ID and a safe human-readable message.
Never return raw Docker socket errors, stack traces, filesystem paths
containing secrets, or credentials to ordinary users.

### 9.5 Idempotency and concurrency

-   Start on a running server should return a stable "already running"
    result or an appropriate conflict, not spawn another container.
-   Stop on a stopped server should be safe.
-   Duplicate create requests should not silently allocate two servers.
-   Use database constraints and operation locks to prevent conflicting
    lifecycle operations.
-   Consider idempotency keys for creation, restore and update requests.
-   Use optimistic revisions for configuration saves so concurrent edits
    do not overwrite each other.
-   Reject or queue conflicting operations, such as restore while backup
    is running.

------------------------------------------------------------------------

## 10. Server Lifecycle State Machine

### 10.1 Desired state vs observed state

Maintain two distinct concepts:

-   **Desired state:** what the user asked for (`running` or `stopped`).
-   **Observed state:** what the runtime reports (`created`, `starting`,
    `running`, `stopping`, `exited`, `unhealthy`, `unknown`).

Do not conflate a user's request with actual runtime success.

### 10.2 State transitions

``` mermaid
stateDiagram-v2
    [*] --> Provisioning
    Provisioning --> Stopped: Created successfully
    Provisioning --> Failed: Creation failed
    Stopped --> Starting: Start requested
    Starting --> Running: Readiness confirmed
    Starting --> Failed: Startup timeout or crash
    Running --> Stopping: Stop requested
    Running --> Unhealthy: Health/readiness lost
    Unhealthy --> Running: Recovery confirmed
    Unhealthy --> Failed: Recovery exhausted
    Stopping --> Stopped: Process/container stopped
    Stopping --> Failed: Stop timeout or error
    Failed --> Starting: Retry
    Failed --> Stopped: Confirmed stopped
```

The actual state model should also represent `deleting`, `updating`,
`restoring`, and `unknown` as needed. A single enum may not be enough
for every operation; model long-running tasks separately.

### 10.3 Startup flow

1.  Authorize the user and validate server state.
2.  Acquire a per-server operation lock.
3.  Persist desired state as running.
4.  Create or start the container using a validated spec.
5.  Begin log capture and status polling.
6.  Check container state.
7.  Check Minecraft protocol readiness where possible.
8.  Mark running only after the defined readiness policy passes.
9.  Emit status event and audit result.
10. On failure, record a stable error and useful diagnostic tail.

Use configurable startup timeouts and backoff. Avoid tight polling
loops.

### 10.4 Stop flow

1.  Acquire the server lock.
2.  Persist a stop task and desired state.
3.  Request graceful shutdown through a verified mechanism.
4.  Wait for the configured timeout.
5.  If necessary, use Docker's configured stop signal/timeout and only
    then force termination.
6.  Record whether shutdown was graceful or forced.
7.  Confirm stopped state.
8.  Release the lock and publish events.

Never claim graceful shutdown if the process was killed.

### 10.5 Crash recovery

At application startup: - Enumerate manager-owned containers using
labels. - Reconcile them with database records. - Identify orphan
containers and missing containers. - Do not delete unknown containers
automatically. - Rebuild observed state from Docker. - Reconcile
queued/running tasks that were interrupted. - Mark ambiguous operations
for recovery or manual review. - Emit audit/system events for
discrepancies.

Use labels such as `io.pumpkin-manager.managed=true`,
`io.pumpkin-manager.server-id=<uuid>`, and a schema/version label. Never
manage containers merely because their names happen to match a pattern.

------------------------------------------------------------------------

## 11. Docker Container Specification

### 11.1 Container creation

Generate the container spec from an allowlisted domain model. The user
must not supply arbitrary Docker JSON.

Validate: - Image reference against policy. - Port availability and bind
address. - Memory and CPU limits. - Data directory allocation. - Restart
policy. - Environment variables against an allowlist. - Network mode and
exposed ports. - Volume mounts. - Container name uniqueness.

Use a persistent data mount that matches the verified Pumpkin image's
expected working directory. The official docs have described `/pumpkin`
as a persistent mount, but confirm this against the selected image
before finalizing the template.

### 11.2 Resource constraints

Provide sensible defaults but allow owners to change them: - Memory
limit. - CPU quota/limit. - Optional PID limit. - Container restart
policy. - Disk quota only if the host filesystem supports reliable
enforcement.

Explain that Docker memory limits are not the same as Minecraft heap
flags and that a Rust process does not need Java heap configuration.
Avoid presenting Java-specific flags in the Pumpkin setup flow.

### 11.3 Network policy

-   Allocate a unique host port per server.
-   Default to a specific bind address policy, with explicit UI
    explanation.
-   Do not expose the Docker control API on the game port.
-   Avoid host networking by default.
-   Support a configurable external address for connection instructions.
-   Explain router/firewall forwarding separately from Docker port
    publishing.
-   Do not automatically alter host firewall or router settings.
-   Prevent collisions with ports already used by the host.
-   Validate IPv4/IPv6 binding behavior on the target deployment.

### 11.4 Image update flow

1.  Resolve and display the target image reference.
2.  Verify compatibility policy and available disk space.
3.  Ensure a backup exists or ask the owner to create one.
4.  Pull image in a background task.
5.  Stop the instance safely.
6.  Recreate container with the same persistent data and approved
    settings.
7.  Run readiness checks.
8.  If startup fails, offer rollback to the previous image.
9.  Preserve logs and task history.
10. Never remove the previous image before a successful upgrade unless
    storage pressure requires explicit owner action.

A container image rollback does not guarantee data-format rollback. Warn
if the new Pumpkin version may mutate world or configuration data.

------------------------------------------------------------------------

## 12. Authentication, Authorization, and Security

Security is a first-class feature because a panel with Docker access can
control the host.

### 12.1 Authentication

-   First-run setup creates the initial owner account.
-   Do not ship default credentials.
-   Disable public registration by default.
-   Hash passwords with a maintained password hashing algorithm such as
    Argon2id.
-   Enforce password length and reasonable throttling.
-   Use secure, opaque server-side sessions.
-   Rotate session identifiers after login and privilege changes.
-   Revoke sessions on logout, password change, and user deactivation.
-   Set cookie flags correctly.
-   Protect cookie-authenticated mutations against CSRF.
-   Use same-origin frontend/API routing where practical.
-   Require HTTPS for remote access; document local HTTP as suitable
    only for trusted LAN access.
-   Add optional TOTP later if there is demand and the implementation
    can be maintained safely.

### 12.2 Authorization

Use centralized policy checks, not scattered role comparisons in UI
components.

Suggested global roles: - `owner` - `admin` - `viewer`

Suggested permissions: - `server.read` - `server.create` -
`server.lifecycle` - `server.delete` - `server.config.read` -
`server.config.write` - `server.files.read` - `server.files.write` -
`server.backup.create` - `server.backup.restore` - `server.update` -
`users.manage` - `settings.manage` - `audit.read`

Default-deny every sensitive route. Check server ownership/assignment if
per-server permissions are introduced.

### 12.3 File system defenses

-   Use a fixed root directory per server.
-   Resolve paths relative to the root.
-   Reject absolute paths and traversal sequences.
-   Prevent symlink traversal and symlink escape.
-   Use safe file creation patterns and atomic replacement.
-   Set upload limits and timeouts.
-   Reject unexpected archive entries during world import.
-   Protect against archive traversal (`../`) and symlink entries.
-   Never extract an archive directly into an unrestricted host path.
-   Do not follow user-controlled symlinks outside the root.

### 12.4 Input and API defenses

-   Validate all request bodies with typed models and limits.
-   Apply request size limits.
-   Apply per-user and per-IP rate limits where appropriate.
-   Use timeouts for Docker and filesystem operations.
-   Avoid arbitrary command execution and shell interpolation.
-   Use parameterized SQL.
-   Configure CORS narrowly; same-origin is preferred.
-   Avoid leaking internal errors.
-   Keep dependencies updated and scan them.
-   Set secure headers and a restrictive Content Security Policy where
    feasible.
-   Validate WebSocket authentication and permissions, including
    server-specific access.
-   Re-check authorization when subscribing to a server's live events.

### 12.5 Secrets

-   Keep passwords, session keys and provider credentials out of source
    control.
-   Supply secrets through restricted environment files or secret files,
    not frontend build variables.
-   Do not log secrets.
-   Set restrictive file permissions.
-   Document secret rotation and recovery.
-   Ensure backups of application settings do not accidentally publish
    secrets.

### 12.6 Audit

Record actor, action, target, outcome, timestamp and request ID for
sensitive operations. Avoid recording passwords, session tokens, full
configuration values containing secrets, or arbitrary uploaded content.

------------------------------------------------------------------------

## 13. Configuration Management

### 13.1 Dual editor

**Guided form** - Exposes only fields whose meaning and valid values are
verified for the selected Pumpkin profile. - Explains defaults and
restart requirements. - Does not invent fields from Vanilla or Paper
configuration.

**Raw editor** - Displays the actual file. - Syntax highlights TOML. -
Validates syntax. - Preserves unknown fields. - Shows a diff against the
saved revision. - Allows reverting to a prior revision.

### 13.2 Save transaction

1.  Read the current file and its revision/hash.
2.  Verify the submitted revision matches the current revision.
3.  Parse TOML and validate known fields.
4.  Save a versioned pre-change copy.
5.  Write to a temporary file in the same filesystem.
6.  Flush as appropriate and atomically rename.
7.  Re-read and verify the file.
8.  Increment configuration revision.
9.  Audit the change.
10. Emit an event.
11. Notify the UI if restart is required.

If the file changes outside the manager, show a conflict instead of
silently overwriting it.

### 13.3 Schema versioning

Keep schemas/profiles under version control. Associate a profile with a
tested Pumpkin release/commit or image digest. When the profile is
unknown: - Allow raw editing with a warning. - Do not show misleading
form controls. - Preserve unknown keys. - Recommend creating a backup
before saving.

------------------------------------------------------------------------

## 14. File Manager

### 14.1 Required operations

-   List directory entries.
-   Show file size, type and modification time.
-   Download a file.
-   Upload a file.
-   Create directory.
-   Rename/move within the same server root.
-   Delete with confirmation.
-   Search filenames.
-   Import/export world archives.

### 14.2 Limits and safeguards

Configurable defaults should cover: - Maximum upload size. - Maximum
archive size. - Maximum extracted file count. - Maximum expanded archive
size. - Maximum directory depth. - Request timeout. - Disk space
threshold.

Reject archives with traversal paths, absolute paths, device files,
unsupported special files, or symlinks that could escape the
destination. Validate free disk space before extraction when possible.

### 14.3 File operation consistency

Do not allow a restore or update to race with file mutation. Use
per-server operation locks or explicit operation compatibility rules.
For a world import, stop the server first unless a verified live-import
mechanism exists.

------------------------------------------------------------------------

## 15. Backup and Restore Design

### 15.1 Backup modes

-   **World backup:** only explicitly identified world data, if
    Pumpkin's layout is verified.
-   **Configuration backup:** configuration files and relevant metadata.
-   **Full server data backup:** the complete persistent data directory,
    excluding explicitly documented transient files only when safe.
-   **Manager backup:** database and application settings, separate from
    server-world backups.

When in doubt, prefer a full data backup over an incomplete guessed file
list.

### 15.2 Consistency strategy

Preferred order: 1. Stop the server and back up the data. 2. Use a
verified filesystem snapshot or application-consistent mechanism. 3.
Only offer live copying with a clear warning that it may be
inconsistent.

Record the consistency method on each backup. Do not call a live,
uncoordinated copy a consistent backup.

### 15.3 Backup process

1.  Validate authorization and target state.
2.  Check free space and backup destination.
3.  Acquire operation lock.
4.  Create a task and publish progress.
5.  Stop server if required by policy.
6.  Create an archive with safe relative paths.
7.  Compute checksum.
8.  Verify archive can be read/listed.
9.  Record metadata transactionally.
10. Resume server if it was running and policy permits.
11. Apply retention only after a successful backup.
12. Release lock and publish final result.

A failure must not delete older valid backups. If automatic restart
fails after a backup, report both the backup result and restart result.

### 15.4 Restore process

1.  Verify backup metadata and checksum.
2.  Confirm the backup belongs to the selected server or an explicitly
    approved import.
3.  Require server stopped.
4.  Create a pre-restore backup where possible.
5.  Validate archive entries and expansion limits.
6.  Extract into a staging directory.
7.  Verify expected files and free space.
8.  Replace data using a recoverable strategy.
9.  Preserve or restore permissions appropriately.
10. Record restore task and audit event.
11. Start only when the user requested it or policy clearly permits it.
12. Run readiness checks.
13. If restore fails, keep the previous data available for recovery.

### 15.5 Retention

Support count-based and age-based retention, with: - Minimum retained
backups. - Exclusion of in-progress backups. - No deletion until a new
backup has completed and verified. - Disk-space warning before retention
is triggered. - Clear audit entries for deleted backups. - A dry-run
preview for manual cleanup.

------------------------------------------------------------------------

## 16. Monitoring, Health, and Events

### 16.1 Metric sources

-   Docker container statistics for container CPU, memory and network.
-   Host-level system metrics for total/free memory and disk.
-   Docker inspect for lifecycle and restart count.
-   Minecraft protocol ping for server reachability and, where
    available, player data.
-   Pumpkin-native metrics only if an official, verified source exists.

Do not infer TPS from CPU usage. Do not equate the process being alive
with the game server accepting connections.

### 16.2 Sampling and retention

-   Configurable sampling interval.
-   Batch database writes.
-   Bounded retention and scheduled cleanup.
-   Aggregate older samples if longer history is desired.
-   Expose metric freshness and source.
-   Handle counter resets after container recreation.
-   Use monotonic clocks for durations where appropriate.

### 16.3 Health states

Suggested UI states: - `Starting` - `Online` - `Stopped` - `Unhealthy` -
`Failed` - `Unknown` - `Maintenance`

The API should also expose raw container state and a readiness reason. A
stopped server is not necessarily unhealthy; it may be intentionally
stopped.

### 16.4 Event stream

WebSocket events should be typed and versioned. Examples:

``` json
{
  "event": "server.state_changed",
  "version": 1,
  "timestamp": "2026-01-01T12:00:00Z",
  "server_id": "uuid",
  "payload": {
    "desired_state": "running",
    "observed_state": "starting"
  }
}
```

This is an illustrative contract, not a live event. Include: -
`server.state_changed` - `server.log` - `task.progress` -
`task.completed` - `backup.completed` - `metric.sample` -
`system.warning`

Use bounded buffers and reconnection support. The client should fetch a
fresh snapshot after reconnecting rather than assuming it received every
event.

------------------------------------------------------------------------

## 17. API Contract

All application routes are prefixed with `/api/v1`. Use JSON for
request/response bodies unless a file upload/download endpoint
explicitly requires streaming.

### 17.1 Authentication

-   `GET /api/v1/auth/setup-status`
-   `POST /api/v1/auth/setup`
-   `POST /api/v1/auth/login`
-   `POST /api/v1/auth/logout`
-   `GET /api/v1/auth/me`
-   `POST /api/v1/auth/change-password`

First-run setup must be protected against concurrent initialization and
should be disabled permanently after the initial owner exists, except
through a documented recovery mechanism.

### 17.2 Servers

-   `GET /api/v1/servers`
-   `POST /api/v1/servers`
-   `GET /api/v1/servers/{id}`
-   `PATCH /api/v1/servers/{id}`
-   `DELETE /api/v1/servers/{id}`
-   `POST /api/v1/servers/{id}/start`
-   `POST /api/v1/servers/{id}/stop`
-   `POST /api/v1/servers/{id}/restart`
-   `POST /api/v1/servers/{id}/update`
-   `GET /api/v1/servers/{id}/status`
-   `GET /api/v1/servers/{id}/logs`
-   `GET /api/v1/servers/{id}/capabilities`

Deletion must explicitly distinguish removing the container from
permanently deleting data. Prefer a soft-delete or archive period for
metadata when practical.

### 17.3 Configuration

-   `GET /api/v1/servers/{id}/config`
-   `GET /api/v1/servers/{id}/config/revisions`
-   `GET /api/v1/servers/{id}/config/revisions/{revision}`
-   `PUT /api/v1/servers/{id}/config`
-   `POST /api/v1/servers/{id}/config/validate`
-   `POST /api/v1/servers/{id}/config/revert`

Use revision identifiers or ETags to prevent lost updates.

### 17.4 Files

-   `GET /api/v1/servers/{id}/files?path=...`
-   `GET /api/v1/servers/{id}/files/download?path=...`
-   `POST /api/v1/servers/{id}/files/upload`
-   `POST /api/v1/servers/{id}/files/directory`
-   `POST /api/v1/servers/{id}/files/rename`
-   `DELETE /api/v1/servers/{id}/files?path=...`
-   `POST /api/v1/servers/{id}/files/import`

Path parameters must be validated by the file service; URL encoding is
not a security boundary.

### 17.5 Backups and tasks

-   `GET /api/v1/servers/{id}/backups`
-   `POST /api/v1/servers/{id}/backups`
-   `GET /api/v1/backups/{backupId}`
-   `GET /api/v1/backups/{backupId}/download`
-   `POST /api/v1/backups/{backupId}/restore`
-   `DELETE /api/v1/backups/{backupId}`
-   `GET /api/v1/tasks/{taskId}`
-   `POST /api/v1/tasks/{taskId}/cancel`

Cancellation must be cooperative. If an operation cannot safely be
cancelled, return a clear explanation rather than abandoning it
mid-write.

### 17.6 Monitoring and administration

-   `GET /api/v1/metrics/summary`
-   `GET /api/v1/servers/{id}/metrics`
-   `GET /api/v1/users`
-   `POST /api/v1/users`
-   `PATCH /api/v1/users/{id}`
-   `DELETE /api/v1/users/{id}`
-   `GET /api/v1/audit`
-   `GET /api/v1/settings`
-   `PATCH /api/v1/settings`
-   `GET /api/v1/events` or `GET /api/v1/ws/events` for WebSocket
    upgrade

### 17.7 API conventions

-   Paginate collections.
-   Validate and cap query limits.
-   Use stable error codes and request IDs.
-   Return `202 Accepted` with task ID for long-running operations.
-   Use `409 Conflict` for invalid lifecycle/concurrency state.
-   Use `422 Unprocessable Entity` for invalid configuration when
    appropriate.
-   Use `401` for unauthenticated and `403` for unauthorized.
-   Use `404` without leaking inaccessible resources.
-   Support optimistic concurrency for configuration.
-   Document the API in OpenAPI and test the schema in CI.

------------------------------------------------------------------------

## 18. Frontend Information Architecture and UX

### 18.1 Main navigation

-   Overview
-   Servers
-   Backups
-   Monitoring
-   Users (authorized roles only)
-   Settings
-   Audit/activity (authorized roles only)

Each server has a detail area: - Overview - Console/logs - Files -
Configuration - Backups - Metrics - Settings

### 18.2 Dashboard

Display: - Server counts by observed state. - Recent activity and task
failures. - Host CPU, memory, and disk. - Per-server cards or table. -
Backup health. - Alerts for low disk space, repeated crash loops,
outdated/unverified images and missing backups.

Do not overuse charts; prioritize actionable information.

### 18.3 Server detail

Header: - Name, status, runtime version. - Connection address and
port. - Start/stop/restart actions with appropriate disabled states. -
Last operation and readiness details.

Tabs: - Overview - Console - Files - Configuration - Backups - Metrics -
Settings

### 18.4 Loading and error states

Every page needs: - Initial loading state. - Empty state. -
Permission-denied state. - Network error and retry. - Stale-data
indication. - Operation-in-progress state. - Destructive-action
confirmation. - Success/failure feedback.

Avoid showing optimistic success for actions that have only been queued.
Distinguish "request accepted" from "server is online."

### 18.5 Accessibility and responsive behavior

-   Keyboard-operable controls.
-   Accessible labels and dialogs.
-   Status conveyed by text as well as color.
-   Focus management in modals.
-   Usable layout on mobile and tablet.
-   Tables that degrade into cards or horizontal scrolling.
-   Respect reduced-motion preferences.

------------------------------------------------------------------------

## 19. Background Jobs and Scheduling

### 19.1 Worker responsibilities

-   Lifecycle reconciliation.
-   Startup/readiness polling.
-   Metrics sampling.
-   Scheduled backups.
-   Backup retention.
-   Scheduled restart tasks if enabled.
-   Image pulls and updates.
-   Task recovery after restart.
-   Temporary-file cleanup.
-   Audit/metrics retention.

### 19.2 Scheduling guarantees

The first release is a single-node scheduler, not a distributed
scheduler. Ensure only one scheduler loop is active per manager process.
If multiple manager replicas are introduced later, add distributed
coordination before allowing them to run the same scheduled jobs.

Persist schedules and task runs. On restart: - Recompute missed jobs
according to an explicit policy. - Avoid running every missed backup
simultaneously. - Prevent duplicate execution. - Record skipped runs and
reasons. - Use bounded concurrency.

### 19.3 Task cancellation

Support cancellation only where safe: - Image pull: best-effort. -
Backup: stop at a safe boundary and clean temporary files. - Restore:
cancellation before commit; after commit begins, prefer completion or
explicit recovery. - Lifecycle operation: do not interrupt at an
arbitrary unsafe point.

------------------------------------------------------------------------

## 20. CasaOS and Docker Compose Deployment

### 20.1 Deployment requirements

-   One documented Compose project.
-   Persistent application data.
-   Persistent server data per instance.
-   Configurable web UI port.
-   Optional reverse proxy.
-   Health check for the manager.
-   Explicit restart policy.
-   No secrets committed to the repository.
-   Clean upgrade path.
-   Clear backup and restore instructions.

### 20.2 Compose principles

The final Compose file should be produced and tested against the
selected runtime and deployment strategy. At minimum: - Pin manager
image tags or build a versioned local image. - Pin Pumpkin image
references. - Use named volumes or documented host bind mounts. - Expose
only the web UI and explicitly allocated game ports. - Avoid exposing
the Docker API over TCP. - Run as a non-root user where the chosen
Docker integration permits it. - Drop unnecessary Linux capabilities. -
Apply `no-new-privileges` where compatible. - Use read-only root
filesystems where feasible, with explicit writable mounts for required
paths. - Add health checks and restart policies. - Set reasonable
resource limits for the manager itself. - Document required UID/GID and
filesystem permissions.

Docker socket access may constrain how completely the manager can be
sandboxed. Explain the residual privilege clearly rather than claiming
that a non-root container eliminates host-level risk.

### 20.3 Example environment file

Provide `.env.example` with non-secret defaults only, such as: -
`PUMPKIN_MANAGER_HTTP_PORT` - `PUMPKIN_MANAGER_DATA_DIR` -
`PUMPKIN_MANAGER_LOG_LEVEL` -
`PUMPKIN_MANAGER_METRICS_INTERVAL_SECONDS` -
`PUMPKIN_MANAGER_BACKUP_DIR` - `PUMPKIN_MANAGER_SESSION_TTL_MINUTES`

Secret values should be generated during setup or supplied through
protected secret files. Validate configuration at startup and fail with
a clear message for invalid values.

### 20.4 CasaOS installation guide

Document: 1. Supported OS/architecture and Docker prerequisites. 2.
Installing via Compose or CasaOS custom app. 3. Creating persistent
directories. 4. Setting directory ownership and permissions. 5.
Selecting the UI port. 6. Completing first-run owner setup. 7. Creating
the first Pumpkin server. 8. Opening only the required game port. 9.
Configuring router/firewall forwarding manually if desired. 10.
Scheduling backups. 11. Updating the manager. 12. Updating Pumpkin
independently. 13. Restoring the manager database and server worlds. 14.
Troubleshooting permission, port, image-pull, and startup failures.

------------------------------------------------------------------------

## 21. Observability and Diagnostics

### 21.1 Application logging

Use structured logs with: - timestamp, - level, - component, - request
ID, - task ID where relevant, - server ID where relevant, - operation, -
duration, - outcome.

Redact credentials, cookies, tokens, and sensitive configuration values.
Provide configurable log retention.

### 21.2 Health endpoints

Separate: - **Liveness:** the process is running. - **Readiness:**
required application dependencies are usable. - **Runtime health:**
Docker daemon is reachable and individual servers are healthy.

A Docker daemon outage should not necessarily make the process liveness
endpoint fail. Report degraded readiness clearly.

### 21.3 Diagnostics bundle

Allow an authorized owner to export a redacted diagnostics bundle
containing: - application version, - OS/architecture, - database schema
version, - runtime adapter version, - Pumpkin compatibility profile, -
sanitized logs, - task failures, - container status summary, -
configuration validation results.

Never include passwords, session tokens, secret environment values, or
world files in the diagnostics bundle.

------------------------------------------------------------------------

## 22. Testing Strategy

Testing is a release requirement, not a final polishing task.

### 22.1 Unit tests

Cover: - State transitions. - Permission matrix. - Port allocation. -
Resource-limit validation. - Configuration parse/validation. - Revision
conflict detection. - Retention policy. - Archive path validation. -
Error-code mapping. - Scheduler expression validation. - Capability
gating.

### 22.2 Integration tests

Use a disposable Docker environment where possible: - Container creation
and labels. - Start/stop/restart. - Logs. - Status reconciliation. -
Persistent volumes. - Port conflicts. - Resource limits. - Image-pull
failures. - Docker daemon unavailable. - Manager restart during an
operation. - SQLite migration and recovery.

### 22.3 Pumpkin compatibility tests

For every supported image/version: - Clean startup. - Readiness probe. -
Log capture. - Graceful stop. - Forced stop fallback. - World
persistence. - Configuration persistence. - Invalid config behavior. -
Recreate/update behavior. - Protocol ping. - Player count if claimed. -
Console command interface if claimed.

Do not release a compatibility profile with untested claims.

### 22.4 Security tests

-   Unauthenticated API access.
-   Viewer attempts to mutate state.
-   Cross-server authorization.
-   CSRF where cookie sessions are used.
-   Session invalidation.
-   Path traversal.
-   Symlink escape.
-   Malicious archive entries.
-   Oversized upload.
-   Request-body limits.
-   SQL injection attempts.
-   WebSocket authorization and reconnect.
-   Docker API arbitrary operation attempts.
-   Secret leakage in logs and diagnostics.

### 22.5 End-to-end tests

Use Playwright to verify: 1. First-run setup. 2. Login/logout. 3. Create
server. 4. Start and observe readiness. 5. View logs. 6. Edit and
validate configuration. 7. Create backup. 8. Stop and restore in a
disposable environment. 9. Permission enforcement. 10. Recover after API
refresh/restart.

### 22.6 Reliability tests

-   Disk fills during backup.
-   Host reboots while a server is running.
-   Manager restarts during startup.
-   Docker daemon becomes unavailable.
-   Image pull fails.
-   Container enters crash loop.
-   Configuration file is modified outside the manager.
-   Backup checksum fails.
-   Restore archive is corrupt.
-   SQLite database is restored from backup.

### 22.7 CI gates

Every pull request should run: - Rust formatting check. - Clippy with
warnings treated according to project policy. - Unit and integration
tests. - Frontend lint and type checking. - Frontend tests. - Production
build. - Dependency audit. - Secret scanning. - Container build. -
Migration checks.

Nightly or release CI can run heavier Docker/Pumpkin integration and E2E
suites.

------------------------------------------------------------------------

## 23. Performance and Resource Budgets

Set measurable targets after the first benchmark on the intended
home-server hardware.

Suggested initial objectives: - Dashboard initial API response under 500
ms on a healthy local system for ordinary requests. - Lifecycle requests
return task IDs promptly rather than waiting for startup. - Log
streaming uses bounded buffers and does not grow memory without limit. -
Metrics sampling has a configurable interval and bounded history. -
Backup concurrency defaults to one heavy backup at a time. - Large file
transfers stream rather than loading entire files into memory. - API
timeouts and task timeouts are explicit. - SQLite writes are batched
where safe.

These are engineering targets, not guarantees. Benchmark with realistic
log volume, multiple instances, and large world backups.

------------------------------------------------------------------------

## 24. Failure Handling and Recovery Matrix

  -----------------------------------------------------------------------
  Failure                             Expected behavior
  ----------------------------------- -----------------------------------
  Docker daemon unavailable           Show runtime degraded state; do not
                                      report servers as stopped without
                                      evidence

  Server crashes                      Record exit information, update
                                      observed state, preserve logs,
                                      apply configured restart policy

  Manager restarts                    Reconcile containers and
                                      resume/recover tasks safely

  Backup fails                        Preserve previous backups, clean
                                      safe temporary artifacts, report
                                      failure

  Restore fails                       Preserve the original data or
                                      recovery copy; do not silently
                                      delete it

  Disk nearly full                    Warn before backup/update; prevent
                                      operations likely to corrupt data

  Port conflict                       Reject creation with a clear
                                      actionable error

  Invalid TOML                        Reject save and preserve current
                                      configuration

  External config edit                Detect revision mismatch and ask
                                      for conflict resolution

  Unsupported Pumpkin feature         Disable it and explain the
                                      capability limitation

  Database migration fails            Do not serve requests against an
                                      unknown schema; preserve database
                                      and explain recovery

  Image update fails                  Keep old image reference and offer
                                      a safe rollback path

  Session expires                     Stop sensitive action and request
                                      re-authentication

  WebSocket disconnects               Reconnect with backoff and fetch a
                                      fresh snapshot
  -----------------------------------------------------------------------

------------------------------------------------------------------------

## 25. Implementation Roadmap and Deliverables

The roadmap is organized by exit criteria rather than calendar promises.
Estimates should be assigned after the first compatibility spike and
team-capacity assessment.

### Phase 0 --- Discovery and compatibility

**Deliverables** - Compatibility report. - Reproducible test scripts. -
Selected image/version and architecture support. - Verified ports,
mounts, logs, shutdown and status behavior. - Feature capability
matrix. - Initial threat model. - Architecture decision records (ADRs).

**Exit criteria** - A disposable Pumpkin server can be started, stopped,
inspected and recreated with persistent data. - Unknown runtime
behaviors are documented. - No unsupported console or metrics claims
remain in the product specification.

### Phase 1 --- Repository and deployment foundation

**Deliverables** - Rust workspace and frontend scaffold. - Formatting,
linting, tests and CI. - Configuration loading and structured logging. -
SQLite migrations. - Docker Compose development environment. - Manager
health/readiness endpoints. - Basic CasaOS deployment documentation.

**Exit criteria** - Clean checkout builds reproducibly. - Database
initializes and migrates. - Container health checks work. - Secrets are
not committed.

### Phase 2 --- Authentication and server inventory

**Deliverables** - First-run setup. - Login/logout/session lifecycle. -
RBAC policy service. - Server metadata CRUD. - Runtime adapter
abstraction. - Docker adapter with safe container labels. - Task table
and background task runner. - Audit event foundation.

**Exit criteria** - Unauthenticated and unauthorized operations are
rejected. - Server records and Docker resources reconcile after
restart. - Container creation accepts only validated specifications.

### Phase 3 --- Lifecycle and dashboard

**Deliverables** - Create/start/stop/restart. - State machine and
per-server locks. - Readiness checks. - Live logs. - Server list/detail
views. - CPU/memory summary. - Failure and task history UI.

**Exit criteria** - Full lifecycle works from the browser. - Readiness
is not inferred from container existence alone. - Failure states and
recovery behavior are testable.

### Phase 4 --- Configuration and files

**Deliverables** - Version-aware Pumpkin config profiles. - TOML raw
editor and guided forms for verified fields. - Validation, diff,
revision history and atomic save. - Sandboxed file browser and
upload/download. - Safe archive import/export.

**Exit criteria** - Configuration is preserved across manager restart. -
External modifications trigger conflict handling. - Path traversal and
archive traversal tests pass.

### Phase 5 --- Backups and restore

**Deliverables** - Manual backups. - Progress events. - Checksums and
verification. - Restore workflow and pre-restore recovery point. -
Retention policy. - Scheduled jobs. - Backup status UI.

**Exit criteria** - A real test world can be backed up and restored. -
Corrupt archives are rejected. - Failure does not delete the last
verified backup.

### Phase 6 --- Monitoring and automation

**Deliverables** - Metrics sampling and retention. - Host/container
health. - Protocol ping. - Scheduled backups and optional restart
tasks. - Low-disk and repeated-crash alerts. - Diagnostics export.

**Exit criteria** - Unsupported metrics are not shown as zero. -
Scheduler avoids duplicate execution after restart. - Metrics storage
remains bounded.

### Phase 7 --- Updates and operational polish

**Deliverables** - Image pull/update task. - Version compatibility
warnings. - Rollback workflow. - User administration. - Audit UI. -
Application database backup/restore. - Responsive and accessibility
review.

**Exit criteria** - Update failure preserves data and offers a
recoverable path. - Permission changes are audited. - Manager recovery
is documented and tested.

### Phase 8 --- Release candidate

**Deliverables** - Security review. - Full test report. - Performance
benchmark. - Upgrade guide. - Clean-install guide. - Recovery guide. -
Release artifacts and checksums. - Known limitations page. - Versioned
compatibility matrix.

**Exit criteria** - All critical tests pass. - No known
critical/high-severity security issues remain unresolved. - Fresh
install, upgrade, backup, restore and recovery have been demonstrated on
the target deployment. - Every advertised Pumpkin-specific feature has
evidence for the supported version.

------------------------------------------------------------------------

## 26. Definition of Done

A feature is done only when all applicable conditions are met:

-   Acceptance criteria are written and satisfied.
-   API input is validated and authorization is enforced.
-   Errors are stable, actionable and safe.
-   Long-running operations use persisted tasks.
-   Sensitive actions create audit events.
-   Unit and integration tests exist.
-   UI loading, empty, failure and permission states are implemented.
-   Accessibility and responsive behavior are considered.
-   Documentation is updated.
-   Logs do not expose secrets.
-   Recovery behavior is defined.
-   Compatibility claims are tested.
-   Migrations and upgrade implications are addressed.

A merged pull request is not, by itself, proof that a feature is done.

------------------------------------------------------------------------

## 27. Release and Versioning Policy

Use semantic versioning for Pumpkin Manager: - Major: breaking API,
configuration or migration changes. - Minor: backward-compatible
features. - Patch: fixes and compatible maintenance.

Each release should include: - Application version and commit. -
Database schema version. - Supported manager deployment architectures. -
Tested Pumpkin versions/image digests. - Migration notes. - Security
notes. - Known limitations. - Upgrade and rollback instructions.

Do not couple manager releases to a particular Pumpkin image unless
compatibility requires it. Allow owners to choose from verified versions
and explicitly acknowledge unverified versions.

------------------------------------------------------------------------

## 28. Documentation Deliverables

Maintain these documents as part of the repository:

-   `README.md`: overview, screenshots, quick start, support matrix.
-   `docs/architecture.md`: components, data flow and trust boundaries.
-   `docs/compatibility/pumpkin-runtime.md`: tested runtime details.
-   `docs/security.md`: threat model, Docker privileges, network
    exposure and reporting.
-   `docs/api.md`: API contracts and authentication.
-   `docs/installation-casaos.md`: installation, storage and ports.
-   `docs/backup-restore.md`: consistency model, retention and recovery.
-   `docs/operations.md`: troubleshooting, logs, health and diagnostics.
-   `docs/development.md`: local setup, tests and contribution workflow.
-   `docs/adr/`: decisions such as SQLite, Docker adapter, session model
    and backup consistency.

Screenshots and claims in the README must reflect the actual
application. Do not advertise unfinished capabilities as available.

------------------------------------------------------------------------

## 29. Risks and Mitigations

  -----------------------------------------------------------------------
  Risk                    Impact                  Mitigation
  ----------------------- ----------------------- -----------------------
  Pumpkin API/CLI changes Control features break  Versioned compatibility
                                                  profiles and
                                                  integration tests

  Docker socket privilege Host compromise         Narrow adapter,
                                                  restricted operations,
                                                  no public Docker API,
                                                  clear security docs

  Inconsistent world      Data loss               Stop-before-backup or
  backups                                         verified snapshot;
                                                  restore tests

  Configuration schema    Invalid settings        Version-aware schema
  drift                                           and raw-edit fallback

  External file edits     Lost changes            Revision hashes and
                                                  conflict detection

  Disk exhaustion         Failed backups or world Preflight checks,
                          corruption              retention, low-disk
                                                  alerts

  Concurrent operations   Corruption or state     Per-server locks and
                          confusion               task state machine

  Container state differs Misleading dashboard    Separate observed
  from readiness                                  runtime state and game
                                                  readiness

  Unverified command      False console promises  Capability-gated
  interface                                       command input

  Malicious archive       Host/data compromise    Safe extraction, path
  uploads                                         validation, limits

  Database corruption     Loss of manager         SQLite backup, restore
                          metadata                tests and integrity
                                                  checks

  Insecure remote         Account/host compromise HTTPS guidance, auth,
  exposure                                        CSRF defense, least
                                                  privilege
  -----------------------------------------------------------------------

------------------------------------------------------------------------

## 30. Open Decisions to Resolve During Phase 0

These should be decided from test evidence, not guesswork:

1.  Which Pumpkin release/image digest is the initial supported target?
2.  Which CPU architectures will be supported initially?
3.  What exact mount path and startup command does the selected image
    use?
4.  Does the selected version support RCON or another stable command
    interface?
5.  Which signals and timeouts provide graceful shutdown?
6.  Can Minecraft status ping provide the needed status and player
    information?
7.  Should the manager directly mount the Docker socket or use a
    restricted socket proxy?
8.  Should server data use named Docker volumes or host bind mounts by
    default? CasaOS usability and backup access both matter.
9.  Which TOML parser preserves unknown fields and formatting
    acceptably? If exact formatting cannot be preserved, document that
    raw save may reformat the file.
10. What is the minimum host storage and memory budget for a practical
    installation?
11. Will external remote access be supported in the first release, or
    will documentation recommend a VPN/reverse proxy?
12. What retention defaults are safe for a low-storage home server?

Record decisions in ADRs with context, options, decision, consequences
and revisit criteria.

------------------------------------------------------------------------

## 31. First Sprint: Concrete Engineering Tasks

Start with a vertical slice, not a large amount of disconnected
scaffolding.

1.  Create the repository and Rust workspace.
2.  Add a Vite React/TypeScript app.
3.  Add formatting, linting, tests and CI.
4.  Write the Pumpkin compatibility smoke-test script.
5.  Run it against the selected upstream image and record results.
6.  Implement a typed runtime adapter around the tested Docker API
    calls.
7.  Implement SQLite migrations for users, servers and tasks.
8.  Implement first-run owner setup and authentication.
9.  Implement one server create/start/stop path.
10. Display observed state and recent logs in the UI.
11. Test persistence through manager and Pumpkin container restarts.
12. Write the first integration test using a disposable data directory.

**Sprint exit demonstration:** From a clean deployment, an owner logs
in, creates one Pumpkin instance, starts it, sees its real logs and
readiness, stops it, restarts the manager, and confirms the instance
metadata and persistent world data remain intact.

Do not begin with plugin management, elaborate analytics, or remote
multi-node support. The vertical slice validates the highest-risk
foundation first.

------------------------------------------------------------------------

## 32. Final Engineering Recommendation

Build Pumpkin Manager as a **single-node control plane with a Rust/Axum
backend, React/TypeScript frontend, SQLite persistence, and one isolated
Docker container per Pumpkin server**. Keep the Pumpkin-specific
integration behind a tested capability adapter. Make lifecycle
operations task-based and recoverable. Treat the Docker Engine as a
privileged boundary. Build configuration, file operations, and backups
around explicit data-safety invariants.

The order matters:

1.  Prove runtime compatibility.
2.  Prove safe container lifecycle and persistence.
3.  Build authentication and the vertical slice.
4.  Add configuration and file management.
5.  Add backups and restore.
6.  Add metrics, scheduling, updates and polish.
7.  Release only after security and recovery tests pass.

The result should be a maintainable control panel rather than a fragile
wrapper around a few Docker commands. The strongest measure of success
is not how many dashboard widgets exist; it is whether an owner can
manage real Pumpkin worlds safely, understand failures quickly, and
recover confidently when something goes wrong.
