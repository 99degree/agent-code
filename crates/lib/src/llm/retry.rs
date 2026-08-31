//! Retry logic and streaming fallback handling.
//!
//! When streaming fails mid-response, the retry handler can:
//! - Discard partial tool executions with synthetic error blocks
//! - Fall back to a smaller model on repeated overload errors
//! - Apply exponential backoff with jitter

use std::collections::HashMap;
use std::time::Duration;

use crate::llm::provider::ProviderKind;

/// Retry configuration.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum retry attempts for transient errors.
    pub max_retries: u32,
    /// Initial backoff duration.
    pub initial_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
    /// Backoff multiplier (exponential).
    pub multiplier: f64,
    /// Fixed per-attempt backoff schedule applied to overload (529/503) and
    /// stream-interrupt retries. Indexed 1-based by attempt; once the list is
    /// exhausted the final entry is reused (subject to `max_backoff`). This
    /// gives a predictable staircase — the first retry is quick, later retries
    /// back off much further — rather than pure exponential growth. Defaults to
    /// 1s, 5s, 15s, then 35s.
    pub backoff_schedule: Vec<Duration>,
    /// Maximum 529/503 (overloaded) retries before falling back.
    pub max_overload_retries: u32,
    /// Maximum retry-after duration we'll accept from the API (milliseconds).
    /// If the API specifies a longer wait, we abort instead of retrying.
    /// Set to 0 to use the API's value regardless of duration.
    pub max_retry_after_ms: u64,
    /// Backoff applied to transport-level network failures (DNS, TLS,
    /// connection reset, request never left the box). These have no status
    /// code to map, so unlike rate-limit/overload they can't be tuned off a
    /// Retry-After header — a longer, seconds-scale wait gives transient
    /// instability time to clear instead of looping on a 1s backoff.
    pub network_backoff: Duration,
    /// Maximum retry attempts for transport-level network failures. These are
    /// unbounded by a Retry-After header and often reflect brief provider
    /// instability, so they get a larger budget than `max_retries` (which
    /// also covers stream/parse errors that tend to repeat on a bad payload).
    pub max_network_retries: u32,
    /// Long-wait threshold (milliseconds). A 429 whose retry-after meets or
    /// exceeds this is treated as a long backoff the API wants us to honor by
    /// *stopping* — we abort rather than block for that long. Set to 0 to
    /// disable the threshold (subject only to `max_retry_after_ms`).
    pub long_wait_after_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1000),
            max_backoff: Duration::from_secs(60),
            multiplier: 2.0,
            max_overload_retries: 4,
            // Predictable staircase: first retry quick, later retries back off
            // far further (1s → 5s → 15s → 35s) so a transient overload is
            // given real breathing room before we fall back to the small model.
            backoff_schedule: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(15),
                Duration::from_secs(35),
            ],
            max_retry_after_ms: 10_000, // 10 seconds
            // An hour: long enough that a 429 requesting this much wait is a
            // "stop and come back later" signal, not a retriable delay.
            long_wait_after_ms: 3_600_000,
            network_backoff: Duration::from_secs(5),
            max_network_retries: 5,
        }
    }
}

/// State tracker for retry logic across multiple attempts.
#[derive(Debug, Default)]
pub struct RetryState {
    /// Number of consecutive failures.
    pub consecutive_failures: u32,
    /// Number of 429 (rate limit) retries.
    pub rate_limit_retries: u32,
    /// Number of 529/503 (overload) retries.
    pub overload_retries: u32,
    /// Whether we've fallen back to the smaller model.
    pub using_fallback: bool,
    /// Whether we've already failed over from one provider to another
    /// (e.g. OpenCode → Kilo) during this turn. Prevents ping-ponging
    /// between the two providers on repeated failures.
    pub using_failover: bool,
}

impl RetryState {
    /// Determine the next action after a failure.
    ///
    /// `current_provider` is the provider the failing request was issued
    /// to; if `error` is a model-availability failure with a matching
    /// cross-provider failover rule ([`FAILOVER_RULES`]), this returns
    /// [`RetryAction::Failover`] — but only when `using_failover` is false,
    /// so a request fails over at most once per turn (no provider
    /// ping-pong).
    pub fn next_action(
        &mut self,
        error: &RetryableError,
        config: &RetryConfig,
        current_provider: ProviderKind,
        using_failover: bool,
        current_model: &str,
        failover_mapping: &HashMap<String, (String, String)>,
    ) -> RetryAction {
        self.consecutive_failures += 1;

        match error {
            RetryableError::RateLimited { retry_after } => {
                self.rate_limit_retries += 1;
                if self.rate_limit_retries > config.max_retries {
                    return RetryAction::Abort("Rate limit retries exhausted".into());
                }
                // A long retry-after is a "stop and come back later" signal,
                // not a wait worth blocking for — abort instead of stalling.
                if config.long_wait_after_ms > 0 && *retry_after >= config.long_wait_after_ms {
                    return RetryAction::Abort(format!(
                        "Rate limit retry-after {}ms exceeds long-wait threshold {}ms \
                         — stopping instead of blocking",
                        retry_after, config.long_wait_after_ms
                    ));
                }
                // If API specifies a retry-after longer than our threshold, abort.
                // 0 means no limit (use API's value regardless).
                if config.max_retry_after_ms > 0 && *retry_after > config.max_retry_after_ms {
                    return RetryAction::Abort(format!(
                        "Rate limit retry-after {}ms exceeds max {}ms",
                        retry_after, config.max_retry_after_ms
                    ));
                }
                RetryAction::Retry {
                    after: Duration::from_millis(*retry_after),
                }
            }
            RetryableError::Overloaded => {
                self.overload_retries += 1;
                if self.overload_retries > config.max_overload_retries {
                    if !self.using_fallback {
                        self.using_fallback = true;
                        self.overload_retries = 0;
                        return RetryAction::FallbackModel;
                    }
                    return RetryAction::Abort("Overload retries exhausted on fallback".into());
                }
                let backoff = schedule_backoff(
                    self.overload_retries,
                    &config.backoff_schedule,
                    config.max_backoff,
                );
                RetryAction::Retry { after: backoff }
            }
            RetryableError::StreamInterrupted => {
                if self.consecutive_failures > config.max_retries {
                    return RetryAction::Abort("Stream retry limit reached".into());
                }
                let backoff = schedule_backoff(
                    self.consecutive_failures,
                    &config.backoff_schedule,
                    config.max_backoff,
                );
                RetryAction::Retry { after: backoff }
            }
            RetryableError::Network => {
                if self.consecutive_failures > config.max_network_retries {
                    return RetryAction::Abort("Network error retry limit reached".into());
                }
                // Transport failures carry no status to derive a wait from, so
                // use the dedicated longer backoff (seconds-scale) rather than
                // the short initial_backoff — this rides out transient blips
                // instead of burning all retries on a 1s cadence.
                let backoff = calculate_backoff(
                    self.consecutive_failures,
                    config.network_backoff,
                    config.max_backoff,
                    config.multiplier,
                );
                RetryAction::Retry { after: backoff }
            }
            RetryableError::NonRetryable(msg) => RetryAction::Abort(msg.clone()),
            // ModelUnavailable: try a cross-provider failover first; if none
            // applies (no rule, wrong provider, or already failed over), the
            // request is unrecoverable and we abort.
            RetryableError::ModelUnavailable { .. } => {
                if let Some(target) = crate::llm::retry::failover_target_configured(
                    error,
                    current_provider,
                    current_model,
                    using_failover,
                    failover_mapping,
                ) {
                    RetryAction::Failover { target }
                } else {
                    RetryAction::Abort("model unavailable".into())
                }
            }
        }
    }

    /// Reset counters after a successful call.
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.rate_limit_retries = 0;
        self.using_failover = false;
        // Don't reset overload_retries or using_fallback — those persist
        // across turns (once we've dropped to the small model, stay there).
    }
}

/// Categorized error for retry logic.
pub enum RetryableError {
    RateLimited {
        retry_after: u64,
    },
    Overloaded,
    StreamInterrupted,
    Network,
    NonRetryable(String),
    /// The provider rejected the request because the model is unavailable
    /// or rate-limited *for that model*, and we have a cross-provider
    /// failover rule. Carries the model name so the caller can look up
    /// the target provider.
    ModelUnavailable {
        model: String,
    },
}

/// Action the caller should take after a failure.
#[derive(Debug)]
pub enum RetryAction {
    /// Wait and retry with the same model.
    Retry { after: Duration },
    /// Switch to the fallback model and retry.
    FallbackModel,
    /// Give up — unrecoverable.
    Abort(String),
    /// Fail over to a different provider/model via a static rule (e.g.
    /// OpenCode `laguna-s` → Kilo `kilo-alpha`). The caller must rebuild
    /// the provider from the supplied target.
    Failover { target: FailoverTarget },
}

/// Static failover rules: when a provider errors out on a model, retry
/// the same request against another provider (its free/open mirror)
/// instead of the smaller-model fallback.
///
/// Each rule fires when the *current* provider matches `from` and the
/// failing model name contains `model_fragment`. The concrete Kilo mirror
/// model id is resolved at runtime via a /models lookup (the exact names
/// like `laguna-s-2.1:free` / `stealth/ox-alpha` change over time, so we
/// do not hard-code them), and the request is repointed to that mirror.
/// A request that has already failed over is never re-failed-over
/// (`using_failover` guards against a ping-pong loop between providers).
///
/// Current rules (OpenCode Zen "free"/preview tiers → Kilo's mirror):
/// - OpenCode `laguna-s` (e.g. `laguna-s-2.1:free`) → Kilo `laguna-s-2.1:free`
/// - OpenCode `x-preview-f-free` → Kilo `stealth/ox-alpha`
///
/// The `search_fragment` differs from the trigger because Kilo's mirror
/// model id does not share the OpenCode name (e.g. `x-preview-f-free`
/// mirrors to `stealth/ox-alpha`). The caller resolves the exact id from
/// Kilo's live `/models` list using `search_fragment`.
const FAILOVER_RULES: &[FailoverRule] = &[
    FailoverRule {
        from: ProviderKind::OpenCode,
        to: ProviderKind::Kilo,
        trigger_fragment: "laguna-s",
        search_fragment: "laguna-s",
    },
    FailoverRule {
        from: ProviderKind::OpenCode,
        to: ProviderKind::Kilo,
        trigger_fragment: "x-preview-f-free",
        search_fragment: "ox-alpha",
    },
];

/// One entry in the cross-provider failover table.
pub struct FailoverRule {
    /// Provider the failing request was issued to.
    pub from: ProviderKind,
    /// Provider to retry against.
    pub to: ProviderKind,
    /// Substring of the *failing OpenCode model* name that triggers this
    /// rule (matched case-insensitively).
    pub trigger_fragment: &'static str,
    /// Substring to match against the failover provider's live `/models`
    /// list to find the concrete mirror model id.
    pub search_fragment: &'static str,
}

/// A resolved failover target: the provider and model *hint* the next
/// attempt should use when `RetryAction::Failover` is returned. The
/// `model_hint` is the failing model's fragment (e.g. `laguna-s`); the
/// caller resolves it to the failover provider's actual mirror model id
/// via a live /models lookup before issuing the request, so we never
/// hard-code a name that may have changed.
#[derive(Debug, Clone)]
pub struct FailoverTarget {
    /// Provider kind to issue the repointed request to.
    pub provider: ProviderKind,
    /// Fragment of the failing model name that triggered the rule; the
    /// caller turns this into the failover provider's real mirror id.
    pub model_hint: String,
}

impl RetryAction {
    /// Whether this action represents a provider-level failover (the
    /// caller must rebuild the provider from a static rule), distinct
    /// from a model-only `FallbackModel`.
    pub fn is_failover(&self) -> bool {
        matches!(self, RetryAction::Failover { .. })
    }
}

impl RetryableError {
    /// Map a raw provider error into a retryable category, surfacing
    /// model-availability failures (OpenAI-compatible 404/400 on an
    /// unknown or bad-request model) as [`ModelUnavailable`] so the
    /// static failover table can repoint the request at another provider.
    pub fn classify(e: &crate::llm::provider::ProviderError) -> Self {
        match e {
            crate::llm::provider::ProviderError::RateLimited { retry_after_ms } => {
                RetryableError::RateLimited {
                    retry_after: *retry_after_ms,
                }
            }
            crate::llm::provider::ProviderError::Overloaded => RetryableError::Overloaded,
            crate::llm::provider::ProviderError::Network(_) => RetryableError::StreamInterrupted,
            crate::llm::provider::ProviderError::InvalidResponse(msg)
            | crate::llm::provider::ProviderError::RequestTooLarge(msg) => {
                // OpenAI-compatible servers emit 404 on an unknown model and
                // 400 on a bad-request model, both surfaced here. Treat either
                // as "model unavailable" so failover can kick in.
                RetryableError::ModelUnavailable { model: msg.clone() }
            }
            crate::llm::provider::ProviderError::Auth(msg) => {
                RetryableError::NonRetryable(msg.clone())
            }
        }
    }

    /// Resolve a cross-provider failover for this error, honoring the
    /// static [`FAILOVER_RULES`] table. Returns `None` if the error is
    /// not a model-availability failure, the current provider has no
    /// rule, the failing model name matches no rule, or the caller has
    /// already failed over (to prevent a ping-pong loop between two
    /// providers on repeated failures).
    pub fn failover_target(
        &self,
        current_provider: ProviderKind,
        using_failover: bool,
    ) -> Option<FailoverTarget> {
        let RetryableError::ModelUnavailable { model } = self else {
            return None;
        };
        // Only one failover per turn — prevents provider ping-pong.
        if using_failover {
            return None;
        }
        let needle = model.to_lowercase();
        for rule in FAILOVER_RULES {
            if rule.from == current_provider && needle.contains(rule.trigger_fragment) {
                return Some(FailoverTarget {
                    provider: rule.to,
                    model_hint: rule.search_fragment.to_string(),
                });
            }
        }
        None
    }
}

/// Resolve a cross-provider failover for this error, honoring the user's
/// `failover_mapping` config first and falling back to the static
/// [`FAILOVER_RULES`] table. Returns `None` if no applicable, credentialed
/// target exists.
///
/// The mapping supports three key shapes:
/// - `"provider"` — matches any model on that provider (e.g. `"openrouter"`)
/// - `"provider/*"` — same as above, explicit wildcard
/// - `"provider/model"` — matches a specific model fragment (e.g.
///   `"openrouter/anthropic/claude-3.5-sonnet"`)
///
/// A target is only used when its provider key is actually configured
/// (`is_configured()`), so we never rotate into a provider the user has no
/// credentials for.
pub fn failover_target_configured(
    error: &RetryableError,
    current_provider: ProviderKind,
    current_model: &str,
    using_failover: bool,
    mapping: &HashMap<String, (String, String)>,
) -> Option<FailoverTarget> {
    if using_failover {
        return None;
    }
    let RetryableError::ModelUnavailable { .. } = error else {
        return None;
    };
    let pname = current_provider.as_name();

    for (key, (to, hint)) in mapping {
        let matches = if let Some(stripped) = key.strip_suffix("/*") {
            stripped == pname
        } else if let Some((kp, km)) = key.split_once('/') {
            kp == pname && current_model.to_lowercase().contains(&km.to_lowercase())
        } else {
            key == pname
        };
        if !matches {
            continue;
        }
        if let Some(to_kind) = ProviderKind::from_name(to)
            && to_kind.is_configured()
        {
            return Some(FailoverTarget {
                provider: to_kind,
                model_hint: hint.clone(),
            });
        }
    }

    // No configured/valid target — fall back to the built-in rules
    // (e.g. OpenCode → Kilo mirrors), which also respect `using_failover`.
    error.failover_target(current_provider, using_failover)
}

/// Calculate exponential backoff with jitter.
fn calculate_backoff(attempt: u32, initial: Duration, max: Duration, multiplier: f64) -> Duration {
    let base = initial.as_millis() as f64 * multiplier.powi(attempt as i32 - 1);
    let capped = base.min(max.as_millis() as f64);
    // Add 10% jitter.
    let jitter = capped * 0.1 * rand_f64();
    Duration::from_millis((capped + jitter) as u64)
}

/// Resolve a backoff duration from a fixed per-attempt schedule.
///
/// `attempt` is 1-based. The schedule is indexed 0-based, so attempt N maps to
/// `schedule[N-1]`; once the attempt count runs past the end of the schedule
/// the final entry is reused (subject to `max`). A small amount of jitter is
/// added so concurrent retries don't all wake at the same instant. The schedule
/// is intentionally a flat staircase (e.g. 1s/5s/15s/35s) rather than pure
/// exponential growth, so the first retry is quick but later retries back off
/// much further — giving a transient overload real breathing room.
fn schedule_backoff(attempt: u32, schedule: &[Duration], max: Duration) -> Duration {
    let idx = (attempt.saturating_sub(1) as usize).min(schedule.len().saturating_sub(1));
    let base = schedule.get(idx).copied().unwrap_or_default().as_millis() as f64;
    let capped = base.min(max.as_millis() as f64);
    // Add 10% jitter.
    let jitter = capped * 0.1 * rand_f64();
    Duration::from_millis((capped + jitter) as u64)
}

/// Simple pseudo-random f64 in [0, 1) using timestamp.
fn rand_f64() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = RetryConfig::default();
        assert_eq!(c.max_retries, 3);
        assert!(c.multiplier > 1.0);
    }

    #[test]
    fn test_rate_limit_aborts_on_long_wait() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            long_wait_after_ms: 3_600_000, // 1h
            ..Default::default()
        };
        // A 429 asking us to wait an hour is a "stop" signal.
        match state.next_action(
            &RetryableError::RateLimited {
                retry_after: 3_600_000,
            },
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(msg) => assert!(msg.contains("long-wait"), "{msg}"),
            other => panic!("Expected Abort on long wait, got {other:?}"),
        }
    }

    #[test]
    fn test_rate_limit_retries_short_wait() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            long_wait_after_ms: 3_600_000,
            ..Default::default()
        };
        match state.next_action(
            &RetryableError::RateLimited { retry_after: 500 },
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { after } => assert!(after.as_millis() >= 500),
            other => panic!("Expected Retry on short wait, got {other:?}"),
        }
    }

    #[test]
    fn test_retry_on_rate_limit() {
        let mut state = RetryState::default();
        let config = RetryConfig::default();
        let err = RetryableError::RateLimited { retry_after: 500 };
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { after } => assert!(after.as_millis() >= 500),
            other => panic!("Expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn test_retry_exhaustion() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            max_retries: 1,
            ..Default::default()
        };
        let err = RetryableError::RateLimited { retry_after: 100 };
        let _ = state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ); // First retry.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(_) => {}
            other => panic!("Expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn test_non_retryable_aborts() {
        let mut state = RetryState::default();
        let config = RetryConfig::default();
        let err = RetryableError::NonRetryable("bad request".into());
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(msg) => assert!(msg.contains("bad request")),
            other => panic!("Expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn test_overload_escalates_to_fallback() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            max_overload_retries: 2,
            ..Default::default()
        };
        let err = RetryableError::Overloaded;
        let _ = state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        );
        let _ = state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        );
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::FallbackModel => {}
            other => panic!("Expected FallbackModel, got {other:?}"),
        }
    }

    #[test]
    fn test_reset_preserves_fallback() {
        let mut state = RetryState {
            using_fallback: true,
            consecutive_failures: 5,
            ..Default::default()
        };
        state.reset();
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.using_fallback); // Preserved.
    }

    #[test]
    fn test_backoff_increases_with_attempt() {
        let initial = Duration::from_millis(1000);
        let max = Duration::from_secs(60);
        let multiplier = 2.0;

        let _b1 = calculate_backoff(1, initial, max, multiplier);
        let b2 = calculate_backoff(2, initial, max, multiplier);
        let b3 = calculate_backoff(3, initial, max, multiplier);

        // Each attempt should generally produce a larger backoff (before jitter caps).
        // With multiplier 2.0: attempt 1 ~1s, attempt 2 ~2s, attempt 3 ~4s.
        assert!(b2.as_millis() >= 1500, "b2 should be >= 1.5s, got {:?}", b2);
        assert!(b3.as_millis() >= 3000, "b3 should be >= 3s, got {:?}", b3);
    }

    #[test]
    fn test_schedule_backoff_staircase() {
        // The default overload/stream schedule is a flat staircase the user
        // expects: first retry quick, then 5s, 15s, 35s, capped at max_backoff.
        let schedule = vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(15),
            Duration::from_secs(35),
        ];
        let max = Duration::from_secs(60);

        let b1 = schedule_backoff(1, &schedule, max);
        let b2 = schedule_backoff(2, &schedule, max);
        let b3 = schedule_backoff(3, &schedule, max);
        let b4 = schedule_backoff(4, &schedule, max);
        // Past the end of the schedule the final entry (35s) is reused.
        let b5 = schedule_backoff(5, &schedule, max);

        assert!(
            b1.as_millis() >= 1000 && b1.as_millis() < 2000,
            "b1 ~1s, got {:?}",
            b1
        );
        assert!(
            b2.as_millis() >= 5000 && b2.as_millis() < 6000,
            "b2 ~5s, got {:?}",
            b2
        );
        assert!(
            b3.as_millis() >= 15000 && b3.as_millis() < 17000,
            "b3 ~15s, got {:?}",
            b3
        );
        assert!(
            b4.as_millis() >= 35000 && b4.as_millis() < 39000,
            "b4 ~35s, got {:?}",
            b4
        );
        assert!(b5.as_millis() >= 35000, "b5 should reuse 35s, got {:?}", b5);
    }

    #[test]
    fn test_schedule_backoff_capped_by_max() {
        let schedule = vec![Duration::from_secs(35), Duration::from_secs(90)];
        let max = Duration::from_secs(60);
        let b = schedule_backoff(2, &schedule, max);
        assert!(
            (b.as_millis() as f64) <= 60_000.0 * 1.1,
            "schedule entry must be capped by max_backoff, got {:?}",
            b
        );
    }

    #[test]
    fn test_reset_clears_rate_limit_retries() {
        let mut state = RetryState {
            consecutive_failures: 3,
            rate_limit_retries: 5,
            overload_retries: 2,
            using_fallback: false,
            using_failover: false,
        };
        state.reset();
        assert_eq!(state.rate_limit_retries, 0);
        assert_eq!(state.consecutive_failures, 0);
        // overload_retries and using_fallback persist.
        assert_eq!(state.overload_retries, 2);
    }

    #[test]
    fn test_overloads_then_fallback_then_abort() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            max_overload_retries: 1,
            ..Default::default()
        };
        let err = RetryableError::Overloaded;

        // First overload: retry with backoff.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { .. } => {}
            other => panic!("Expected Retry, got {other:?}"),
        }

        // Second overload: exceeds max_overload_retries, triggers fallback.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::FallbackModel => {}
            other => panic!("Expected FallbackModel, got {other:?}"),
        }
        assert!(state.using_fallback);

        // Now on fallback model, overload again: retry.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { .. } => {}
            other => panic!("Expected Retry on fallback, got {other:?}"),
        }

        // Exceed overloads on fallback: abort.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(msg) => assert!(msg.contains("fallback")),
            other => panic!("Expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn test_stream_interrupted_retries_then_aborts() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            max_retries: 2,
            ..Default::default()
        };
        let err = RetryableError::StreamInterrupted;

        // First two interruptions should retry.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { .. } => {}
            other => panic!("Expected Retry, got {other:?}"),
        }
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { .. } => {}
            other => panic!("Expected Retry, got {other:?}"),
        }

        // Third interruption exceeds max_retries => abort.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(msg) => assert!(msg.contains("Stream")),
            other => panic!("Expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn test_network_error_retries_then_aborts() {
        let mut state = RetryState::default();
        let config = RetryConfig {
            max_network_retries: 2,
            ..Default::default()
        };
        let err = RetryableError::Network;

        // First two transport failures should retry with backoff.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { .. } => {}
            other => panic!("Expected Retry, got {other:?}"),
        }
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { .. } => {}
            other => panic!("Expected Retry, got {other:?}"),
        }

        // Third failure exceeds max_retries => abort, so the turn stops
        // instead of looping forever on an unreachable endpoint.
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(msg) => assert!(msg.contains("Network")),
            other => panic!("Expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn test_network_error_uses_longer_backoff() {
        // Transport failures have no status to derive a wait from, so the
        // network backoff (seconds-scale) should kick in on the first retry,
        // not the short initial_backoff (1s).
        let mut state = RetryState::default();
        let config = RetryConfig::default();
        match state.next_action(
            &RetryableError::Network,
            &config,
            ProviderKind::OpenAi,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Retry { after } => {
                assert!(
                    after.as_millis() >= 5000,
                    "network retry should use seconds-scale backoff, got {}ms",
                    after.as_millis()
                );
            }
            other => panic!("Expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn test_retry_state_default_values() {
        let state = RetryState::default();
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.rate_limit_retries, 0);
        assert_eq!(state.overload_retries, 0);
        assert!(!state.using_fallback);
    }

    #[test]
    fn test_classify_maps_model_404_to_unavailable() {
        use crate::llm::provider::ProviderError;
        // OpenAI-compatible servers surface an unknown model as a 404
        // InvalidResponse; failover should treat it as ModelUnavailable.
        let err = ProviderError::InvalidResponse("model laguna-s not found".into());
        let retryable = RetryableError::classify(&err);
        assert!(matches!(
            retryable,
            RetryableError::ModelUnavailable { model } if model.contains("laguna-s")
        ));
    }

    #[test]
    fn test_failover_opencode_laguna_to_kilo() {
        let err = RetryableError::ModelUnavailable {
            model: "opencode:laguna-s".into(),
        };
        let target = err.failover_target(ProviderKind::OpenCode, false);
        match target {
            Some(t) => {
                assert_eq!(t.provider, ProviderKind::Kilo);
                assert_eq!(t.model_hint, "laguna-s");
            }
            None => panic!("expected OpenCode laguna-s → Kilo failover"),
        }
    }

    #[test]
    fn test_failover_opencode_x_preview_to_kilo() {
        let err = RetryableError::ModelUnavailable {
            model: "x-preview-f-free".into(),
        };
        let target = err.failover_target(ProviderKind::OpenCode, false);
        match target {
            Some(t) => {
                assert_eq!(t.provider, ProviderKind::Kilo);
                assert_eq!(t.model_hint, "ox-alpha");
            }
            None => panic!("expected OpenCode x-preview-f-free → Kilo failover"),
        }
    }

    #[test]
    fn test_failover_does_not_apply_to_non_opencode() {
        let err = RetryableError::ModelUnavailable {
            model: "laguna-s".into(),
        };
        assert!(err.failover_target(ProviderKind::OpenAi, false).is_none());
    }

    #[test]
    fn test_failover_only_once_per_turn() {
        let err = RetryableError::ModelUnavailable {
            model: "laguna-s".into(),
        };
        // After a failover, the next ModelUnavailable on the same turn must
        // not produce another failover target (prevents provider ping-pong).
        assert!(err.failover_target(ProviderKind::OpenCode, true).is_none());
    }

    #[test]
    fn test_next_action_falls_over_when_rule_matches() {
        let mut state = RetryState::default();
        let config = RetryConfig::default();
        let err = RetryableError::ModelUnavailable {
            model: "laguna-s".into(),
        };
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenCode,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Failover { target } => {
                assert_eq!(target.provider, ProviderKind::Kilo);
                assert_eq!(target.model_hint, "laguna-s");
            }
            other => panic!("Expected Failover, got {other:?}"),
        }
    }

    #[test]
    fn test_next_action_aborts_when_no_rule() {
        let mut state = RetryState::default();
        let config = RetryConfig::default();
        let err = RetryableError::ModelUnavailable {
            model: "some-other-model".into(),
        };
        match state.next_action(
            &err,
            &config,
            ProviderKind::OpenCode,
            false,
            "",
            &HashMap::new(),
        ) {
            RetryAction::Abort(_) => {}
            other => panic!("Expected Abort, got {other:?}"),
        }
    }
}

#[test]
fn test_failover_mapping_configured_openrouter_to_kilo() {
    // openrouter -> kilo is in the default mapping. Set KILO_API_KEY so the
    // rule is guaranteed to fire regardless of the ambient/parallel env,
    // then restore whatever was there.
    let prior = std::env::var("KILO_API_KEY").ok();
    unsafe {
        std::env::set_var("KILO_API_KEY", "test-key");
    }
    let mut mapping = HashMap::new();
    mapping.insert(
        "openrouter".to_string(),
        ("kilo".to_string(), "tencent/hy3:free".to_string()),
    );
    let err = RetryableError::ModelUnavailable {
        model: "anthropic/claude-3.5-sonnet".into(),
    };
    let target = failover_target_configured(
        &err,
        ProviderKind::OpenRouter,
        "anthropic/claude-3.5-sonnet",
        false,
        &mapping,
    );
    unsafe {
        match prior {
            Some(v) => std::env::set_var("KILO_API_KEY", v),
            None => std::env::remove_var("KILO_API_KEY"),
        }
    }
    match target {
        Some(t) => {
            assert_eq!(t.provider, ProviderKind::Kilo);
            assert_eq!(t.model_hint, "tencent/hy3:free");
        }
        None => panic!("expected openrouter -> kilo failover from mapping"),
    }
}

#[test]
fn test_failover_mapping_skips_unknown_provider() {
    // A mapped target whose provider name is not recognized must be skipped,
    // so no failover is produced. This does not depend on the ambient env.
    let mut mapping = HashMap::new();
    mapping.insert(
        "openrouter".to_string(),
        ("zzz_no_such_provider".to_string(), "whatever".to_string()),
    );
    let err = RetryableError::ModelUnavailable {
        model: "anthropic/claude-3.5-sonnet".into(),
    };
    let target = failover_target_configured(
        &err,
        ProviderKind::OpenRouter,
        "anthropic/claude-3.5-sonnet",
        false,
        &mapping,
    );
    // Unknown target is skipped; the static table has no openrouter rule.
    assert!(target.is_none());
}

#[test]
fn test_failover_mapping_specific_model_key() {
    // "provider/model" keys only match when the model fragment is present.
    let mut mapping = HashMap::new();
    mapping.insert(
        "openrouter/anthropic/claude-3.5-sonnet".to_string(),
        ("kilo".to_string(), "tencent/hy3:free".to_string()),
    );
    let err = RetryableError::ModelUnavailable {
        model: "anthropic/claude-3.5-sonnet".into(),
    };
    let matched = failover_target_configured(
        &err,
        ProviderKind::OpenRouter,
        "anthropic/claude-3.5-sonnet",
        false,
        &mapping,
    );
    assert!(matched.is_some());
    let unmatched = failover_target_configured(
        &err,
        ProviderKind::OpenRouter,
        "google/gemini-pro",
        false,
        &mapping,
    );
    assert!(unmatched.is_none());
}

#[test]
fn test_failover_mapping_only_once_per_turn() {
    // Once a failover has already happened (using_failover = true), no
    // further rotation is attempted, even with a valid mapping.
    let mut mapping = HashMap::new();
    mapping.insert(
        "openrouter".to_string(),
        ("kilo".to_string(), "tencent/hy3:free".to_string()),
    );
    let err = RetryableError::ModelUnavailable {
        model: "anthropic/claude-3.5-sonnet".into(),
    };
    let target = failover_target_configured(
        &err,
        ProviderKind::OpenRouter,
        "anthropic/claude-3.5-sonnet",
        true,
        &mapping,
    );
    assert!(target.is_none());
}
