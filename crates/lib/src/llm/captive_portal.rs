//! Captive portal login for LLM network error recovery.
//!
//! When an LLM API call fails with a transport-level network error, the
//! agent may be behind a captive portal (e.g. a Wi-Fi login page). This
//! module fetches the portal login page, parses the HTML form, submits the
//! "agree"/"accept"/"continue" field, and the caller then retries the LLM call.
//!
//! Portals are defined as builtin profiles. Each profile carries its own
//! cookie store so cookies from one portal never leak to another.
//!
//! No TOML configuration is required — profiles are compiled in and
//! selected by name at runtime.
//!
//! Form discovery:
//!
//! 1. Parse the first `<form>` element from static HTML via `scraper`.
//! 2. If none is found, fall back to a `button[onclick]` handler (static).
//! 3. If still none, scan inline `<script>` blocks and external `<script src>`
//!    URLs for form data embedded in JavaScript (form HTML assigned to
//!    `innerHTML`/`document.write`, JSON field configs, etc.) — portals that
//!    render the login form purely via JS have no `<form>` in the raw HTML.
//!
//! JavaScript execution itself is intentionally *not* performed; we only
//! extract declarative form data that the page author embedded in script
//! text. This avoids pulling in a headless browser while covering the common
//! JS-injected captive-portal login shape.

use crate::llm::captive_portal_js::execute_portal_js;
use regex::Regex;
use reqwest::header::LOCATION;
use scraper::{Html, Selector};
use std::collections::HashMap;
use std::sync::LazyLock;

/// A builtin captive portal profile.
///
/// The `login_url` is set per-instance at runtime; the profile only carries
/// the static form field name/value and a display name.
#[derive(Debug, Clone)]
pub struct CaptivePortalProfile {
    /// Human-readable name (e.g. "agree", "accept", "continue").
    pub name: &'static str,
    /// Name of the HTML form field that indicates agreement.
    pub agree_field: &'static str,
    /// Value submitted in the agree field.
    pub agree_value: &'static str,
    /// Extra static fields to merge into the submit body (e.g. passcode).
    /// Used when the portal's login endpoint expects fields beyond the agree
    /// flag (e.g. GovWifi's `passcode`).
    pub extra_fields: &'static [(&'static str, &'static str)],
}

/// Builtin captive portal profiles.
///
/// Each profile represents a common captive portal login form shape.
/// Profiles are tried in order until one succeeds.
pub const CAPTIVE_PORTAL_PROFILES: &[CaptivePortalProfile] = &[
    CaptivePortalProfile {
        name: "agree",
        agree_field: "agree",
        agree_value: "yes",
        extra_fields: &[],
    },
    CaptivePortalProfile {
        name: "accept",
        agree_field: "accept",
        agree_value: "true",
        extra_fields: &[],
    },
    CaptivePortalProfile {
        name: "continue",
        agree_field: "continue",
        agree_value: "Continue",
        extra_fields: &[],
    },
    CaptivePortalProfile {
        name: "terms",
        agree_field: "terms",
        agree_value: "agreed",
        extra_fields: &[],
    },
    // Hong Kong GovWifi (free.wifi.gov.hk). No `<form>` — the login button
    // calls `Service.userLogin` which POSTs to `apiPath + "/wifi/login"` with
    // a `passcode` field. The API base path (`Common.apiPath`) and the
    // `/wifi/login` endpoint are extracted from the portal's JS files.
    CaptivePortalProfile {
        name: "govwifi",
        agree_field: "passcode",
        agree_value: "",
        extra_fields: &[],
    },
];

/// Probe a captive portal URL to check if it is reachable (HTTP 200).
///
/// Used to distinguish "captive portal waiting for agree" (the portal's own
/// login page is reachable and returns 200) from "no network at all"
/// (connection error / timeout reaching anything).
pub async fn probe_portal_connectivity(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}
///
/// Add a portal URL here to test the page fetch + form submit logic via
/// `/connect`. Each URL is tried against every `CAPTIVE_PORTAL_PROFILES`
/// profile until one succeeds.
///
/// This is a real captive portal (Hong Kong GovWifi). Manual testing via
/// `/connect` requires network access to the portal. Success means the
/// agree form redirect returns to the origin URL; failure means the portal
/// did not redirect back (expected in non-captive environments).
pub const TEST_PORTALS: &[&str] = &[
    "https://free.wifi.gov.hk/GovWifiUI_/userlanding/tc/login.html?zoneName=KPRH160&ssid=Wi-Fi.HK&url=http%3A%2F%2Fexample.com%2F&",
];

/// Captive portal client for one portal. Holds a persistent reqwest cookie
/// jar so cookies survive across redirects and the final LLM retry.
pub struct CaptivePortal {
    /// reqwest client with cookie store enabled — persists session cookies
    /// across the GET login page and the agree POST.
    client: reqwest::Client,
    profile: CaptivePortalProfile,
    /// The URL the user originally wanted to reach (e.g. http://google.com).
    /// After the portal agree flow, we check that redirects brought us back
    /// here — that, not an HTTP 200 from the portal itself, is the real
    /// proof the external network is unblocked.
    origin_url: String,
    login_url: String,
    /// Diagnostics from the last login attempt (form action, inputs, headers).
    diagnostics: std::sync::Mutex<Option<String>>,
}

impl CaptivePortal {
    /// Build a new captive portal client for a profile with a login URL.
    ///
    /// The `origin_url` (the URL the user originally wanted to reach) is
    /// extracted from the `url` query parameter of the login URL — captive
    /// portals redirect back to it after the agree flow. If no `url`
    /// parameter is present, falls back to the login URL itself.
    pub fn new(profile: CaptivePortalProfile, login_url: String) -> Self {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build captive portal HTTP client");
        let origin_url = extract_origin_url(&login_url).unwrap_or_else(|| login_url.clone());
        Self {
            client,
            profile,
            origin_url,
            login_url,
            diagnostics: std::sync::Mutex::new(None),
        }
    }

    /// Return the diagnostics string from the last login attempt, if any.
    pub fn diagnostics(&self) -> Option<String> {
        self.diagnostics.lock().unwrap().clone()
    }

    /// Log into the captive portal: GET the login page (following redirects
    /// to the actual portal page), parse the form, collect Set-Cookie
    /// headers, and POST the agree field back to the form's action URL with
    /// cookies preserved via reqwest's cookie jar.
    ///
    /// Returns `Ok(())` when the agree submission was accepted, or a
    /// `String` error describing why login failed.
    pub async fn login(&self) -> Result<(), String> {
        self.login_detailed().await.map(|_| ())
    }

    /// Like [`Self::login`] but returns a human-readable step-by-step
    /// summary of each phase (fetch page, find form, submit, response) for
    /// debugging via `/connect`.
    pub async fn login_detailed(&self) -> Result<String, String> {
        let mut summary = String::new();

        tracing::info!(
            "captive portal: GET login page (profile={}, url={})",
            self.profile.name,
            self.login_url
        );
        summary.push_str(&format!(
            "captive portal: GET login page (profile={}, url={})\n",
            self.profile.name, self.login_url
        ));

        // Step 1: Fetch the login page with cookies. reqwest follows
        // redirects automatically, so if the initial request gets redirected
        // to the captive portal page (e.g. http://google.com ->
        // https://free.wifi.gov.hk/.../login.html?zoneName=...&url=...),
        // `resp.url()` reflects the final page URL and the cookie jar holds
        // any Set-Cookie from the redirect chain.
        let resp = self
            .client
            .get(&self.login_url)
            .send()
            .await
            .map_err(|e| format!("failed to fetch captive portal login page: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                "captive portal: login page returned status {} for {}",
                status.as_u16(),
                self.login_url
            );
            return Err(format!(
                "captive portal login page returned status {}",
                status.as_u16()
            ));
        }

        tracing::info!(
            "captive portal: login page fetched, status={}, final_url={}",
            status.as_u16(),
            resp.url()
        );
        summary.push_str(&format!(
            "captive portal: login page fetched, status={}, final_url={}\n",
            status.as_u16(),
            resp.url()
        ));

        // Log all response headers for debugging.
        summary.push_str("captive portal: GET response headers:\n");
        for (name, value) in resp.headers() {
            if let Ok(v) = value.to_str() {
                summary.push_str(&format!("  {name}: {v}\n"));
            }
        }

        // The final URL after any redirects — captive portals often redirect
        // from a generic URL (google.com) to the portal-specific login page.
        let final_url: String = resp.url().as_str().to_string();

        // Log the effective cookie jar contents after the redirect chain.
        // reqwest::Client with cookie_store(true) auto-manages cookies, but
        // we expose them via the Jar for diagnostics only.
        let jar_cookies: Vec<String> = Vec::new(); // cookie store is managed internally by reqwest
        summary.push_str(&format!(
            "captive portal: jar cookies: {}\n",
            jar_cookies.join(", ")
        ));

        let html = resp
            .text()
            .await
            .map_err(|e| format!("failed to read login page body: {e}"))?;

        // Step 2: Parse the first form and its inputs from the HTML.
        // Static HTML first, then JS-injected form data (innerHTML/document.write
        // in inline scripts, or JSON configs in external scripts), then a
        // button[onclick] fallback as last resort.
        let form = match parse_first_form(&html) {
            Some(f) => f,
            None => match parse_js_form(&html, &final_url, &self.client).await {
                Some(f) => f,
                None => match parse_login_button(&html) {
                    Some(btn) => {
                        tracing::info!(
                            "captive portal: no <form> found; using JS login button on page (action={})",
                            btn.action
                        );
                        btn
                    }
                    None => {
                        return Err(format!(
                            "captive portal: reached login page for {} but no HTML form, \
                             JS-embedded form, or login button found \
                             (JavaScript-driven login; cannot auto-submit)",
                            self.login_url
                        ));
                    }
                },
            },
        };

        summary.push_str(&format!(
            "captive portal: parsed form, action={}, inputs={}\n",
            form.action,
            form.inputs.len()
        ));
        for input in &form.inputs {
            summary.push_str(&format!(
                "captive portal:   form input name={} value={}\n",
                input.name, input.value
            ));
        }

        // Resolve the form action against the *final* URL (after redirects),
        // not the original login URL, since the page may have moved.
        let action_url = resolve_action_url(&final_url, &form.action);

        tracing::info!(
            "captive portal: found form, action={}, inputs={}",
            action_url,
            form.inputs.len()
        );

        // Build form data: include all existing inputs, overriding the agree
        // field. Captive portals often carry the original target URL as a
        // hidden `url` parameter (e.g. ?url=http%3A%2F%2Fgoogle.com%2F).
        let mut form_data: HashMap<String, String> = HashMap::new();
        for input in &form.inputs {
            form_data.insert(input.name.clone(), input.value.clone());
        }
        // Merge static extra fields declared by the profile (e.g. GovWifi's
        // passcode). These only fill fields not already present from the form.
        for (name, value) in self.profile.extra_fields {
            form_data
                .entry(name.to_string())
                .or_insert_with(|| value.to_string());
        }
        // Override the agree field with the profile's agreed value so the
        // submit signals acceptance even when the form's own value is empty.
        form_data.insert(
            self.profile.agree_field.to_string(),
            self.profile.agree_value.to_string(),
        );

        // Step 3: Submit the agree POST via reqwest so the persistent cookie
        // jar is automatically attached (cookies from the GET + any redirect
        // Set-Cookie headers are sent without manually building a Cookie
        // header).
        tracing::info!(
            "captive portal: POST agree form (profile={}, field={}, action={})",
            self.profile.name,
            self.profile.agree_field,
            action_url
        );
        summary.push_str(&format!(
            "captive portal: POST agree form (profile={}, field={}, action={})\n",
            self.profile.name, self.profile.agree_field, action_url
        ));
        summary.push_str("captive portal: POST body:\n");
        for (k, v) in &form_data {
            summary.push_str(&format!("  {k}={v}\n"));
        }
        let mut req = self.client.post(&action_url).form(&form_data);

        // Disable automatic redirect following so we can inspect the raw
        // redirect response (3xx Location) and follow it manually while
        // preserving cookies — captive portals often redirect back to the
        // original URL via a 3xx that automatic following would swallow.
        let mut current_url = action_url.clone();
        let max_redirects = 10u32;
        let mut final_resp = None;
        for _ in 0..max_redirects {
            let resp = req
                .send()
                .await
                .map_err(|e| format!("failed to submit captive portal agree form: {e}"))?;
            let status = resp.status();
            let url = resp.url().clone();
            let headers = resp.headers().clone();
            // The cookie jar is updated automatically by reqwest on each
            // response (Set-Cookie headers).
            // Follow redirect manually if we get a 3xx with Location.
            if status.is_redirection()
                && let Some(loc) = headers.get(LOCATION).and_then(|v| v.to_str().ok())
            {
                let next_url = resolve_action_url(url.as_str(), loc);
                tracing::info!(
                    "captive portal: following redirect {status} {current_url} -> {next_url}"
                );
                summary.push_str(&format!(
                    "captive portal: redirect {status} -> {next_url}\n"
                ));
                current_url = next_url;
                let next_req = self.client.get(&current_url);
                req = next_req;
                continue;
            }
            final_resp = Some(resp);
            break;
        }
        let resp = final_resp.ok_or_else(|| "captive portal redirect loop".to_string())?;

        let status = resp.status();
        let final_url = resp.url().clone();
        let headers = resp.headers().clone();
        // Capture the response body so the user can see what the portal
        // returned after the agree submit (success page, error message, or
        // a JS redirect snippet). Do this before any redirect following.
        let body_text = resp.text().await.unwrap_or_default();

        tracing::info!(
            "captive portal: agree form submitted, status={}, final_url={}",
            status.as_u16(),
            final_url
        );
        summary.push_str(&format!(
            "captive portal: agree form submitted, status={}, final_url={}\n",
            status.as_u16(),
            final_url
        ));
        // Log response headers from the submit (Set-Cookie, Location, etc.).
        summary.push_str("captive portal: POST response headers:\n");
        for (name, value) in headers.iter() {
            if let Ok(v) = value.to_str() {
                summary.push_str(&format!("  {name}: {v}\n"));
            }
        }
        if let Some(loc) = headers.get(LOCATION).and_then(|v| v.to_str().ok()) {
            summary.push_str(&format!("captive portal: redirect Location: {loc}\n"));
        }
        // Log a truncated view of the response body so the user can tell if
        // the submit succeeded (e.g. a "success" / welcome message) or failed
        // (e.g. an error page or another login form).
        if !body_text.is_empty() {
            let preview = if body_text.len() > 2000 {
                format!("{}\n... (truncated, {} bytes total)", &body_text[..2000], body_text.len())
            } else {
                body_text.clone()
            };
            summary.push_str("captive portal: POST response body:\n");
            summary.push_str(&preview);
            summary.push('\n');
        }

        // A captive portal login is genuinely accepted only when one of:
        //   1. The agree form redirect brings us back to the origin URL
        //      (strongest proof the external network is unblocked), OR
        //   2. The response body no longer looks like a login page — i.e. it
        //      does not contain a form with the agree field, nor typical
        //      login/landing-page keywords ("login", "sign in", "agree",
        //      "accept", "captive", "portal", "wifi login"). A 2xx that just
        //      returns the landing/login page again is a failed submit, not
        //      a success.
        let still_on_login_page = Self::body_looks_like_login_page(&body_text);
        if current_url == self.origin_url || (!still_on_login_page && status.is_success()) {
            tracing::info!(
                "captive portal: agree form accepted by server (status={}, back_to_origin={})",
                status.as_u16(),
                current_url == self.origin_url
            );
            summary.push_str(&format!(
                "captive portal: agree form accepted by server (status={})\n",
                status.as_u16()
            ));
            return Ok(summary);
        }

        // If we got a non-success status, check if any redirect brought us back to origin.
        if current_url == self.origin_url {
            tracing::info!(
                "captive portal: agree form redirect returned to origin url {}",
                current_url
            );
            summary.push_str(&format!(
                "captive portal: agree form redirect returned to origin url {}\n",
                current_url
            ));
            return Ok(summary);
        }

        tracing::warn!(
            "captive portal: agree form not accepted; status={}, current={} origin={}, still_on_login_page={}",
            status.as_u16(),
            current_url,
            self.origin_url,
            still_on_login_page
        );
        Err(format!(
            "captive portal login did not succeed; server returned status {}, \
             current url {current} does not match origin url {origin}\n\
             response body:\n{body_text}",
            status.as_u16(),
            current = current_url,
            origin = self.origin_url,
            body_text = body_text
        ))
    }

    /// Heuristic: does the response body look like a captive-portal login /
    /// landing page (meaning the agree submit did NOT actually log us in)?
    fn body_looks_like_login_page(body: &str) -> bool {
        static AGREE_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"<input[^>]*name\s*=\s*['"]?(agree|accept|continue|terms|passcode)['"]?"#,
            )
            .expect("valid agree-field regex")
        });
        let lower = body.to_ascii_lowercase();
        // If there's still a form carrying an agree/accept/continue field, we
        // are back on the login page.
        let has_agree_field = AGREE_FIELD_RE.is_match(&lower);
        // Typical login/landing-page keywords.
        let has_login_keywords = [
            "login", "sign in", "sign-in", "agree", "accept", "continue",
            "captive portal", "wifi login", "wireless login", "hotspot",
            "authentication required",
        ]
        .iter()
        .any(|kw| lower.contains(kw));
        has_agree_field || has_login_keywords
    }

    /// The profile name.
    pub fn name(&self) -> &'static str {
        self.profile.name
    }

    /// The portal login URL.
    pub fn login_url(&self) -> &str {
        &self.login_url
    }

    /// The origin URL the user started from (e.g. http://google.com).
    /// Login is considered successful only if the agree form redirect
    /// brings us back here.
    pub fn origin_url(&self) -> &str {
        &self.origin_url
    }
}

/// Manager for multiple captive portals. Tries each portal in order until
/// one succeeds.
pub struct CaptivePortalManager {
    portals: Vec<CaptivePortal>,
}

impl CaptivePortalManager {
    /// Build a manager from profile name + login URL pairs.
    pub fn new(entries: &[(&'static str, String)]) -> Self {
        let portals = entries
            .iter()
            .filter_map(|(name, url)| {
                CAPTIVE_PORTAL_PROFILES
                    .iter()
                    .find(|p| p.name == *name)
                    .map(|p| CaptivePortal::new(p.clone(), url.clone()))
            })
            .collect();
        Self { portals }
    }

    /// Number of portals configured.
    pub fn portal_count(&self) -> usize {
        self.portals.len()
    }

    /// Iterate over the configured portals.
    pub fn portals(&self) -> std::slice::Iter<'_, CaptivePortal> {
        self.portals.iter()
    }

    /// Try logging into each portal in order. Returns `Ok(())` on the first
    /// successful login, or an aggregated error string if all fail.
    pub async fn login(&self) -> Result<(), String> {
        if self.portals.is_empty() {
            return Err("no captive portals configured".into());
        }
        let mut last_err = String::new();
        for portal in &self.portals {
            match portal.login().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = format!("{}: {e}", portal.login_url);
                    tracing::warn!("captive portal login failed for {}: {e}", portal.login_url);
                }
            }
        }
        Err(format!(
            "captive portal login failed on all portals: {last_err}"
        ))
    }
}

/// Parsed HTML form: action URL and input name/value pairs.
pub struct Form {
    pub action: String,
    pub inputs: Vec<Input>,
}

/// One form input: name and value.
pub struct Input {
    pub name: String,
    pub value: String,
}

/// Parse the first `<form>` element from HTML using scraper's DOM parser,
/// extracting its `action` attribute and all `<input>` name/value pairs.
///
/// This is intentionally minimal: captive portal login pages are usually
/// simple forms with hidden inputs and a single agree/submit field. We use
/// scraper (a CSS-selector HTML parser) instead of regex so malformed HTML
/// and missing attributes are handled correctly.
fn parse_first_form(html: &str) -> Option<Form> {
    let document = Html::parse_document(html);
    let form_selector = Selector::parse("form").ok()?;
    let form = document.select(&form_selector).next()?;

    let action = form.value().attr("action").unwrap_or_default().to_string();

    let input_selector = Selector::parse("input[name][value]").ok()?;
    let inputs: Vec<Input> = form
        .select(&input_selector)
        .map(|input| Input {
            name: input.value().attr("name").unwrap_or_default().to_string(),
            value: input.value().attr("value").unwrap_or_default().to_string(),
        })
        .collect();

    Some(Form { action, inputs })
}

/// Resolve a form action URL against the page URL. Handles relative paths
/// and protocol-relative URLs.
fn resolve_action_url(page_url: &str, action: &str) -> String {
    if action.is_empty() {
        return page_url.to_string();
    }
    // Absolute URL.
    if action.starts_with("http://") || action.starts_with("https://") {
        return action.to_string();
    }
    let parsed = match reqwest::Url::parse(page_url) {
        Ok(p) => p,
        Err(_) => return action.to_string(),
    };
    // Protocol-relative URL: //host/path
    if action.starts_with("//") {
        return format!("{}:{}", parsed.scheme(), action);
    }
    // Root-relative: /path
    if action.starts_with('/') {
        let host = parsed.host_str().unwrap_or("");
        return format!("{}://{}{}", parsed.scheme(), host, action);
    }
    // Relative path: resolve against page URL's directory.
    let base = page_url
        .rsplit_once('/')
        .map(|(base, _)| base)
        .unwrap_or(page_url);
    format!("{base}/{action}")
}

/// Extract all `<script src="...">` URLs from HTML using scraper.
fn extract_script_srcs(html: &str) -> Option<Vec<String>> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("script[src]").ok()?;
    let srcs: Vec<String> = document
        .select(&selector)
        .filter_map(|script| script.value().attr("src").map(|s| s.to_string()))
        .collect();
    if srcs.is_empty() { None } else { Some(srcs) }
}

/// Parse a JavaScript-driven login button (e.g. `<button onclick="Service.userLogin(...)">`).
///
/// Returns a synthetic `Form` whose action is empty (resolved to the page
/// URL by [`resolve_action_url`]), carrying all inputs found on the page
/// plus a synthetic agree field. Captive portals that render their login
/// button via JS often POST back to the same page URL with the agree field.
fn parse_login_button(html: &str) -> Option<Form> {
    let document = Html::parse_document(html);

    // Detect a login/submit button with an onclick handler (no <form> element).
    let btn_selector = Selector::parse("button[onclick]").ok()?;
    document.select(&btn_selector).next()?;

    // Collect all named inputs on the page.
    let input_selector = Selector::parse("input[name][value]").ok()?;
    let inputs: Vec<Input> = document
        .select(&input_selector)
        .map(|input| Input {
            name: input.value().attr("name").unwrap_or_default().to_string(),
            value: input.value().attr("value").unwrap_or_default().to_string(),
        })
        .collect();

    Some(Form {
        action: String::new(),
        inputs,
    })
}

/// Parse form data embedded in JavaScript within the HTML page.
///
/// Captive portals that render their login form purely via JS have no
/// `<form>` element in the raw HTML. Instead they often:
///
/// - assign an HTML form string to `innerHTML` / `document.write`,
/// - put the agree field name/value in a JS object/JSON config,
/// - load an external `<script src="...">` that emits the form HTML.
///
/// We scan inline `<script>` blocks and, for external scripts, fetch them
/// (same client/cookie jar) and scan their text too. We only extract
/// *declarative* form data — no JavaScript is executed.
///
/// Strategy:
/// 1. For every inline `<script>` and every fetched external script,
///    look for a string literal that looks like an HTML `<form>...</form>`
///    snippet (assigned to `innerHTML`, passed to `document.write`, etc.).
///    If found, parse that snippet as a form.
/// 2. Otherwise look for JSON-looking object literals in script text that
///    contain `agree`/`accept`/`continue`-ish field names alongside a form
///    `action` URL, and synthesize a `Form` from them.
///
/// `html` is the raw page HTML; `page_url` is the final URL used to resolve
/// relative script `src` URLs; `http` is the shared cookie-jar client used
/// to fetch external scripts.
async fn parse_js_form(html: &str, page_url: &str, http: &reqwest::Client) -> Option<Form> {
    let selector = Selector::parse("script").ok()?;

    // Do all scraper work in a scoped block so the `!Send` scraper types
    // (`Html`, `ElementRef`) are dropped before any `.await`.
    // Otherwise the async future is not `Send` and cannot be spawned.
    let (inline_texts, script_srcs): (Vec<String>, Vec<String>) = {
        let document = Html::parse_document(html);
        let mut inline_texts: Vec<String> = Vec::new();
        let mut script_srcs: Vec<String> = Vec::new();
        for script in document.select(&selector) {
            if let Some(src) = script.value().attr("src") {
                script_srcs.push(resolve_action_url(page_url, src));
                continue;
            }
            let text = script.text().next().filter(|t| !t.trim().is_empty());
            if let Some(text) = text {
                inline_texts.push(text.to_string());
            }
        }
        (inline_texts, script_srcs)
    };

    // Fetch external scripts (await happens outside the scraper loop).
    let mut external_texts: Vec<String> = Vec::new();
    for src_url in script_srcs {
        if let Some(text) = fetch_script_text(http, &src_url).await {
            external_texts.push(text);
        }
    }

    let script_texts: Vec<String> = inline_texts.into_iter().chain(external_texts).collect();

    // 1. Look for an HTML form snippet embedded in script text.
    for text in &script_texts {
        if let Some(form) = extract_form_snippet(text) {
            return Some(form);
        }
    }

    // 2. Look for a JSON-like config object with agree-ish fields.
    for text in &script_texts {
        if let Some(form) = extract_js_config_form(text) {
            return Some(form);
        }
    }

    // 3. Look for jQuery/AJAX submit patterns, e.g.
    //    `$.post("/login", {agree: "yes"})`,
    //    `$.ajax({url: "/login", data: {agree: "yes"}})`,
    //    `axios.post("/login", {agree: "yes"})`.
    //    Captive portals that bind a login button to `onclick="Service.userLogin(...)"`
    //    usually perform the actual login via an AJAX POST inside that function,
    //    so we scan the external JS files for the submit URL and data fields.
    for text in &script_texts {
        if let Some(form) = extract_ajax_form(text) {
            return Some(form);
        }
    }

    // 4. Fall back to executing JavaScript in a sandboxed QuickJS environment.
    // This handles portals that generate forms dynamically via complex JS logic.

    // For simplicity, we'll execute all scripts. In practice, inline scripts
    // define variables/functions that external scripts use.
    if let Ok(js_result) = execute_portal_js(
        script_texts.clone(), // all scripts as inline (they're already fetched)
        vec![],               // no additional external scripts
        page_url.to_string(),
        page_url.to_string(), // origin_url fallback to page_url
    )
    .await
        && js_result.submitted {
            let mut inputs: Vec<Input> = Vec::new();
            for (name, value) in js_result.fields {
                inputs.push(Input { name, value });
            }
            let action = js_result.action.unwrap_or_default();
            tracing::debug!(
                "captive portal: JS execution captured form action={}, fields={}",
                action,
                inputs.len()
            );
            return Some(Form { action, inputs });
        }

    None
}

/// Look for jQuery/AJAX submit patterns in JS text and synthesize a `Form`.
///
/// Handles:
/// - `$.post("url", { key: "val", ... })`
/// - `$.ajax({ url: "url", data: {...} })`
/// - `axios.post("url", { key: "val", ... })`
/// - `Service.request({url: "url", ...}, { key: "val", ... })` — GovWifi-style
///   portal login where `Service.request` wraps `$.ajax` with an API path.
///
/// Returns a `Form` whose action is the submit URL and whose inputs are the
/// agree/accept/continue/terms fields found in the data object.
fn extract_ajax_form(js: &str) -> Option<Form> {
    // Pattern: $.post("url", {...}) or $.post('url', {...})
    if let Some(form) = extract_jquery_post(js) {
        return Some(form);
    }
    // Pattern: $.ajax({ url: "url", ..., data: {...} })
    if let Some(form) = extract_jquery_ajax(js) {
        return Some(form);
    }
    // Pattern: axios.post("url", {...})
    if let Some(form) = extract_axios_post(js) {
        return Some(form);
    }
    // Pattern: Service.request({url: "...", ...}, {...}) — used by GovWifi
    // and other portals that wrap $.ajax behind a Service helper.
    if let Some(form) = extract_service_request(js) {
        return Some(form);
    }
    None
}

/// Extract `Service.request({url: "...", ...}, data)` from JS text.
///
/// GovWifi-style portals bind the login button to `Service.userLogin(...)`,
/// which internally calls `Service.request({url: Common.apiPath +
/// "/wifi/login", type: "POST"}, {passcode: "..."})`. We scan for the
/// `Service.request({url: ...}` pattern, extract the url expression and the
/// second argument (data object), and synthesize a `Form`.
///
/// Returns `None` when no `Service.request` call with a url and data fields
/// is found.
fn extract_service_request(js: &str) -> Option<Form> {
    // Match `Service.request({...url: <expr>...}, {...data...})`.
    // We tolerate whitespace and other keys between `url` and the closing
    // brace of the options object. The url expression may be a plain string
    // literal (`url: "/wifi/login"`) or a concatenation
    // (`url: Common.apiPath + "/wifi/login"`).
    let re = regex::Regex::new(
        r#"(?i)Service\.request\(\s*\{\s*[^{}]*?url\s*:\s*([^,}]*?)\s*[^{}]*?\}\s*,\s*(\{[^{}]*\})"#,
    )
    .ok()?;
    for caps in re.captures_iter(js) {
        let url_expr = caps.get(1)?.as_str().trim();
        // Extract the first string literal from the url expression.
        let url_val = extract_string_literal(url_expr)?;
        let data_obj = caps.get(2)?.as_str();
        // Extract all field names from the data object (GovWifi uses
        // `passcode`, not an agree-ish field). Values that are string
        // literals are preserved; others default to empty and are filled
        // in by the profile's agree field during submit.
        let inputs = extract_all_fields(data_obj);
        if !inputs.is_empty() {
            return Some(Form {
                action: url_val,
                inputs,
            });
        }
    }
    None
}

/// Extract the first double- or single-quoted string literal from `s`,
/// returning its unquoted contents.
fn extract_string_literal(s: &str) -> Option<String> {
    let re = regex::Regex::new(r#"["']([^"']+)["']"#).ok()?;
    re.captures(s)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract all `key: value` pairs from a JS object snippet like
/// `{passcode: passcode, csrf: "abc"}`. Fields with string-literal values
/// keep them; fields with unquoted values (JS variable references like
/// `passcode: passcode`) default to empty — the caller fills in the
/// profile's agree value for the primary field during submit.
fn extract_all_fields(data_obj: &str) -> Vec<Input> {
    let mut inputs = Vec::new();
    // Match `key: "value"` (string literal) or `key: <unquoted>`.
    let re = match regex::Regex::new(
        r#"["']?([a-zA-Z_][a-zA-Z0-9_]*)["']?\s*:\s*(?:["']([^"']*)["']|([^,{}]+))"#,
    ) {
        Ok(r) => r,
        Err(_) => return inputs,
    };
    for caps in re.captures_iter(data_obj) {
        // `continue` instead of `?` because this returns Vec<Input>, not Option.
        let Some(name_cap) = caps.get(1) else {
            continue;
        };
        let name = name_cap.as_str().to_string();
        // Only keep string-literal values; unquoted identifiers (e.g.
        // `passcode: passcode`) default to empty.
        let value = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        inputs.push(Input { name, value });
    }
    inputs
}

/// Extract `$.post(url, data)` from JS text.
fn extract_jquery_post(js: &str) -> Option<Form> {
    let re = regex::Regex::new(r#"(?i)\$\.post\(\s*["']([^"']+)["']\s*,\s*(\{[^{}]*\})"#).ok()?;
    for caps in re.captures_iter(js) {
        // Capture groups: group 1 = url, group 2 = data object
        let url_val = caps.get(1)?.as_str().to_string();
        let data_obj = caps.get(2)?.as_str();
        let inputs = extract_agree_fields(data_obj);
        if !inputs.is_empty() {
            return Some(Form {
                action: url_val,
                inputs,
            });
        }
    }
    None
}

/// Extract `$.ajax({ url: "url", ..., data: {...} })` from JS text.
fn extract_jquery_ajax(js: &str) -> Option<Form> {
    // Match `$.ajax({ ... url: "url" ... data: { ... } })` allowing other keys
    // between url and data. We scan for `$.ajax({` then find the matching close
    // brace, then extract url and data from within.
    let ajax_re = regex::Regex::new(r"(?i)\$\.ajax\(\s*\{").ok()?;
    // Compiled once outside the loop to avoid `regex_creation_in_loops`.
    let data_re = regex::Regex::new(r"(?i)data\s*:\s*\{").ok()?;
    for m in ajax_re.find_iter(js) {
        let start = m.end();
        let rest = &js[start..];
        // Find the matching closing brace for the options object.
        let end = find_matching_brace(rest)?;
        let opts = &rest[..end];
        let url = extract_json_str(opts, "url")?;
        // Locate the `data: {...}` object using brace matching so nested
        // braces are handled correctly.
        if let Some(dm) = data_re.find(opts) {
            let data_start = dm.end() - 1; // position of the opening `{`
            let data_rest = &opts[data_start..];
            let data_end = find_matching_brace(data_rest)?;
            let data_obj = &data_rest[..data_end];
            let inputs = extract_agree_fields(data_obj);
            if !inputs.is_empty() {
                return Some(Form {
                    action: url,
                    inputs,
                });
            }
        }
    }
    None
}

/// Extract `axios.post(url, data)` from JS text.
fn extract_axios_post(js: &str) -> Option<Form> {
    let re =
        regex::Regex::new(r#"(?i)axios\.post\(\s*["']([^"']+)["']\s*,\s*(\{[^{}]*\})"#).ok()?;
    for caps in re.captures_iter(js) {
        let url_val = caps.get(1)?.as_str().to_string();
        let data_obj = caps.get(2)?.as_str();
        let inputs = extract_agree_fields(data_obj);
        if !inputs.is_empty() {
            return Some(Form {
                action: url_val,
                inputs,
            });
        }
    }
    None
}

/// Find the index of the matching closing brace for an object literal that
/// starts at position 0 of `s`, respecting nested braces and string literals.
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            if c == '\\' {
                escaped = true;
            } else if c == '"' || c == '\'' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract agree/accept/continue/terms field name/value pairs from a data
/// object snippet like `{agree: "yes", csrf: "abc123"}`.
fn extract_agree_fields(data_obj: &str) -> Vec<Input> {
    let mut inputs = Vec::new();
    for field in ["agree", "accept", "continue", "terms"] {
        if let Some(val) = extract_json_str(data_obj, field) {
            inputs.push(Input {
                name: field.to_string(),
                value: val,
            });
        }
    }
    inputs
}

// 3. Look for jQuery/AJAX submit patterns, e.g.
//    `$.post("/login", {agree: "yes"})`,
//    `$.ajax({url: "/login", data: {agree: "yes"}})`,
//    `axios.post("/login", {agree: "yes"})`.
//    Captive portals that bind a login button to `onclick="Service.userLogin(...)"`
//    usually perform the actual login via an AJAX POST inside that function,
//    so we scan the external JS files for the submit URL and data fields.
pub fn extract_form_snippet(js: &str) -> Option<Form> {
    // Find candidate form snippets between <form ...> and </form>.
    let form_start_re = regex::Regex::new(r"<form\b[^>]*>").ok()?;
    for m in form_start_re.find_iter(js) {
        let start = m.start();
        let rest = &js[start..];
        let end = rest.find("</form>")? + 7; // include closing tag length
        let snippet = &rest[..end];
        if let Some(form) = parse_first_form(snippet) {
            return Some(form);
        }
    }
    None
}

/// Look for a JSON-like object literal in JS text that carries form field
/// configuration (e.g. `{action: "/login", fields: {agree: "yes"}}`) and
/// synthesize a `Form` from it.
fn extract_js_config_form(js: &str) -> Option<Form> {
    // Very heuristic: find `{...}` objects containing a field name that
    // looks like an agree/accept/continue field.
    let re = regex::Regex::new(r"\{[^{}]*\b(agree|accept|continue|terms)\b[^{}]*\}").ok()?;
    for m in re.find_iter(js) {
        let obj = m.as_str();
        // Try to extract an `action` or `url` key from the same object.
        let action = extract_json_str(obj, "action")
            .or_else(|| extract_json_str(obj, "url"))
            .or_else(|| extract_json_str(obj, "submit_url"))
            .unwrap_or_default();
        let mut inputs: Vec<Input> = Vec::new();
        for field in ["agree", "accept", "continue", "terms"] {
            if let Some(val) = extract_json_str(obj, field) {
                inputs.push(Input {
                    name: field.to_string(),
                    value: val,
                });
            }
        }
        if inputs.is_empty() {
            continue;
        }
        return Some(Form { action, inputs });
    }
    None
}

/// Extract a string value for a key from a JSON-like object snippet, e.g.
/// `action: "/login"` or `action:"/login"` or `agree: "yes"`.
fn extract_json_str(obj: &str, key: &str) -> Option<String> {
    // All literal braces are doubled (`{{`/`}}`) because this is a format! string.
    // The value must be followed by a comma, `}`, `}`, or end of string.
    // The key may be quoted (`"action": ...`) or unquoted (`action: ...`).
    let pattern = format!(
        r#"["']?{key}["']?\s*:?\s*["']?([^"',{{}}]+)["']?\s*(?:[,{{}}]|$)"#,
        key = regex::escape(key)
    );
    let re = Regex::new(&pattern).ok()?;
    re.captures(obj)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().trim_matches('"').to_string())
}

/// Fetch the text of an external `<script src>` URL using the shared client
/// (cookies preserved). Returns `None` on any failure — external scripts are
/// best-effort.
async fn fetch_script_text(http: &reqwest::Client, url: &str) -> Option<String> {
    let resp = http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// Extract the origin URL (the URL the user originally wanted to reach) from
/// a captive portal login URL.
///
/// Captive portals carry the original target URL as a `url` query parameter,
/// typically URL-encoded (e.g. `url=http%3A%2F%2Fgoogle.com%2F`). This parses
/// the query string and returns the decoded value of the first `url`
/// parameter. Returns `None` if no `url` parameter is present.
fn extract_origin_url(login_url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(login_url).ok()?;
    let url_value = parsed.query_pairs().find(|(k, _)| k == "url")?.1;
    Some(url_value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_parse_first_form_extracts_action_and_inputs() {
        let html = r#"
<html>
<body>
<form action="/login" method="POST">
  <input type="hidden" name="csrf" value="abc123">
  <input type="checkbox" name="agree" value="yes">
  <input type="submit" value="I agree">
</form>
</body>
</html>
"#;
        let form = parse_first_form(html).expect("should parse form");
        assert_eq!(form.action, "/login");
        // Only inputs with both name and value are captured; the submit
        // button has no name, so it's skipped.
        assert_eq!(form.inputs.len(), 2);
        let agree = form
            .inputs
            .iter()
            .find(|i| i.name == "agree")
            .expect("should have agree input");
        assert_eq!(agree.value, "yes");
    }

    #[test]
    fn test_parse_first_form_no_form_returns_none() {
        let html = "<html><body>Hello, world!</body></html>";
        assert!(parse_first_form(html).is_none());
    }

    #[test]
    fn test_parse_first_form_missing_action_defaults_empty() {
        let html = r#"<form><input name="agree" value="yes"></form>"#;
        let form = parse_first_form(html).expect("should parse form");
        assert_eq!(form.action, "");
    }

    #[test]
    fn test_resolve_action_url_relative() {
        assert_eq!(
            resolve_action_url("https://portal.example.com/login", "/auth"),
            "https://portal.example.com/auth"
        );
    }

    #[test]
    fn test_resolve_action_url_absolute() {
        assert_eq!(
            resolve_action_url(
                "https://portal.example.com/login",
                "https://other.example.com/auth"
            ),
            "https://other.example.com/auth"
        );
    }

    #[test]
    fn test_resolve_action_url_relative_path() {
        // Relative path resolves against the page URL's directory (the last
        // path segment is treated as a file, like HTML base resolution).
        assert_eq!(
            resolve_action_url("https://portal.example.com/login/", "auth"),
            "https://portal.example.com/login/auth"
        );
        assert_eq!(
            resolve_action_url("https://portal.example.com/login", "auth"),
            "https://portal.example.com/auth"
        );
    }

    #[test]
    fn test_resolve_action_url_empty_action() {
        assert_eq!(
            resolve_action_url("https://portal.example.com/login", ""),
            "https://portal.example.com/login"
        );
    }

    #[test]
    fn test_resolve_action_url_protocol_relative() {
        assert_eq!(
            resolve_action_url(
                "https://portal.example.com/login",
                "//portal.example.com/auth"
            ),
            "https://portal.example.com/auth"
        );
    }

    #[test]
    fn test_manager_empty_returns_error() {
        let manager = CaptivePortalManager::new(&[]);
        let result = futures::executor::block_on(manager.login());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("no captive portals configured")
        );
    }

    #[test]
    fn test_profiles_have_default_names() {
        let names: Vec<&str> = CAPTIVE_PORTAL_PROFILES.iter().map(|p| p.name).collect();
        assert!(names.contains(&"agree"));
        assert!(names.contains(&"accept"));
        assert!(names.contains(&"continue"));
        assert!(names.contains(&"terms"));
    }

    #[test]
    fn test_profile_names_are_unique() {
        let names: Vec<&str> = CAPTIVE_PORTAL_PROFILES.iter().map(|p| p.name).collect();
        let mut seen = HashMap::new();
        for name in &names {
            *seen.entry(name).or_insert(0usize) += 1;
        }
        for (name, count) in &seen {
            assert_eq!(*count, 1, "duplicate profile name: {name}");
        }
    }

    #[test]
    fn test_origin_url_is_stored_from_login_url() {
        let profile = CaptivePortalProfile {
            name: "agree",
            agree_field: "agree",
            agree_value: "yes",
            extra_fields: &[],
        };
        let portal = CaptivePortal::new(profile.clone(), "http://google.com/".to_string());
        assert_eq!(portal.origin_url(), "http://google.com/");
        assert_eq!(portal.login_url(), "http://google.com/");
    }

    #[test]
    fn test_extract_origin_url_from_url_param() {
        let login = "https://free.wifi.gov.hk/GovWifiUI_/userlanding/tc/login.html?zoneName=KPRH160&ssid=Wi-Fi.HK&url=http%3A%2F%2Fexample.com%2F&";
        assert_eq!(
            extract_origin_url(login).as_deref(),
            Some("http://example.com/")
        );
    }

    #[test]
    fn test_extract_origin_url_no_url_param_returns_none() {
        let login =
            "https://free.wifi.gov.hk/GovWifiUI_/userlanding/tc/login.html?zoneName=KPRH160";
        assert_eq!(extract_origin_url(login), None);
    }

    #[test]
    fn test_origin_url_from_url_param() {
        let profile = CaptivePortalProfile {
            name: "agree",
            agree_field: "agree",
            agree_value: "yes",
            extra_fields: &[],
        };
        let login = "https://free.wifi.gov.hk/GovWifiUI_/userlanding/tc/login.html?zoneName=KPRH160&ssid=Wi-Fi.HK&url=http%3A%2F%2Fexample.com%2F&";
        let portal = CaptivePortal::new(profile.clone(), login.to_string());
        assert_eq!(portal.origin_url(), "http://example.com/");
        assert_eq!(portal.login_url(), login);
    }

    #[test]
    fn test_parse_login_button_no_form_button_only() {
        // Simulates the GovWifi portal page: no <form>, just a button with
        // an onclick handler and no named inputs.
        let html = r#"
        <html>
        <body>
          <div class="btn_wrapper">
            <button type="button" class="btn btn-default btn_gw_default wide"
              onclick="Service.userLogin(this,'.loadingGif')">同意</button>
          </div>
        </body>
        </html>
        "#;
        let form = parse_login_button(html).expect("should find login button");
        assert_eq!(form.action, "");
        assert_eq!(form.inputs.len(), 0);
    }

    #[test]
    fn test_parse_login_button_with_inputs() {
        let html = r#"
        <html>
        <body>
          <input type="hidden" name="csrf" value="abc123">
          <button type="button" onclick="Service.userLogin(this)">同意</button>
        </body>
        </html>
        "#;
        let form = parse_login_button(html).expect("should find login button");
        assert_eq!(form.action, "");
        assert_eq!(form.inputs.len(), 1);
        assert_eq!(form.inputs[0].name, "csrf");
        assert_eq!(form.inputs[0].value, "abc123");
    }

    #[test]
    fn test_parse_login_button_no_button_returns_none() {
        let html = "<html><body>No button here</body></html>";
        assert!(parse_login_button(html).is_none());
    }

    #[test]
    fn test_extract_form_snippet_finds_form_in_js() {
        let js = r#"
        var el = document.createElement('div');
        el.innerHTML = '<form action="/login" method="POST"><input name="agree" value="yes"></form>';
        document.body.appendChild(el);
        "#;
        let form = extract_form_snippet(js).expect("should find form");
        assert_eq!(form.action, "/login");
        assert_eq!(form.inputs.len(), 1);
        assert_eq!(form.inputs[0].name, "agree");
    }

    #[test]
    fn test_extract_form_snippet_no_form_returns_none() {
        let js = "var x = 1; console.log('hello');";
        assert!(extract_form_snippet(js).is_none());
    }

    #[test]
    fn test_extract_jquery_post_extracts_url_and_fields() {
        let js = r#"$.post("/login", {agree: "yes", csrf: "abc123"});"#;
        let form = extract_jquery_post(js).expect("should parse $.post");
        assert_eq!(form.action, "/login");
        assert_eq!(form.inputs.len(), 1);
        assert_eq!(form.inputs[0].name, "agree");
        assert_eq!(form.inputs[0].value, "yes");
    }

    #[test]
    fn test_extract_jquery_post_no_agree_field_returns_none() {
        let js = r#"$.post("/login", {foo: "bar"});"#;
        assert!(extract_jquery_post(js).is_none());
    }

    #[test]
    fn test_extract_jquery_ajax_extracts_url_and_fields() {
        let js = r#"$.ajax({url: "/login", type: "POST", data: {agree: "true"}});"#;
        let form = extract_jquery_ajax(js).expect("should parse $.ajax");
        assert_eq!(form.action, "/login");
        assert_eq!(form.inputs.len(), 1);
        assert_eq!(form.inputs[0].name, "agree");
        assert_eq!(form.inputs[0].value, "true");
    }

    #[test]
    fn test_extract_axios_post_extracts_url_and_fields() {
        let js = r#"axios.post("/submit", {accept: "true"});"#;
        let form = extract_axios_post(js).expect("should parse axios.post");
        assert_eq!(form.action, "/submit");
        assert_eq!(form.inputs.len(), 1);
        assert_eq!(form.inputs[0].name, "accept");
        assert_eq!(form.inputs[0].value, "true");
    }

    #[test]
    fn test_find_matching_brace() {
        // i+1 is the exclusive end index past the matching close brace.
        assert_eq!(find_matching_brace("{}"), Some(2));
        assert_eq!(find_matching_brace("{a: 1, b: {c: 2}}"), Some(17));
        assert_eq!(find_matching_brace("{a: '}'}"), Some(8));
        assert_eq!(find_matching_brace("{unclosed"), None);
    }

    #[test]
    fn test_extract_js_config_form_synthesizes_form() {
        let js = r#"
        var config = {action: "/submit", agree: "true", csrf: "abc123"};
        "#;
        let form = extract_js_config_form(js).expect("should synthesize form");
        assert_eq!(form.action, "/submit");
        assert_eq!(form.inputs.len(), 1);
        assert_eq!(form.inputs[0].name, "agree");
        assert_eq!(form.inputs[0].value, "true");
    }

    #[test]
    fn test_extract_js_config_form_no_agree_field_returns_none() {
        let js = r#"
        var config = {action: "/submit", foo: "bar"};
        "#;
        assert!(extract_js_config_form(js).is_none());
    }

    #[test]
    fn test_extract_json_str_extracts_value() {
        let obj = r#"action: "/login", agree: "yes"#;
        assert_eq!(extract_json_str(obj, "action").as_deref(), Some("/login"));
        assert_eq!(extract_json_str(obj, "agree").as_deref(), Some("yes"));
        assert!(extract_json_str(obj, "missing").is_none());
    }

    #[test]
    fn test_extract_json_str_quoted_keys() {
        let obj = r#"{"action": "/login", "agree": "true"}"#;
        assert_eq!(extract_json_str(obj, "action").as_deref(), Some("/login"));
        assert_eq!(extract_json_str(obj, "agree").as_deref(), Some("true"));
    }
}
