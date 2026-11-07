//! Captive portal login for LLM network error recovery.
//!
//! When an LLM API call fails with a transport-level network error, the
//! agent may be behind a captive portal (e.g. a Wi-Fi login page). This
//! module fetches the portal login page, extracts the HTML form, submits
//! an "agree"/"accept"/"continue" field, and the caller then retries the LLM call.
//!
//! Portals are defined as builtin profiles. Each profile carries its own
//! cookie store so cookies from one portal never leak to another.
//!
//! No TOML configuration is required — profiles are compiled in and
//! selected by name at runtime.

use regex::Regex;
use reqwest::header::SET_COOKIE;
use std::collections::HashMap;

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
    },
    CaptivePortalProfile {
        name: "accept",
        agree_field: "accept",
        agree_value: "true",
    },
    CaptivePortalProfile {
        name: "continue",
        agree_field: "continue",
        agree_value: "Continue",
    },
    CaptivePortalProfile {
        name: "terms",
        agree_field: "terms",
        agree_value: "agreed",
    },
];

/// Captive portal URLs for manual testing (no config needed).
///
/// Add a portal URL here to test the page fetch + form submit logic via
/// `/connect`. Each URL is tried against every `CAPTIVE_PORTAL_PROFILES`
/// profile until one succeeds.
pub const TEST_PORTALS: &[&str] = &[
    "https://free.wifi.gov.hk/GovWifiUI_/userlanding/tc/login.html?&url=http%3A%2F%2Fgoogle.com%2F&",
];

/// Captive portal client for one portal. Holds cookies collected from the
/// login page so they can be sent with the agree POST.
pub struct CaptivePortal {
    client: reqwest::Client,
    profile: CaptivePortalProfile,
    login_url: String,
    cookies: std::sync::Mutex<HashMap<String, String>>,
}

impl CaptivePortal {
    /// Build a new captive portal client for a profile with a login URL.
    pub fn new(profile: CaptivePortalProfile, login_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build captive portal HTTP client");
        Self {
            client,
            profile,
            login_url,
            cookies: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Log into the captive portal: GET the login page (following redirects
    /// to the actual portal page), parse the form, collect Set-Cookie
    /// headers, and POST the agree field back to the form's action URL with
    /// the cookies attached.
    ///
    /// Returns `Ok(())` when the agree submission was accepted, or a
    /// `String` error describing why login failed.
    pub async fn login(&self) -> Result<(), String> {
        self.login_detailed().await.map(|_| ())
    }

    /// Like [`Self::login`] but returns a human-readable step-by-step
    /// summary of each phase (fetch page, fetch JS, find form, submit,
    /// response) for debugging via `/connect`.
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

        // Fetch the login page. reqwest follows redirects automatically, so
        // if the initial request gets redirected to the captive portal page
        // (e.g. http://google.com →
        // https://free.wifi.gov.hk/.../login.html?zoneName=...&url=...),
        // `resp.url()` reflects the final page URL.
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
            "captive portal: login page fetched, status={}\n",
            status.as_u16()
        ));

        // The final URL after any redirects — captive portals often redirect
        // from a generic URL (google.com) to the portal-specific login page.
        let final_url: String = resp.url().as_str().to_string();

        // Collect cookies from all Set-Cookie headers.
        let mut cookie_count = 0;
        for set_cookie in resp.headers().get_all(SET_COOKIE).iter() {
            if let Ok(values) = set_cookie.to_str() {
                for cookie in values.split(';') {
                    if let Some((name, value)) = cookie.split_once('=') {
                        let name = name.trim();
                        let value = value.trim();
                        if !name.is_empty() && !value.is_empty() {
                            self.cookies
                                .lock()
                                .unwrap()
                                .insert(name.to_string(), value.to_string());
                            cookie_count += 1;
                        }
                    }
                }
            }
        }
        tracing::debug!("captive portal: collected {cookie_count} cookies");
        summary.push_str(&format!(
            "captive portal: collected {cookie_count} cookies\n"
        ));

        let html = resp
            .text()
            .await
            .map_err(|e| format!("failed to read login page body: {e}"))?;

        // Fetch any JS files referenced by <script src> tags and append their
        // text to the HTML so JS-driven forms (e.g. GovWiFi) can be parsed.
        let mut integrated = html.clone();
        if let Some(scripts) = extract_script_srcs(&html) {
            for src in scripts {
                let js_url = resolve_action_url(&final_url, &src);
                summary.push_str(&format!("captive portal: fetching JS {js_url}\n"));
                match self.client.get(&js_url).send().await {
                    Ok(js_resp) => {
                        if let Ok(js_text) = js_resp.text().await {
                            integrated.push_str("\n<!-- JS: ");
                            integrated.push_str(&js_url);
                            integrated.push_str(" -->\n");
                            integrated.push_str(&js_text);
                        }
                    }
                    Err(e) => {
                        tracing::debug!("captive portal: failed to fetch JS {js_url}: {e}");
                        summary.push_str(&format!(
                            "captive portal: failed to fetch JS {js_url}: {e}\n"
                        ));
                    }
                }
            }
        }

        // Parse the first form and find its action + inputs. Some captive
        // portals drive login via JavaScript (no <form> element in HTML),
        // but the JS file carries the form action/inputs, which we now
        // integrated into `integrated`.
        let form = match parse_first_form(&integrated) {
            Some(f) => f,
            None => match parse_login_button(&integrated) {
                Some(btn) => {
                    tracing::info!(
                        "captive portal: no <form> found; using JS login button on page (action={})",
                        btn.action
                    );
                    btn
                }
                None => {
                    return Err(format!(
                        "captive portal: reached login page for {} but no HTML form or login button found \
                         (JavaScript-driven login; cannot auto-submit)",
                        self.login_url
                    ));
                }
            },
        };

        summary.push_str(&format!(
            "captive portal: parsed form, action={}, inputs={}\n",
            form.action,
            form.inputs.len()
        ));

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
        form_data.insert(
            self.profile.agree_field.to_string(),
            self.profile.agree_value.to_string(),
        );

        // Build Cookie header from collected cookies.
        let cookie_header = self
            .cookies
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");

        // Submit the agree POST.
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
        let mut req = self.client.post(&action_url).form(&form_data);
        if !cookie_header.is_empty() {
            req = req.header("Cookie", cookie_header);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("failed to submit captive portal agree form: {e}"))?;

        let status = resp.status();
        tracing::info!(
            "captive portal: agree form submitted, status={}",
            status.as_u16()
        );
        summary.push_str(&format!(
            "captive portal: agree form submitted, status={}\n",
            status.as_u16()
        ));

        // Captive portals commonly redirect (3xx) to the original URL after a
        // successful login, so treat 2xx and 3xx as success.
        if !status.is_success() && !status.is_redirection() {
            return Err(format!(
                "captive portal agree form returned status {}",
                status.as_u16()
            ));
        }

        Ok(summary)
    }

    /// The profile name.
    pub fn name(&self) -> &'static str {
        self.profile.name
    }

    /// The portal login URL.
    pub fn login_url(&self) -> &str {
        &self.login_url
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
struct Form {
    action: String,
    inputs: Vec<Input>,
}

/// One form input: name and value.
struct Input {
    name: String,
    value: String,
}

/// Parse the first `<form>` element from HTML, extracting its `action`
/// attribute and all `<input>` name/value pairs.
///
/// This is intentionally minimal: captive portal login pages are usually
/// simple forms with hidden inputs and a single agree/submit field. We use
/// regex instead of a full HTML parser to avoid pulling in a heavy
/// dependency.
fn parse_first_form(html: &str) -> Option<Form> {
    let form_re = Regex::new(r"(?is)<form[^>]*>.*?</form>").ok()?;
    let form_match = form_re.find(html)?;
    let form_html = form_match.as_str();

    // Extract action attribute.
    let action_re = Regex::new(r#"(?i)action\s*=\s*["']([^"']*)["']"#).ok()?;
    let action = action_re
        .captures(form_html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();

    // Extract all inputs with name and value.
    let input_re = Regex::new(
        r#"(?i)<input[^>]*name\s*=\s*["']([^"']*)["'][^>]*value\s*=\s*["']([^"']*)["']"#,
    )
    .ok()?;
    let inputs = input_re
        .captures_iter(form_html)
        .filter_map(|c| {
            let name = c.get(1)?.as_str().to_string();
            let value = c.get(2)?.as_str().to_string();
            Some(Input { name, value })
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

/// Parse a JavaScript-driven login button (e.g. `<button onclick="Service.userLogin(...)">`).
///
/// Returns a synthetic `Form` whose action is empty (resolved to the page
/// URL by [`resolve_action_url`]), carrying all inputs found on the page
/// plus a synthetic agree field. Captive portals that render their login
/// button via JS often POST back to the same page URL with the agree field.
fn parse_login_button(html: &str) -> Option<Form> {
    // Detect a login/submit button without a <form> element.
    let btn_re = Regex::new(r#"(?i)<button[^>]*onclick\s*=\s*["'][^"']*["'][^>]*>"#).ok()?;
    btn_re.find(html)?;

    // Collect all named inputs on the page.
    let input_re = Regex::new(
        r#"(?i)<input[^>]*name\s*=\s*["']([^"']*)["'][^>]*value\s*=\s*["']([^"']*)["']"#,
    )
    .ok()?;
    let inputs: Vec<Input> = input_re
        .captures_iter(html)
        .filter_map(|c| {
            let name = c.get(1)?.as_str().to_string();
            let value = c.get(2)?.as_str().to_string();
            Some(Input { name, value })
        })
        .collect();

    Some(Form {
        action: String::new(),
        inputs,
    })
}

/// Extract all `<script src="...">` URLs from HTML.
fn extract_script_srcs(html: &str) -> Option<Vec<String>> {
    let re = Regex::new(r#"(?i)<script[^>]*src\s*=\s*["']([^"']*)["'][^>]*>"#).ok()?;
    let srcs: Vec<String> = re
        .captures_iter(html)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    if srcs.is_empty() { None } else { Some(srcs) }
}
mod tests {
    use super::*;

    #[test]
    fn test_extract_script_srcs() {
        let html = r#"<html><head><script src="../js/main.js"></script><script src="https://cdn.example.com/lib.js"></script></head><body></body></html>"#;
        let srcs = extract_script_srcs(html).expect("should find scripts");
        assert_eq!(srcs.len(), 2);
        assert!(srcs.contains(&"../js/main.js".to_string()));
        assert!(srcs.contains(&"https://cdn.example.com/lib.js".to_string()));
    }

    #[test]
    fn test_extract_script_srcs_none() {
        let html = "<html><body>No scripts here</body></html>";
        assert!(extract_script_srcs(html).is_none());
    }

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
}
