//! Self-identification headers sent with LLM provider requests.
//!
//! App-builder agents (pi.dev, Lovable, v0, bolt.new) and OpenRouter-style
//! gateways advertise the calling application via well-known headers so the
//! provider can attribute traffic and (on OpenRouter) surface the app on
//! leaderboards. We advertise `agent-code` the same way: `X-Title` carries the
//! app name and `HTTP-Referer` carries a stable homepage, plus a conventional
//! `User-Agent`. These are static for the agent itself and match the
//! `agent-code/0.2` `User-Agent` already used by the web tools.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};

/// App name advertised to LLM providers (`X-Title`).
pub const APP_NAME: &str = "agent-code";

/// Homepage advertised to LLM providers (`HTTP-Referer`).
pub const APP_REFERER: &str = "https://github.com/avala-ai/agent-code";

/// Build the standard self-identification header set.
///
/// Returns a `HeaderMap` with `X-Title`, `HTTP-Referer`, and `User-Agent`.
/// Safe to merge into any request's existing header map.
pub fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-title"),
        HeaderValue::from_static(APP_NAME),
    );
    headers.insert(
        HeaderName::from_static("http-referer"),
        HeaderValue::from_static(APP_REFERER),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&format!("agent-code/{}", env!("CARGO_PKG_VERSION")))
            .unwrap_or_else(|_| HeaderValue::from_static("agent-code")),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_include_identity() {
        let h = headers();
        assert_eq!(h.get("x-title").map(|v| v.to_str().unwrap()), Some(APP_NAME));
        assert_eq!(
            h.get("http-referer").map(|v| v.to_str().unwrap()),
            Some(APP_REFERER)
        );
        assert!(h.contains_key(USER_AGENT));
    }
}
