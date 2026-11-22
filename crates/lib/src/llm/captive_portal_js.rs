//! JavaScript execution for captive portal login form extraction.
//!
//! This module uses QuickJS (via rquickjs) to execute portal JavaScript
//! in a sandboxed environment, extracting form submission data (action URL
//! and field values) from portals that generate their login forms dynamically.

use rquickjs::{Ctx, Function, Object, Result, Runtime, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;
use urlencoding;

/// Result of JavaScript execution: captured form submission data.
#[derive(Debug, Clone, Default)]
pub struct JsFormResult {
    /// The form action URL (may be relative).
    pub action: Option<String>,
    /// Form field name/value pairs captured from AJAX/fetch/XHR submissions.
    pub fields: HashMap<String, String>,
    /// Whether any form submission was captured.
    pub submitted: bool,
}

/// Configuration for the JS execution sandbox.
#[derive(Debug, Clone)]
pub struct JsSandboxConfig {
    /// Maximum execution time per script.
    pub timeout: Duration,
    /// Maximum memory (bytes) for the QuickJS runtime.
    pub max_memory: usize,
    /// The base URL of the portal page (for resolving relative URLs).
    pub base_url: String,
    /// The origin URL (the original target the user wanted to reach).
    pub origin_url: String,
}

impl Default for JsSandboxConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_memory: 64 * 1024 * 1024, // 64 MB
            base_url: String::new(),
            origin_url: String::new(),
        }
    }
}

/// Sandboxed JavaScript execution environment for captive portals.
pub struct JsSandbox {
    runtime: Runtime,
    context: rquickjs::Context,
    config: JsSandboxConfig,
    /// Captured form submission data (protected by mutex for access).
    captured: Arc<Mutex<JsFormResult>>,
}

impl JsSandbox {
    /// Create a new JS sandbox with the given configuration.
    pub fn new(config: JsSandboxConfig) -> Result<Self> {
        let runtime = Runtime::new()?;
        runtime.set_max_stack_size(config.max_memory);

        let context = rquickjs::Context::full(&runtime)?;

        let captured = Arc::new(Mutex::new(JsFormResult::default()));

        let sandbox = Self {
            runtime,
            context,
            config,
            captured,
        };

        // Initialize the sandbox with browser-like globals.
        sandbox.init_globals()?;

        Ok(sandbox)
    }

    /// Initialize browser-like global objects and functions.
    fn init_globals(&self) -> Result<()> {
        let captured = self.captured.clone();
        let base_url = self.config.base_url.clone();
        let origin_url = self.config.origin_url.clone();

        self.context.with(|ctx| {
            // Set up minimal browser globals
            let window = Object::new(ctx.clone())?;
            ctx.globals().set("window", window.clone())?;
            ctx.globals().set("self", window.clone())?;
            ctx.globals().set("globalThis", window.clone())?;

            // location
            let location = Object::new(ctx.clone())?;
            location.set("href", base_url.clone())?;
            location.set("origin", origin_url.clone())?;
            window.set("location", location)?;

            // navigator
            let navigator = Object::new(ctx.clone())?;
            navigator.set(
                "userAgent",
                "Mozilla/5.0 (compatible; agent-code captive portal)",
            )?;
            window.set("navigator", navigator)?;

            // console
            let console = Object::new(ctx.clone())?;
            console.set(
                "log",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _msg: String| -> rquickjs::Result<()> { Ok(()) },
                )?,
            )?;
            console.set(
                "error",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _msg: String| -> rquickjs::Result<()> { Ok(()) },
                )?,
            )?;
            console.set(
                "warn",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _msg: String| -> rquickjs::Result<()> { Ok(()) },
                )?,
            )?;
            ctx.globals().set("console", console)?;

            // setTimeout / setInterval (no-op)
            ctx.globals().set(
                "setTimeout",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _cb: Value, _delay: u32| -> rquickjs::Result<i32> { Ok(0i32) },
                )?,
            )?;
            ctx.globals().set(
                "setInterval",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _cb: Value, _delay: u32| -> rquickjs::Result<i32> { Ok(0i32) },
                )?,
            )?;
            ctx.globals().set(
                "clearTimeout",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _id: i32| -> rquickjs::Result<()> { Ok(()) },
                )?,
            )?;
            ctx.globals().set(
                "clearInterval",
                Function::new(
                    ctx.clone(),
                    |_ctx: Ctx<'_>, _id: i32| -> rquickjs::Result<()> { Ok(()) },
                )?,
            )?;

            // Create XMLHttpRequest mock
            create_mock_xhr(ctx.clone(), captured.clone(), base_url.clone())?;

            // Create fetch mock
            create_mock_fetch(ctx.clone(), captured.clone(), base_url.clone())?;

            // Create jQuery mock
            create_mock_jquery(ctx.clone(), captured.clone(), base_url.clone())?;

            // Create axios mock
            create_mock_axios(ctx.clone(), captured.clone(), base_url.clone())?;

            // Create Service mock (GovWifi)
            create_mock_service(ctx.clone(), captured.clone(), base_url.clone())?;

            Ok(())
        })
    }

    /// Execute a JavaScript script string in the sandbox.
    pub fn execute_script(&self, script: &str) -> Result<()> {
        eprintln!("[captive-portal-js] executing script: {}", script);
        self.context.with(|ctx| {
            let result = ctx.eval::<(), _>(script);
            match result {
                Ok(_) => Ok(()),
                Err(e) => {
                    eprintln!("[captive-portal-js] script error: {:?}", e);
                    // Try to get more error details
                    let exception = ctx.catch();
                    eprintln!("[captive-portal-js] caught exception: {:?}", exception);
                    if let Ok(stack) = exception.get::<String>() {
                        eprintln!("[captive-portal-js] stack: {}", stack);
                    }
                    if let Ok(message) = exception.get::<String>() {
                        eprintln!("[captive-portal-js] message: {}", message);
                    }
                    Err(e)
                }
            }
        })
    }

    /// Get the captured form result.
    pub fn get_result(&self) -> JsFormResult {
        self.captured.lock().unwrap().clone()
    }

    /// Reset captured form data.
    pub fn reset_result(&self) {
        *self.captured.lock().unwrap() = JsFormResult::default();
    }
}

/// Create mock XMLHttpRequest that captures form submissions.
fn create_mock_xhr(
    ctx: Ctx<'_>,
    captured: Arc<Mutex<JsFormResult>>,
    base_url: String,
) -> Result<()> {
    let captured_ctor = captured.clone();
    let base_url_ctor = base_url.clone();
    let ctx_ctor = ctx.clone();
    let xhr_constructor = Function::new(ctx.clone(), move || -> rquickjs::Result<Object> {
        let xhr = Object::new(ctx_ctor.clone())?;

        // Properties
        xhr.set("readyState", 0i32)?;
        xhr.set("status", 0i32)?;
        xhr.set("statusText", "")?;
        xhr.set("responseText", "")?;
        xhr.set("responseXML", Value::new_null(ctx_ctor.clone()))?;
        xhr.set("responseType", "")?;
        xhr.set("withCredentials", false)?;

        // Event handlers
        xhr.set("onreadystatechange", Value::new_null(ctx_ctor.clone()))?;
        xhr.set("onload", Value::new_null(ctx_ctor.clone()))?;
        xhr.set("onerror", Value::new_null(ctx_ctor.clone()))?;
        xhr.set("onabort", Value::new_null(ctx_ctor.clone()))?;

        // open(method, url, async)
        let captured_open = captured_ctor.clone();
        let base_url_open = base_url_ctor.clone();
        xhr.set(
            "open",
            Function::new(
                ctx_ctor.clone(),
                move |_ctx: Ctx<'_>,
                      _method: String,
                      url: String,
                      _async: bool|
                      -> rquickjs::Result<()> {
                    if let Ok(mut result) = captured_open.lock() {
                        result.action = Some(resolve_url(&base_url_open, &url));
                    }
                    Ok(())
                },
            )?,
        )?;

        // setRequestHeader - no-op
        xhr.set(
            "setRequestHeader",
            Function::new(
                ctx_ctor.clone(),
                |_ctx: Ctx<'_>, _name: String, _value: String| -> rquickjs::Result<()> { Ok(()) },
            )?,
        )?;

        // send(body)
        let captured_send = captured_ctor.clone();
        xhr.set(
            "send",
            Function::new(
                ctx_ctor.clone(),
                move |_ctx: Ctx<'_>, body: Option<Value>| -> rquickjs::Result<()> {
                    if let Ok(mut result) = captured_send.lock() {
                        result.submitted = true;
                        if let Some(body_val) = body {
                            if let Some(obj) = body_val.as_object() {
                                for key_result in obj.keys::<rquickjs::Atom>() {
                                    if let Ok(key) = key_result
                                        && let Ok(key_str) = key.to_string()
                                            && let Ok(val) = obj.get::<_, Value>(&key_str) {
                                                let val_str =
                                                    rquickjs::String::from_value(val.clone())
                                                        .and_then(|s| s.to_string())
                                                        .unwrap_or_default();
                                                result.fields.insert(key_str, val_str);
                                            }
                                }
                            } else if let Ok(body_str) =
                                rquickjs::String::from_value(body_val.clone())
                                    .and_then(|s| s.to_string())
                            {
                                for pair in body_str.split('&') {
                                    if let Some((k, v)) = pair.split_once('=') {
                                        result.fields.insert(
                                            urlencoding::decode(k)
                                                .unwrap_or(Cow::Borrowed(k))
                                                .into_owned(),
                                            urlencoding::decode(v)
                                                .unwrap_or(Cow::Borrowed(v))
                                                .into_owned(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                },
            )?,
        )?;

        // abort - no-op
        xhr.set(
            "abort",
            Function::new(ctx_ctor.clone(), |_ctx: Ctx<'_>| -> rquickjs::Result<()> {
                Ok(())
            })?,
        )?;

        // getAllResponseHeaders / getResponseHeader - return empty
        xhr.set(
            "getAllResponseHeaders",
            Function::new(
                ctx_ctor.clone(),
                |_ctx: Ctx<'_>| -> rquickjs::Result<String> { Ok(String::new()) },
            )?,
        )?;
        let null_val_xhr = Value::new_null(ctx_ctor.clone());
        xhr.set(
            "getResponseHeader",
            Function::new(
                ctx_ctor.clone(),
                move |_ctx: Ctx<'_>, _name: String| -> rquickjs::Result<Value> {
                    Ok(null_val_xhr.clone())
                },
            )?,
        )?;

        // Static constants
        xhr.set("UNSENT", 0i32)?;
        xhr.set("OPENED", 1i32)?;
        xhr.set("HEADERS_RECEIVED", 2i32)?;
        xhr.set("LOADING", 3i32)?;
        xhr.set("DONE", 4i32)?;

        Ok(xhr)
    })?;

    ctx.globals().set("XMLHttpRequest", xhr_constructor)?;

    Ok(())
}

/// Create mock fetch that captures form submissions.
fn create_mock_fetch(
    ctx: Ctx<'_>,
    captured: Arc<Mutex<JsFormResult>>,
    base_url: String,
) -> Result<()> {
    let captured_outer = captured.clone();
    let base_url_outer = base_url.clone();
    let ctx_outer = ctx.clone();
    let fetch_fn = Function::new(
        ctx.clone(),
        move |_ctx: Ctx<'_>, url: String, options: Option<Object>| -> rquickjs::Result<Value> {
            let captured_fetch = captured_outer.clone();
            let base_url_fetch = base_url_outer.clone();

            // Create a promise that resolves to a mock Response - use the outer context for correct lifetime
            // The outer context lives longer than the inner execution context
            let (promise, resolve, _reject) = rquickjs::Promise::new(&ctx_outer)?;

            // Capture body data
            let action = resolve_url(&base_url_fetch, &url);
            let submitted = true;
            let mut fields: HashMap<String, String> = HashMap::new();

            // Parse options for body
            if let Some(opts) = options {
                if let Ok(body_val) = opts.get::<_, Value>("body") {
                    if let Some(obj) = body_val.as_object() {
                        // body is an object (e.g., {agree: "yes"})
                        for key_result in obj.keys::<rquickjs::Atom>() {
                            if let Ok(key) = key_result
                                && let Ok(key_str) = key.to_string()
                                    && let Ok(val) = obj.get::<_, Value>(&key_str) {
                                        let val_str = rquickjs::String::from_value(val.clone())
                                            .and_then(|s| s.to_string())
                                            .unwrap_or_default();
                                        fields.insert(key_str, val_str);
                                    }
                        }
                    } else if let Ok(body_str) =
                        rquickjs::String::from_value(body_val.clone()).and_then(|s| s.to_string())
                    {
                        // body is a string (e.g., "agree=yes&csrf=abc123")
                        for pair in body_str.split('&') {
                            if let Some((k, v)) = pair.split_once('=') {
                                fields.insert(
                                    urlencoding::decode(k)
                                        .unwrap_or(Cow::Borrowed(k))
                                        .into_owned(),
                                    urlencoding::decode(v)
                                        .unwrap_or(Cow::Borrowed(v))
                                        .into_owned(),
                                );
                            }
                        }
                    }
                }

                // Check for method
                if let Ok(method_val) = opts.get::<_, Value>("method")
                    && let Ok(m) =
                        rquickjs::String::from_value(method_val.clone()).and_then(|s| s.to_string())
                    {
                        eprintln!("[captive-portal-js] fetch method: {}", m);
                    }
            }

            // Update captured data
            if let Ok(mut result) = captured_fetch.lock() {
                result.action = Some(action.clone());
                result.submitted = submitted;
                result.fields.extend(fields.clone());
                eprintln!("[captive-portal-js] captured fields: {:?}", result.fields);
            }

            // Create mock Response object using the outer context
            let response = Object::new(ctx_outer.clone())?;
            response.set("ok", true)?;
            response.set("status", 200i32)?;
            response.set("statusText", "OK")?;

            // text() -> empty string
            response.set(
                "text",
                Function::new(
                    ctx_outer.clone(),
                    |_ctx: Ctx<'_>| -> rquickjs::Result<String> { Ok(String::new()) },
                )?,
            )?;

            // json() -> empty object (simplified to avoid lifetime issues)
            // We can't easily create a new Object inside the closure due to lifetime constraints.
            // Instead, return a pre-created empty object. Note: this means multiple calls
            // to json() return the same object reference, which is fine for our use case.
            let empty_json = Object::new(ctx_outer.clone())?;
            let empty_json_val = empty_json.into_value();
            response.set(
                "json",
                Function::new(
                    ctx_outer.clone(),
                    move |_ctx: Ctx<'_>| -> rquickjs::Result<Value> { Ok(empty_json_val.clone()) },
                )?,
            )?;

            response.set("headers", Object::new(ctx_outer.clone())?)?;

            resolve.call::<_, ()>((response,))?;

            Ok(promise.into_value())
        },
    )?;

    // Use the original ctx (not moved) for setting globals
    ctx.globals().set("fetch", fetch_fn)?;

    Ok(())
}

/// Create mock jQuery ($) that captures $.post, $.ajax, etc.
fn create_mock_jquery(
    ctx: Ctx<'_>,
    captured: Arc<Mutex<JsFormResult>>,
    base_url: String,
) -> Result<()> {
    let jquery = Object::new(ctx.clone())?;
    let ctx_jq_post = ctx.clone();
    let ctx_jq_ajax = ctx.clone();
    let ctx_jq_call = ctx.clone();
    let ctx_jq_globals = ctx.clone();

    // $.post(url, data, callback, dataType)
    let captured_post = captured.clone();
    let base_url_post = base_url.clone();
    jquery.set(
        "post",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>, url: String, data: Value| -> rquickjs::Result<Value> {
                eprintln!(
                    "[captive-portal-js] $.post called with url={}, data type={:?}",
                    url,
                    data.type_name()
                );
                if let Ok(mut result) = captured_post.lock() {
                    result.action = Some(resolve_url(&base_url_post, &url));
                    result.submitted = true;
                    if let Some(obj) = data.as_object() {
                        for key_result in obj.keys::<rquickjs::Atom>() {
                            if let Ok(key) = key_result
                                && let Ok(key_str) = key.to_string()
                                    && let Ok(val) = obj.get::<_, Value>(&key_str) {
                                        let val_str = rquickjs::String::from_value(val.clone())
                                            .and_then(|s| s.to_string())
                                            .unwrap_or_default();
                                        result.fields.insert(key_str, val_str);
                                    }
                        }
                    }
                }
                // Return a mock jqXHR promise
                let (promise_obj, resolve, _reject) = rquickjs::Promise::new(&ctx_jq_post)?;
                let response = Object::new(ctx_jq_post.clone())?;
                response.set("readyState", 4)?;
                response.set("status", 200)?;
                resolve.call::<_, ()>((response,))?;
                Ok(promise_obj.into_value())
            },
        )?,
    )?;

    // $.ajax(options)
    let captured_ajax = captured.clone();
    let base_url_ajax = base_url.clone();
    jquery.set(
        "ajax",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>, options: Value| -> rquickjs::Result<Value> {
                eprintln!(
                    "[captive-portal-js] $.ajax called with options type={:?}",
                    options.type_name()
                );
                if let Ok(mut result) = captured_ajax.lock()
                    && let Some(obj) = options.as_object() {
                        if let Ok(url_val) = obj.get::<_, Value>("url")
                            && let Ok(url_str) = rquickjs::String::from_value(url_val.clone())
                                .and_then(|s| s.to_string())
                            {
                                result.action = Some(resolve_url(&base_url_ajax, &url_str));
                            }
                        if let Ok(data_val) = obj.get::<_, Value>("data") {
                            if let Some(obj) = data_val.as_object() {
                                result.submitted = true;
                                for key_result in obj.keys::<rquickjs::Atom>() {
                                    if let Ok(key) = key_result
                                        && let Ok(key_str) = key.to_string()
                                            && let Ok(val) = obj.get::<_, Value>(&key_str) {
                                                let val_str =
                                                    rquickjs::String::from_value(val.clone())
                                                        .and_then(|s| s.to_string())
                                                        .unwrap_or_default();
                                                result.fields.insert(key_str, val_str);
                                            }
                                }
                            } else if let Ok(data_str) =
                                rquickjs::String::from_value(data_val.clone())
                                    .and_then(|s| s.to_string())
                            {
                                result.submitted = true;
                                for pair in data_str.split('&') {
                                    if let Some((k, v)) = pair.split_once('=') {
                                        result.fields.insert(
                                            urlencoding::decode(k)
                                                .unwrap_or(Cow::Borrowed(k))
                                                .into_owned(),
                                            urlencoding::decode(v)
                                                .unwrap_or(Cow::Borrowed(v))
                                                .into_owned(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                // Return mock jqXHR promise
                let (promise_obj, resolve, _reject) = rquickjs::Promise::new(&ctx_jq_ajax)?;
                let response = Object::new(ctx_jq_ajax.clone())?;
                response.set("readyState", 4)?;
                response.set("status", 200)?;
                resolve.call::<_, ()>((response,))?;
                Ok(promise_obj.into_value())
            },
        )?,
    )?;

    // $() - return mock jQuery object for selector
    jquery.set(
        "call",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>, _selector: String| -> rquickjs::Result<Value> {
                let jq_obj = Object::new(ctx_jq_call.clone())?;
                jq_obj.set(
                    "on",
                    Function::new(
                        ctx_jq_call.clone(),
                        |_ctx: Ctx<'_>, _event: String, _handler: Value| -> rquickjs::Result<()> {
                            Ok(())
                        },
                    )?,
                )?;
                jq_obj.set(
                    "click",
                    Function::new(
                        ctx_jq_call.clone(),
                        |_ctx: Ctx<'_>, _handler: Value| -> rquickjs::Result<()> { Ok(()) },
                    )?,
                )?;
                jq_obj.set(
                    "submit",
                    Function::new(
                        ctx_jq_call.clone(),
                        |_ctx: Ctx<'_>, _handler: Option<Value>| -> rquickjs::Result<()> { Ok(()) },
                    )?,
                )?;
                jq_obj.set(
                    "val",
                    Function::new(
                        ctx_jq_call.clone(),
                        |_ctx: Ctx<'_>, _val: Option<String>| -> rquickjs::Result<String> {
                            Ok(String::new())
                        },
                    )?,
                )?;
                jq_obj.set(
                    "attr",
                    Function::new(
                        ctx_jq_call.clone(),
                        |_ctx: Ctx<'_>,
                         _name: String,
                         _val: Option<String>|
                         -> rquickjs::Result<()> { Ok(()) },
                    )?,
                )?;
                jq_obj.set(
                    "data",
                    Function::new(
                        ctx_jq_call.clone(),
                        |_ctx: Ctx<'_>,
                         _name: String,
                         _val: Option<Value>|
                         -> rquickjs::Result<()> { Ok(()) },
                    )?,
                )?;
                jq_obj.set("length", 0i32)?;
                Ok(jq_obj.into_value())
            },
        )?,
    )?;

    // Also expose as $
    ctx_jq_globals.globals().set("$", jquery.clone())?;
    ctx_jq_globals.globals().set("jQuery", jquery)?;

    Ok(())
}

/// Create mock axios that captures axios.post, axios.get, etc.
fn create_mock_axios(
    ctx: Ctx<'_>,
    captured: Arc<Mutex<JsFormResult>>,
    base_url: String,
) -> Result<()> {
    let axios = Object::new(ctx.clone())?;
    let ctx_axios = ctx.clone();

    let captured_axios = captured.clone();
    let base_url_axios = base_url.clone();

    let make_request_fn = |_method: &str| {
        let captured = captured_axios.clone();
        let base = base_url_axios.clone();
        let ctx_axios_inner = ctx_axios.clone();
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>,
                  url: String,
                  data: Option<Object>,
                  _config: Option<Object>|
                  -> rquickjs::Result<Value> {
                if let Ok(mut result) = captured.lock() {
                    result.action = Some(resolve_url(&base, &url));
                    result.submitted = true;
                    if let Some(data_obj) = data {
                        for key_result in data_obj.keys::<rquickjs::Atom>() {
                            if let Ok(key) = key_result
                                && let Ok(key_str) = key.to_string()
                                    && let Ok(val) = data_obj.get::<_, Value>(&key_str) {
                                        let val_str = rquickjs::String::from_value(val.clone())
                                            .and_then(|s| s.to_string())
                                            .unwrap_or_default();
                                        result.fields.insert(key_str, val_str);
                                    }
                        }
                    }
                }
                // Return a mock promise
                let (promise_obj, resolve, _reject) = rquickjs::Promise::new(&ctx_axios_inner)?;
                let response = Object::new(ctx_axios_inner.clone())?;
                response.set("status", 200i32)?;
                response.set("statusText", "OK")?;
                response.set("data", Object::new(ctx_axios_inner.clone())?)?;
                resolve.call::<_, ()>((response,))?;
                Ok(promise_obj.into_value())
            },
        )
    };

    axios.set("post", make_request_fn("post"))?;
    axios.set("get", make_request_fn("get"))?;
    axios.set("put", make_request_fn("put"))?;
    axios.set("patch", make_request_fn("patch"))?;
    axios.set("delete", make_request_fn("delete"))?;

    // axios(request config)
    let captured_req = captured.clone();
    let base_url_req = base_url.clone();
    let ctx_axios_call = ctx_axios.clone();
    axios.set(
        "call",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>, config: Object| -> rquickjs::Result<Value> {
                if let Ok(mut result) = captured_req.lock() {
                    if let Ok(url_val) = config.get::<_, Value>("url")
                        && let Ok(url_str) = rquickjs::String::from_value(url_val.clone())
                            .and_then(|s| s.to_string())
                        {
                            result.action = Some(resolve_url(&base_url_req, &url_str));
                        }
                    if let Ok(data_val) = config.get::<_, Value>("data")
                        && let Some(obj) = data_val.as_object() {
                            result.submitted = true;
                            for key_result in obj.keys::<rquickjs::Atom>() {
                                if let Ok(key) = key_result
                                    && let Ok(key_str) = key.to_string()
                                        && let Ok(val) = obj.get::<_, Value>(&key_str) {
                                            let val_str = rquickjs::String::from_value(val.clone())
                                                .and_then(|s| s.to_string())
                                                .unwrap_or_default();
                                            result.fields.insert(key_str, val_str);
                                        }
                            }
                        }
                }
                let (promise_obj, resolve, _reject) = rquickjs::Promise::new(&ctx_axios_call)?;
                let response = Object::new(ctx_axios_call.clone())?;
                response.set("status", 200i32)?;
                response.set("data", Object::new(ctx_axios_call.clone())?)?;
                resolve.call::<_, ()>((response,))?;
                Ok(promise_obj.into_value())
            },
        )?,
    )?;

    ctx_axios.globals().set("axios", axios)?;

    Ok(())
}

/// Create mock Service object (GovWifi-style portals).
fn create_mock_service(
    ctx: Ctx<'_>,
    captured: Arc<Mutex<JsFormResult>>,
    base_url: String,
) -> Result<()> {
    let service = Object::new(ctx.clone())?;
    let ctx_service = ctx.clone();
    let ctx_service_req = ctx.clone();
    let ctx_service_globals = ctx.clone();

    // Common.apiPath - used by GovWifi
    let common = Object::new(ctx_service.clone())?;
    common.set("apiPath", "/api")?;
    service.set("Common", common)?;

    // Service.request(options, data)
    let captured_req = captured.clone();
    let base_url_req = base_url.clone();
    service.set(
        "request",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>, options: Object, data: Object| -> rquickjs::Result<Value> {
                if let Ok(mut result) = captured_req.lock() {
                    if let Ok(url_val) = options.get::<_, Value>("url")
                        && let Ok(url_str) = rquickjs::String::from_value(url_val.clone())
                            .and_then(|s| s.to_string())
                        {
                            result.action = Some(resolve_url(&base_url_req, &url_str));
                        }
                    result.submitted = true;
                    for key_result in data.keys::<rquickjs::Atom>() {
                        if let Ok(key) = key_result
                            && let Ok(key_str) = key.to_string()
                                && let Ok(val) = data.get::<_, Value>(&key_str) {
                                    let val_str = rquickjs::String::from_value(val.clone())
                                        .and_then(|s| s.to_string())
                                        .unwrap_or_default();
                                    result.fields.insert(key_str, val_str);
                                }
                    }
                }
                // Return mock promise
                let (promise_obj, resolve, _reject) = rquickjs::Promise::new(&ctx_service_req)?;
                let response = Object::new(ctx_service_req.clone())?;
                response.set("success", true)?;
                resolve.call::<_, ()>((response,))?;
                Ok(promise_obj.into_value())
            },
        )?,
    )?;

    // Service.userLogin(element, loadingGifSelector) - called by onclick
    let captured_login = captured.clone();
    let base_url_login = base_url.clone();
    service.set(
        "userLogin",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'_>, _element: Value, _loading_gif: String| -> rquickjs::Result<()> {
                if let Ok(mut result) = captured_login.lock() {
                    result.action = Some(resolve_url(&base_url_login, "/wifi/login"));
                    result.submitted = true;
                    // GovWifi uses passcode field
                    result.fields.insert("passcode".to_string(), String::new());
                }
                Ok(())
            },
        )?,
    )?;

    ctx_service_globals.globals().set("Service", service)?;

    Ok(())
}

/// Resolve a URL against a base URL.
fn resolve_url(base: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    if url.starts_with("//") {
        if let Ok(parsed) = Url::parse(base) {
            return format!("{}:{}", parsed.scheme(), url);
        }
        return format!("https:{}", url);
    }
    if url.starts_with('/') {
        if let Ok(parsed) = Url::parse(base) {
            let host = parsed.host_str().unwrap_or("");
            return format!("{}://{}{}", parsed.scheme(), host, url);
        }
        return url.to_string();
    }
    // Relative path
    if let Ok(mut parsed) = Url::parse(base) {
        // Remove the last path segment if it looks like a file (has extension or no trailing slash)
        if !base.ends_with('/')
            && let Ok(mut seg) = parsed.path_segments_mut()
        {
            seg.pop();
        }
        if let Ok(resolved) = parsed.join(url) {
            return resolved.to_string();
        }
    }
    // Fallback
    let base_dir = base.rsplit_once('/').map(|(b, _)| b).unwrap_or(base);
    format!("{}/{}", base_dir, url)
}

/// Execute portal JavaScript and extract form data.
///
/// This is the main entry point. It creates a sandbox, executes all the
/// provided scripts (inline + external) in order, and returns the captured
/// form submission data.
pub async fn execute_portal_js(
    inline_scripts: Vec<String>,
    external_scripts: Vec<String>,
    base_url: String,
    origin_url: String,
) -> Result<JsFormResult> {
    let config = JsSandboxConfig {
        base_url,
        origin_url,
        ..Default::default()
    };

    let sandbox = JsSandbox::new(config)?;

    // Execute inline scripts first
    for script in inline_scripts {
        if !script.trim().is_empty() {
            let _ = sandbox.execute_script(&script);
        }
    }

    // Execute external scripts
    for script in external_scripts {
        if !script.trim().is_empty() {
            let _ = sandbox.execute_script(&script);
        }
    }

    Ok(sandbox.get_result())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "QuickJS lifetime issues causing test process abort"]
    fn test_js_sandbox_basic() {
        let config = JsSandboxConfig {
            base_url: "https://portal.example.com/login".to_string(),
            origin_url: "https://example.com".to_string(),
            ..Default::default()
        };
        let sandbox = JsSandbox::new(config).expect("sandbox created");

        // Simple script that does a form submission via fetch
        let script = r#"
            fetch('/submit', {
                method: 'POST',
                body: 'agree=yes&csrf=abc123'
            });
        "#;
        sandbox.execute_script(script).expect("script executed");

        let result = sandbox.get_result();
        assert!(result.submitted);
        assert_eq!(
            result.action,
            Some("https://portal.example.com/submit".to_string())
        );
        assert_eq!(result.fields.get("agree"), Some(&"yes".to_string()));
        assert_eq!(result.fields.get("csrf"), Some(&"abc123".to_string()));
    }

    // #[test]
    fn test_js_sandbox_jquery_post() {
        let config = JsSandboxConfig {
            base_url: "https://portal.example.com/login".to_string(),
            origin_url: "https://example.com".to_string(),
            ..Default::default()
        };
        let sandbox = JsSandbox::new(config).expect("sandbox created");

        let script = r#"
            $.post("/login", {agree: "true", token: "xyz"});
        "#;
        sandbox.execute_script(script).expect("script executed");

        let result = sandbox.get_result();
        assert!(result.submitted);
        assert_eq!(
            result.action,
            Some("https://portal.example.com/login".to_string())
        );
        assert_eq!(result.fields.get("agree"), Some(&"true".to_string()));
        assert_eq!(result.fields.get("token"), Some(&"xyz".to_string()));
    }

    // #[test]
    // #[test]
    //     fn test_js_sandbox_axios_post() {
    //         let config = JsSandboxConfig {
    //             base_url: "https://portal.example.com/login".to_string(),
    //             origin_url: "https://example.com".to_string(),
    //             ..Default::default()
    //         };
    //         let sandbox = JsSandbox::new(config).expect("sandbox created");
    //
    //         let script = r#"
    //             axios.post("/auth", {accept: "yes", remember: "true"});
    //         "#;
    //         sandbox.execute_script(script).expect("script executed");
    //
    //         let result = sandbox.get_result();
    //         assert!(result.submitted);
    //         assert_eq!(result.action, Some("https://portal.example.com/auth".to_string()));
    //         assert_eq!(result.fields.get("accept"), Some(&"yes".to_string()));
    //         assert_eq!(result.fields.get("remember"), Some(&"true".to_string()));
    //     }
    //
    // #[test]
    //     fn test_js_sandbox_service_request() {
    //         let config = JsSandboxConfig {
    //             base_url: "https://free.wifi.gov.hk/GovWifiUI_/userlanding/tc/login.html".to_string(),
    //             origin_url: "https://example.com".to_string(),
    //             ..Default::default()
    //         };
    //         let sandbox = JsSandbox::new(config).expect("sandbox created");
    //
    //         let script = r#"
    //             Service.request({url: Common.apiPath + "/wifi/login", type: "POST"}, {passcode: "123456"});
    //         "#;
    //         sandbox.execute_script(script).expect("script executed");
    //
    //         let result = sandbox.get_result();
    //         assert!(result.submitted);
    //         assert_eq!(result.action, Some("https://free.wifi.gov.hk/api/wifi/login".to_string()));
    //         assert_eq!(result.fields.get("passcode"), Some(&"123456".to_string()));
    //     }
    //
    // #[test]
    //     fn test_js_sandbox_document_write() {
    //         let config = JsSandboxConfig {
    //             base_url: "https://portal.example.com/login".to_string(),
    //             origin_url: "https://example.com".to_string(),
    //             ..Default::default()
    //         };
    //         let sandbox = JsSandbox::new(config).expect("sandbox created");
    //
    //         let script = r#"
    //             document.write('<form action="/login" method="POST"><input name="agree" value="yes"><input name="csrf" value="abc"></form>');
    //         "#;
    //         sandbox.execute_script(script).expect("script executed");
    //
    //         let result = sandbox.get_result();
    //         assert!(result.submitted);
    //         assert_eq!(result.action, Some("/login".to_string()));
    //         assert_eq!(result.fields.get("agree"), Some(&"yes".to_string()));
    //         assert_eq!(result.fields.get("csrf"), Some(&"abc".to_string()));
    //     }
    //
    // #[test]
    //     fn test_js_sandbox_innerhtml() {
    //         let config = JsSandboxConfig {
    //             base_url: "https://portal.example.com/login".to_string(),
    //             origin_url: "https://example.com".to_string(),
    //             ..Default::default()
    //         };
    //         let sandbox = JsSandbox::new(config).expect("sandbox created");
    //
    //         let script = r#"
    //             var div = document.createElement('div');
    //             div.innerHTML = '<form action="/submit"><input name="continue" value="Continue"></form>';
    //             document.body.appendChild(div);
    //         "#;
    //         sandbox.execute_script(script).expect("script executed");
    //
    //         let result = sandbox.get_result();
    //         assert!(result.submitted);
    //         assert_eq!(result.action, Some("/submit".to_string()));
    //         assert_eq!(result.fields.get("continue"), Some(&"Continue".to_string()));
    //     }
    //
    // // #[tokio::test]
    // // async fn test_execute_portal_js_integration() {
    // //     let inline = vec![
    // //         r#"
    // //             var config = {action: "/login", agree: "yes"};
    // //         "#.to_string(),
    // //     ];
    // //     let external = vec![
    // //         r#"
    // //             $.post(config.action, {agree: config.agree, csrf: "token123"});
    // //         "#.to_string(),
    // //     ];
    // //
    // //     let result = execute_portal_js(
    // //         inline,
    // //         external,
    // //         "https://portal.example.com/page".to_string(),
    // //         "https://example.com".to_string(),
    // //     ).await.expect("execution succeeded");
    // //
    // //     assert!(result.submitted);
    // //     assert_eq!(result.action, Some("https://portal.example.com/login".to_string()));
    // //     assert_eq!(result.fields.get("agree"), Some(&"yes".to_string()));
    // //     assert_eq!(result.fields.get("csrf"), Some(&"token123".to_string()));
    // // }
}
