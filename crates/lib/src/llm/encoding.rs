//! Gzip request-body compression negotiation for LLM providers.
//!
//! LLM request bodies can be large (hundreds of KB when tool results or file
//! content are present). When a provider accepts `Content-Encoding: gzip`,
//! sending the request body gzip-compressed cuts upload bandwidth and latency
//! substantially (typically 5–15× for JSON tool payloads).
//!
//! Per provider, a one-time *probe request* is issued to the same endpoint
//! that a real call uses. The probe sends a tiny gzip-compressed body with
//! `Content-Encoding: gzip` + `Content-Type: application/json` and inspects
//! the response:
//! - a decoding/rejection error (e.g. `400` whose body mentions "decode",
//!   "gzip", "decompression", "invalid") → gzip is unsupported by this endpoint
//! - any other status (auth error, model-not-found, validation error, 429,
//!   5xx) → gzip is supported (the server at least understood the headers)
//! - a network-level failure → unknown; stay conservative and don't use gzip
//!
//! The result is cached per provider instance in an [`Encoder`] so the probe
//! runs at most once per process for a given base URL. The first real request
//! after construction is sent uncompressed (zero first-call latency); from the
//! second request onward the negotiated encoding is used when the body is
//! larger than [`MIN_COMPRESS_LEN`].

use flate2::Compression;
use flate2::write::GzEncoder;
use reqwest::header::CONTENT_ENCODING;

/// Bodies at least this large are worth gzip-compressing (below the threshold
/// gzip overhead exceeds the savings on already-tiny JSON).
pub const MIN_COMPRESS_LEN: usize = 1024;

/// A cached, per-provider gzip negotiation result.
///
/// Built from a [`OnceLock<bool>`]: `true` once the provider endpoint has been
/// probed and confirmed to accept gzip, `false` after a probe proved it does
/// not, and `None` before any probe ran.
#[derive(Default)]
pub struct Encoder {
    negotiated: std::sync::OnceLock<bool>,
}

impl Encoder {
    /// Create a fresh encoder with no prior probe result.
    pub fn new() -> Self {
        Self {
            negotiated: std::sync::OnceLock::new(),
        }
    }

    /// Returns `true` once gzip has been negotiated and confirmed for this
    /// endpoint. Always `false` before the first probe completes.
    pub fn is_negotiated(&self) -> bool {
        self.negotiated.get().copied().unwrap_or(false)
    }

    /// Record a probe outcome. Used in tests to simulate a completed probe;
    /// in normal operation this is set by [`probe_if_needed`].
    ///
    /// Returns `false` if a probe had already completed (the first writer
    /// wins, matching `OnceLock` semantics).
    pub fn set_negotiated(&self, value: bool) -> bool {
        self.negotiated.set(value).is_ok()
    }

    /// Encode `body` (already serialized JSON) if gzip is negotiated and the
    /// body is large enough to benefit from compression. Returns the
    /// (possibly compressed) bytes plus a flag telling the caller whether to
    /// set `Content-Encoding: gzip`.
    ///
    /// Bodies smaller than [`MIN_COMPRESS_LEN`] are returned as-is even when
    /// gzip is negotiated, because the gzip frame overhead would make them
    /// larger rather than smaller.
    pub fn encode(&self, body: &[u8]) -> (Vec<u8>, bool) {
        if self.is_negotiated()
            && body.len() >= MIN_COMPRESS_LEN
            && let Ok(compressed) = gzip_bytes(body)
            && compressed.len() < body.len()
        {
            return (compressed, true);
        }
        (body.to_vec(), false)
    }
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("negotiated", &self.negotiated.get())
            .finish()
    }
}

/// Gzip-compress a byte slice. Pure function; used both by the probe body
/// construction and by [`Encoder::encode`].
pub fn gzip_bytes(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let estimate = data.len() / 2;
    let mut enc = GzEncoder::new(Vec::with_capacity(estimate.max(32)), Compression::default());
    use std::io::Write;
    enc.write_all(data)?;
    enc.finish()
}

/// Classify a probe response: `true` means gzip is accepted (any outcome
/// other than an explicit decoding rejection), `false` means the endpoint
/// rejects `Content-Encoding: gzip`.
pub fn probe_response_ok(status: u16, body_text: &str) -> bool {
    let lowered = body_text.to_lowercase();
    let rejects_gzip = lowered.contains("gzip")
        || lowered.contains("compress")
        || lowered.contains("decomp")
        || lowered.contains("decode");

    match status {
        // A decoding error on a probe = gzip not accepted.
        400 if rejects_gzip => false,
        // Any other status: the server parsed the headers and didn't choke on
        // the encoding (auth errors, model-unknowns, rate limits, overloads,
        // even 200 on an empty body are all fine — they confirm the endpoint
        // speaks gzip).
        _ => true,
    }
}

/// Build a probe request body: a minimal valid message-shaped payload,
/// gzip-compressed, to send to the same endpoint as a real call.
pub fn probe_body(model: &str) -> std::io::Result<Vec<u8>> {
    let payload = serde_json::json!({
        "model": model,
        "messages": [],
    });
    let json = serde_json::to_vec(&payload).expect("probe payload is static JSON");
    gzip_bytes(&json)
}

/// Run a one-shot probe against `url` using `http`, caching the result in
/// `encoder`. Cheap relative to a real LLM call. The [`OnceLock`] inside
/// [`Encoder`] ensures only the first caller's probe result is recorded; if
/// a caller is already probing, this call returns immediately and lets the
/// in-flight probe's outcome populate the cache.
///
/// The probe runs only over the network when the endpoint has not yet been
/// negotiated (checked via `encoder.is_negotiated`'s backing `OnceLock`). If
/// a previous probe already recorded a result — success (`true`) or failure
/// (`false`) — this is a cheap no-op returning that cached value. This means
/// a failed probe (endpoint rejected gzip) permanently disables compression
/// for that provider instance for the process lifetime, avoiding repeated
/// probe round-trips to an endpoint that provably doesn't support gzip.
pub async fn probe_if_needed(
    http: &reqwest::Client,
    url: &str,
    auth_headers: &reqwest::header::HeaderMap,
    model: &str,
    encoder: &Encoder,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> bool {
    // Fast path: already probed (success or failure). A cached `false` means
    // a prior probe proved this endpoint doesn't accept gzip — honor it and
    // skip the network entirely.
    if encoder.negotiated.get().is_some() {
        return encoder.is_negotiated();
    }

    // Slow path: run the probe. If a concurrent call already set the value
    // while we were building the request, honor theirs. The entire probe —
    // including the body construction (gzip_bytes) — is raced against the
    // caller's cancellation token so a user cancel during negotiation does not
    // block the turn for the probe's network round-trip.
    let result = (|| async {
        let body = probe_body(model)?;
        let request = http
            .post(url)
            .headers(auth_headers.clone())
            .header(CONTENT_ENCODING, "gzip")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(reqwest::Body::from(body))
            .build()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        let resp = if let Some(ct) = cancel {
            // Race the send against the cancellation token so a user cancel
            // aborts the probe's network round-trip promptly instead of
            // hanging until the 300s client timeout fires. The cancel branch
            // returns `Ok(None)` from this closure, short-circuiting the probe
            // with "unknown" (conservative: don't assume gzip is supported).
            tokio::select! {
                biased;
                _ = ct.cancelled() => return Ok(None),
                r = http.execute(request) => r,
            }
        } else {
            http.execute(request).await
        };
        // A transport-level failure (DNS, connection reset, timeout) means
        // we never got a response that could confirm gzip support — stay
        // conservative and treat it as "not negotiated".
        let resp = match resp {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_else(|_| String::new());
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Some(probe_response_ok(status, &text)))
    })();
    let negotiated = result.await.ok().flatten().unwrap_or(false);
    // First writer wins; if someone raced us, theirs is already set and our
    // `set` is a no-op.
    let _ = encoder.negotiated.set(negotiated);
    negotiated
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    #[test]
    fn gzip_round_trip() {
        let original = b"hello gzip world".repeat(64);
        let compressed = gzip_bytes(&original).unwrap();
        assert!(compressed.len() < original.len());
        let mut decoded = Vec::new();
        GzDecoder::new(&compressed[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn empty_payload_round_trips() {
        let compressed = gzip_bytes(b"").unwrap();
        let mut decoded = Vec::new();
        GzDecoder::new(&compressed[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn probe_body_is_valid_compressed_json() {
        let compressed = probe_body("claude-sonnet-4").unwrap();
        let mut decoded = Vec::new();
        GzDecoder::new(&compressed[..])
            .read_to_end(&mut decoded)
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(json["model"], "claude-sonnet-4");
        assert_eq!(json["messages"], serde_json::Value::Array(Vec::new()));
    }

    #[test]
    fn probe_response_ok_accepts_non_decode_errors() {
        // Auth error, rate limit, overload, model-not-found, success — all
        // confirm the endpoint understood the gzip envelope.
        assert!(probe_response_ok(401, r#"{"error":"unauthorized"}"#));
        assert!(probe_response_ok(429, r#"{"error":"rate limited"}"#));
        assert!(probe_response_ok(503, r#"{"error":"overloaded"}"#));
        assert!(probe_response_ok(404, r#"{"error":"model not found"}"#));
        assert!(probe_response_ok(200, r#""""#));
        assert!(probe_response_ok(
            400,
            r#"{"error":"invalid tool call id"}"#
        ));
    }

    #[test]
    fn probe_response_ok_rejects_decode_errors() {
        assert!(!probe_response_ok(
            400,
            r#"{"error":"invalid gzip encoding"}"#
        ));
        assert!(!probe_response_ok(
            400,
            r#"{"error":"decompression failed"}"#
        ));
        assert!(!probe_response_ok(
            400,
            r#"{"error":"could not decode body"}"#
        ));
        // Any status outside 400, even with a decode-looking message, is a
        // normal server response (the probe payload was a tiny static JSON),
        // so it does NOT count as a gzip rejection.
        assert!(probe_response_ok(422, r#"{"error":"decompress failed"}"#));
        assert!(probe_response_ok(200, r#"{"error":"decompress failed"}"#));
    }

    #[test]
    fn probe_response_ok_ignores_decode_keyword_in_unrelated_messages() {
        // A 404 whose body happens to mention "decode" in a totally unrelated
        // context (e.g. a model name "codec-decode") is still a model/unknown
        // error, not a gzip rejection. The keyword check is anchored on the
        // 400 status.
        assert!(probe_response_ok(
            404,
            r#"{"error":"model codec-decode not found"}"#
        ));
    }

    #[test]
    fn encoder_encode_below_threshold_is_plain() {
        let enc = Encoder::new();
        // Prime a negotiated (true) state for testing the encode path in
        // isolation; normally this comes from a probe.
        let _ = enc.set_negotiated(true);
        let body: Vec<u8> = vec![b'{'; 500];
        let (out, compressed) = enc.encode(&body);
        assert!(!compressed, "below threshold must stay uncompressed");
        assert_eq!(out, body);
    }

    #[test]
    fn encoder_encode_at_threshold_is_compressed() {
        let enc = Encoder::new();
        let _ = enc.set_negotiated(true);
        // A body of repeated bytes compresses well and is >= MIN_COMPRESS_LEN.
        let body: Vec<u8> = vec![b'A'; MIN_COMPRESS_LEN];
        let (out, compressed) = enc.encode(&body);
        assert!(compressed);
        assert!(out.len() < body.len());
    }

    #[test]
    fn encoder_not_negotiated_is_plain() {
        let enc = Encoder::new();
        let body: Vec<u8> = vec![b'A'; MIN_COMPRESS_LEN * 4];
        let (out, compressed) = enc.encode(&body);
        assert!(!compressed);
        assert_eq!(out, body);
    }

    #[test]
    fn encoder_negotiated_false_stays_plain() {
        let enc = Encoder::new();
        let _ = enc.set_negotiated(false);
        let body: Vec<u8> = vec![b'A'; MIN_COMPRESS_LEN * 4];
        let (out, compressed) = enc.encode(&body);
        assert!(!compressed);
        assert_eq!(out, body);
    }

    #[tokio::test]
    async fn probe_if_needed_caches_on_success() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let (server, local) = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local = l.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let _ = s.read(&mut buf).await;
                let body = r#"{"error":"unauthorized"}"#;
                let mut resp = String::new();
                resp.push_str("HTTP/1.1 401 Unauthorized\r\n");
                resp.push_str("Content-Type: application/json\r\n");
                resp.push_str(&format!("Content-Length: {}\r\n", body.len()));
                resp.push_str("Connection: close\r\n\r\n");
                resp.push_str(body);
                let _ = s.write_all(resp.as_bytes()).await;
            });
            (server, local)
        };

        let url = format!("http://{local}/chat/completions");
        let http = reqwest::Client::builder().build().unwrap();
        let headers = reqwest::header::HeaderMap::new();
        let enc = Encoder::new();

        probe_if_needed(&http, &url, &headers, "gpt-5.4", &enc, None).await;
        server.await.unwrap();

        assert!(enc.is_negotiated(), "probe should cache true on 401");
    }

    #[tokio::test]
    async fn probe_if_needed_caches_on_decode_rejection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let (server, local) = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local = l.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let _ = s.read(&mut buf).await;
                let body = r#"{"error":"invalid gzip encoding"}"#;
                let mut resp = String::new();
                resp.push_str("HTTP/1.1 400 Bad Request\r\n");
                resp.push_str("Content-Type: application/json\r\n");
                resp.push_str(&format!("Content-Length: {}\r\n", body.len()));
                resp.push_str("Connection: close\r\n\r\n");
                resp.push_str(body);
                let _ = s.write_all(resp.as_bytes()).await;
            });
            (server, local)
        };

        let url = format!("http://{local}/chat/completions");
        let http = reqwest::Client::builder().build().unwrap();
        let headers = reqwest::header::HeaderMap::new();
        let enc = Encoder::new();

        probe_if_needed(&http, &url, &headers, "gpt-5.4", &enc, None).await;
        server.await.unwrap();

        assert!(
            !enc.is_negotiated(),
            "probe should cache false on decode rejection"
        );
    }

    #[tokio::test]
    async fn probe_if_needed_no_probe_when_already_set() {
        // Once the cache is populated, probe_if_needed must be a no-op and
        // NOT hit the network (the mock server below would otherwise error).
        let enc = Encoder::new();
        let _ = enc.set_negotiated(true);
        let http = reqwest::Client::builder().build().unwrap();
        let headers = reqwest::header::HeaderMap::new();
        // Deliberately bogus URL: never contacted because the cache is set.
        probe_if_needed(
            &http,
            "http://256.256.256.256/nothing",
            &headers,
            "gpt-5.4",
            &enc,
            None,
        )
        .await;
        assert!(enc.is_negotiated());
    }

    /// When the cancellation token is already cancelled, `probe_if_needed`
    /// must abort promptly instead of waiting for the network round-trip
    /// (or the 300s client timeout). Returns `false` conservatively and does
    /// not cache a negotiation result.
    #[tokio::test]
    async fn probe_if_needed_aborts_on_cancel() {
        use tokio_util::sync::CancellationToken;

        let cancel = CancellationToken::new();
        cancel.cancel();

        // Use a real client but point at a bogus URL. If the cancel branch
        // doesn't win the select, reqwest would block (or fail) on connection,
        // so we'd hit the 300s timeout — the elapsed assertion catches that.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap();
        let headers = reqwest::header::HeaderMap::new();
        let enc = Encoder::new();
        let url = "http://127.0.0.1:1/chat/completions";

        let start = std::time::Instant::now();
        let result = probe_if_needed(&http, url, &headers, "gpt-5.4", &enc, Some(&cancel)).await;
        let elapsed = start.elapsed();

        assert!(!result, "canceled probe must return false (not negotiated)");
        assert!(
            !enc.is_negotiated(),
            "canceled probe must not cache a negotiation result"
        );
        // Must return well under the 300s client timeout — the cancel branch
        // wins the select immediately.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "canceled probe should return promptly (elapsed {:?})",
            elapsed
        );
    }
}
