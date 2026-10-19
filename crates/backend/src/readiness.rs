//! Minecraft Java-edition status ping (readiness probe).
//!
//! Speaks the `Status` phase of the Minecraft protocol over plain TCP:
//! handshake (`next_state = 1`) + status request → JSON status response.
//! This is what makes `Running` mean "game server reachable" instead of
//! "container exists". No `minecraft-protocol` crate: the two packets we
//! need are hand-rolled varints, keeping the dependency surface minimal.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Parsed game-server status from a successful ping.
#[derive(Debug, Clone, PartialEq)]
pub struct PingResult {
    pub version_name: String,
    pub version_protocol: i32,
    pub motd_text: String,
    pub players_online: i32,
    pub players_max: i32,
    /// Full request→response round trip.
    pub latency_ms: u64,
}

/// Why a ping produced no usable status. Never a game-server fact, only a
/// probe outcome — callers must not present these as server state.
#[derive(Debug, Clone, PartialEq)]
pub enum PingError {
    /// TCP connect/read/write failed.
    Unreachable(String),
    /// Exceeded the probe deadline.
    Timeout,
    /// Bytes arrived but were not a status response.
    Malformed(String),
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// Host to probe for a server bound to `bind_address`. Wildcard bindings
/// answer on loopback; a specific address must be probed directly.
#[must_use]
pub fn probe_host(bind_address: &str) -> &str {
    match bind_address.trim() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        other => other,
    }
}

/// Ping a Java-edition server. `host` is a plain IP/hostname (no scheme),
/// `port` the Java game port (usually 25565).
pub async fn ping_java(host: &str, port: u16, timeout: Duration) -> Result<PingResult, PingError> {
    use tokio::io::AsyncReadExt;
    let started = std::time::Instant::now();
    let work = async {
        let mut stream = tokio::net::TcpStream::connect((host, port))
            .await
            .map_err(|e| PingError::Unreachable(e.to_string()))?;

        // Handshake: packet id 0x00, protocol version, host, port, next=1.
        let mut handshake = Vec::new();
        write_varint(&mut handshake, 0x00);
        write_varint(&mut handshake, 767);
        write_string(&mut handshake, host);
        handshake.extend_from_slice(&port.to_be_bytes());
        write_varint(&mut handshake, 1);
        send_packet(&mut stream, &handshake).await?;

        // Status request: packet id 0x00, empty body.
        send_packet(&mut stream, &[0x00]).await?;

        // Status response: length + id 0x00 + varint JSON length + JSON.
        let len = read_varint_async(&mut stream)
            .await
            .map_err(PingError::Malformed)?;
        if len <= 0 || len > 2_000_000 {
            return Err(PingError::Malformed(format!("bad packet length {len}")));
        }
        let mut buf = vec![0u8; len as usize];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| PingError::Malformed(format!("packet truncated: {e}")))?;
        parse_status_response(&buf)
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(Ok(mut result)) => {
            result.latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            Ok(result)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(PingError::Timeout),
    }
}

fn parse_status_response(buf: &[u8]) -> Result<PingResult, PingError> {
    let malformed = |msg: String| PingError::Malformed(msg);
    let (packet_id, mut rest) =
        read_varint_slice(buf).ok_or_else(|| malformed("no packet id".into()))?;
    if packet_id != 0 {
        return Err(malformed(format!("unexpected packet id {packet_id}")));
    }
    let (json_len, body) =
        read_varint_slice(rest).ok_or_else(|| malformed("no string length".into()))?;
    rest = body;
    if json_len < 0 || rest.len() < json_len as usize {
        return Err(malformed("truncated status JSON".into()));
    }
    let value: serde_json::Value = serde_json::from_slice(&rest[..json_len as usize])
        .map_err(|e| malformed(format!("invalid status JSON: {e}")))?;
    Ok(PingResult {
        version_name: value
            .pointer("/version/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        version_protocol: value
            .pointer("/version/protocol")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1) as i32,
        motd_text: description_text(value.pointer("/description")),
        players_online: value
            .pointer("/players/online")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        players_max: value
            .pointer("/players/max")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        latency_ms: 0, // filled in by `ping_java`
    })
}

/// Descriptions arrive as a string, `{"text": ...}`, or chat-component
/// arrays. Extract printable text without guessing formatting.
fn description_text(node: Option<&serde_json::Value>) -> String {
    match node {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(map)) => {
            let mut out = map
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(extra) = map.get("extra").and_then(|v| v.as_array()) {
                for part in extra {
                    out.push_str(&description_text(Some(part)));
                }
            }
            out
        }
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .map(|p| description_text(Some(p)))
            .collect::<String>(),
        _ => String::new(),
    }
}

async fn send_packet(stream: &mut tokio::net::TcpStream, payload: &[u8]) -> Result<(), PingError> {
    use tokio::io::AsyncWriteExt;
    let mut packet = Vec::with_capacity(payload.len() + 5);
    write_varint(&mut packet, payload.len() as i32);
    packet.extend_from_slice(payload);
    stream
        .write_all(&packet)
        .await
        .map_err(|e| PingError::Unreachable(e.to_string()))
}

async fn read_varint_async(stream: &mut tokio::net::TcpStream) -> Result<i32, String> {
    use tokio::io::AsyncReadExt;
    let mut result: u32 = 0;
    for i in 0..5 {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|e| format!("packet truncated: {e}"))?;
        result |= ((byte[0] & 0x7F) as u32) << (7 * i);
        if byte[0] & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err("varint exceeds 5 bytes".to_string())
}

/// Read a varint from a byte slice; returns value + remainder.
fn read_varint_slice(buf: &[u8]) -> Option<(i32, &[u8])> {
    let mut result: u32 = 0;
    for (i, &byte) in buf.iter().take(5).enumerate() {
        result |= ((byte & 0x7F) as u32) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((result as i32, &buf[i + 1..]));
        }
    }
    None
}

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut remaining = value as u32;
    loop {
        let mut byte = (remaining & 0x7F) as u8;
        remaining >>= 7;
        if remaining != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if remaining == 0 {
            break;
        }
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

// ---------------------------------------------------------------------------
// Tests (fake status server; no Docker, no internet)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const STATUS_JSON: &str = r#"{"version":{"name":"26.3","protocol":777},"players":{"max":1000,"online":3,"sample":[]},"description":{"text":"A blazingly fast Pumpkin server!"}}"#;

    /// Minimal status-phase peer: reads handshake + request, answers canned JSON.
    async fn fake_status_server() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain whatever the client sent (handshake + request may be
            // coalesced in one segment); then answer canned JSON.
            let mut buf = vec![0u8; 1024];
            let _ = sock.read(&mut buf).await;
            // Status response: packet length + id 0x00 + varint len + JSON.
            let json = STATUS_JSON.as_bytes();
            let mut payload = vec![0x00u8];
            write_varint(&mut payload, json.len() as i32);
            payload.extend_from_slice(json);
            let mut packet = Vec::new();
            write_varint(&mut packet, payload.len() as i32);
            packet.extend_from_slice(&payload);
            let _ = sock.write_all(&packet).await;
            // Keep the socket open briefly so the client can finish reading.
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        addr
    }

    fn write_varint(buf: &mut Vec<u8>, value: i32) {
        let mut u = value as u32;
        loop {
            let mut b = (u & 0x7F) as u8;
            u >>= 7;
            if u != 0 {
                b |= 0x80;
            }
            buf.push(b);
            if u == 0 {
                break;
            }
        }
    }

    #[tokio::test]
    async fn ping_parses_fake_server() {
        let addr = fake_status_server().await;
        let result = ping_java("127.0.0.1", addr.port(), Duration::from_secs(5))
            .await
            .expect("ping should succeed");
        assert_eq!(result.version_name, "26.3");
        assert_eq!(result.version_protocol, 777);
        assert_eq!(result.players_online, 3);
        assert_eq!(result.players_max, 1000);
        assert!(result.motd_text.contains("Pumpkin"));
    }

    #[tokio::test]
    async fn ping_refused_is_unreachable_not_timeout() {
        // Port 1 is (practically) never bound: connect must fail fast.
        let result = ping_java("127.0.0.1", 1, Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(PingError::Unreachable(_))),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn ping_garbage_is_malformed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = sock.write_all(b"this is not minecraft").await;
        });
        let result = ping_java("127.0.0.1", addr.port(), Duration::from_secs(5)).await;
        assert!(
            matches!(result, Err(PingError::Malformed(_))),
            "got {result:?}"
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    /// Live check vs real Pumpkin (needs `pumpkin-live` on :25577).
    /// Run: `cargo test -p pumpkin-manager-backend -- --ignored live_`.
    #[tokio::test]
    #[ignore]
    async fn live_ping_real_pumpkin() {
        let r = ping_java("127.0.0.1", 25577, Duration::from_secs(5))
            .await
            .expect("live ping");
        assert!(r.players_max > 0);
        assert!(!r.version_name.is_empty());
    }
    #[test]
    fn probe_host_uses_loopback_only_for_wildcards() {
        assert_eq!(probe_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(probe_host("::"), "127.0.0.1");
        assert_eq!(probe_host(""), "127.0.0.1");
        assert_eq!(probe_host("10.0.0.5"), "10.0.0.5");
    }
}
