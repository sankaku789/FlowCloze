//! Provider quota settings and request pacing.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaProfile {
    pub name: String,
    pub rpm: Option<u32>,
    pub tpm: Option<u64>,
    pub rpd: Option<u32>,
    pub reserve_requests: u32,
    pub adaptive_max_tasks_per_batch: Option<usize>,
    pub adaptive_max_input_tokens: Option<usize>,
    pub adaptive_max_output_tokens: Option<usize>,
    pub adaptive_max_blanks_per_batch: Option<usize>,
}

impl QuotaProfile {
    pub fn request_budget(&self) -> Option<usize> {
        self.rpd
            .map(|rpd| rpd.saturating_sub(self.reserve_requests) as usize)
    }

    pub fn disable_adaptive_expansion(&mut self) {
        self.adaptive_max_tasks_per_batch = None;
        self.adaptive_max_input_tokens = None;
        self.adaptive_max_output_tokens = None;
        self.adaptive_max_blanks_per_batch = None;
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuotaProfileConfig {
    rpm: Option<u32>,
    tpm: Option<u64>,
    rpd: Option<u32>,
    #[serde(default)]
    reserve_requests: u32,
    adaptive_max_tasks_per_batch: Option<usize>,
    adaptive_max_input_tokens: Option<usize>,
    adaptive_max_output_tokens: Option<usize>,
    adaptive_max_blanks_per_batch: Option<usize>,
    #[serde(default)]
    models: HashMap<String, QuotaOverrideConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaOverrideConfig {
    rpm: Option<u32>,
    tpm: Option<u64>,
    rpd: Option<u32>,
    reserve_requests: Option<u32>,
    adaptive_max_tasks_per_batch: Option<usize>,
    adaptive_max_input_tokens: Option<usize>,
    adaptive_max_output_tokens: Option<usize>,
    adaptive_max_blanks_per_batch: Option<usize>,
}

impl QuotaProfileConfig {
    pub(crate) fn resolve(&self, name: String, model: &str) -> Result<QuotaProfile, String> {
        let override_config = self.models.get(model);
        let profile = QuotaProfile {
            name,
            rpm: override_config.and_then(|value| value.rpm).or(self.rpm),
            tpm: override_config.and_then(|value| value.tpm).or(self.tpm),
            rpd: override_config.and_then(|value| value.rpd).or(self.rpd),
            reserve_requests: override_config
                .and_then(|value| value.reserve_requests)
                .unwrap_or(self.reserve_requests),
            adaptive_max_tasks_per_batch: override_config
                .and_then(|value| value.adaptive_max_tasks_per_batch)
                .or(self.adaptive_max_tasks_per_batch),
            adaptive_max_input_tokens: override_config
                .and_then(|value| value.adaptive_max_input_tokens)
                .or(self.adaptive_max_input_tokens),
            adaptive_max_output_tokens: override_config
                .and_then(|value| value.adaptive_max_output_tokens)
                .or(self.adaptive_max_output_tokens),
            adaptive_max_blanks_per_batch: override_config
                .and_then(|value| value.adaptive_max_blanks_per_batch)
                .or(self.adaptive_max_blanks_per_batch),
        };
        validate_profile(&profile)?;
        Ok(profile)
    }
}

fn validate_profile(profile: &QuotaProfile) -> Result<(), String> {
    if profile.rpm == Some(0) {
        return Err(format!(
            "quota profile '{}' の rpm は1以上にしてください",
            profile.name
        ));
    }
    if profile.tpm == Some(0) {
        return Err(format!(
            "quota profile '{}' の tpm は1以上にしてください",
            profile.name
        ));
    }
    if profile.rpd == Some(0) {
        return Err(format!(
            "quota profile '{}' の rpd は1以上にしてください",
            profile.name
        ));
    }
    if profile.adaptive_max_tasks_per_batch == Some(0) {
        return Err(format!(
            "quota profile '{}' の adaptive_max_tasks_per_batch は1以上にしてください",
            profile.name
        ));
    }
    if profile.adaptive_max_input_tokens == Some(0) {
        return Err(format!(
            "quota profile '{}' の adaptive_max_input_tokens は1以上にしてください",
            profile.name
        ));
    }
    if profile.adaptive_max_output_tokens == Some(0) {
        return Err(format!(
            "quota profile '{}' の adaptive_max_output_tokens は1以上にしてください",
            profile.name
        ));
    }
    if profile.adaptive_max_blanks_per_batch == Some(0) {
        return Err(format!(
            "quota profile '{}' の adaptive_max_blanks_per_batch は1以上にしてください",
            profile.name
        ));
    }
    if let Some(rpd) = profile.rpd {
        if profile.reserve_requests >= rpd {
            return Err(format!(
                "quota profile '{}' の reserve_requests は rpd より小さくしてください",
                profile.name
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct RateGate {
    inner: Arc<RateGateInner>,
}

struct RateGateInner {
    profile: QuotaProfile,
    state: Mutex<RateState>,
}

#[derive(Default)]
struct RateState {
    last_sent: Option<Instant>,
    recent: VecDeque<(Instant, u64)>,
}

impl RateGate {
    pub fn new(profile: QuotaProfile) -> Self {
        Self {
            inner: Arc::new(RateGateInner {
                profile,
                state: Mutex::new(RateState::default()),
            }),
        }
    }

    /// RPMは均等間隔、TPMは直近60秒のrolling windowとして送信前に待機する。
    pub fn acquire(&self, estimated_tokens: u64) -> Result<Duration, String> {
        let window = Duration::from_secs(60);
        let mut total_wait = Duration::ZERO;
        loop {
            let now = Instant::now();
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| "quota rate gate lock failed")?;
            while state
                .recent
                .front()
                .is_some_and(|(sent, _)| now.saturating_duration_since(*sent) >= window)
            {
                state.recent.pop_front();
            }

            let mut wait = Duration::ZERO;
            if let Some(rpm) = self.inner.profile.rpm {
                let interval = interval_for_rpm(rpm);
                if let Some(last_sent) = state.last_sent {
                    let next = last_sent + interval;
                    if next > now {
                        wait = wait.max(next.duration_since(now));
                    }
                }
            }

            if let Some(tpm) = self.inner.profile.tpm {
                if estimated_tokens > tpm {
                    return Err(format!(
                        "estimated request tokens ({estimated_tokens}) exceed configured TPM ({tpm})"
                    ));
                }
                let used = state.recent.iter().map(|(_, tokens)| *tokens).sum::<u64>();
                if used.saturating_add(estimated_tokens) > tpm {
                    let mut need_to_expire = used + estimated_tokens - tpm;
                    for (sent, tokens) in &state.recent {
                        need_to_expire = need_to_expire.saturating_sub(*tokens);
                        if need_to_expire == 0 {
                            let available = *sent + window;
                            if available > now {
                                wait = wait.max(available.duration_since(now));
                            }
                            break;
                        }
                    }
                }
            }

            if wait.is_zero() {
                state.last_sent = Some(now);
                state.recent.push_back((now, estimated_tokens));
                return Ok(total_wait);
            }
            drop(state);
            std::thread::sleep(wait);
            total_wait += wait;
        }
    }
}

fn interval_for_rpm(rpm: u32) -> Duration {
    Duration::from_secs_f64(60.0 / f64::from(rpm.max(1)))
}

pub fn estimate_request_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    text.chars()
        .map(|ch| {
            if matches!(
                ch,
                '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
            ) || ch.is_ascii_alphanumeric()
            {
                1u64
            } else {
                0u64
            }
        })
        .sum::<u64>()
        .max(chars / 4)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_budget_reserves_daily_requests() {
        let profile = QuotaProfile {
            name: "gemini".into(),
            rpm: Some(5),
            tpm: Some(250_000),
            rpd: Some(20),
            reserve_requests: 4,
            adaptive_max_tasks_per_batch: Some(12),
            adaptive_max_input_tokens: Some(18_000),
            adaptive_max_output_tokens: Some(6_000),
            adaptive_max_blanks_per_batch: Some(24),
        };
        assert_eq!(profile.request_budget(), Some(16));
    }

    #[test]
    fn model_override_replaces_profile_values() {
        let config: QuotaProfileConfig = toml::from_str(
            r#"
rpm = 5
tpm = 250000
rpd = 20
reserve_requests = 4

[models."gemini-3.8-flash"]
rpd = 30
"#,
        )
        .unwrap();
        let profile = config.resolve("gemini".into(), "gemini-3.8-flash").unwrap();
        assert_eq!(profile.rpm, Some(5));
        assert_eq!(profile.rpd, Some(30));
        assert_eq!(profile.reserve_requests, 4);
    }

    #[test]
    fn rpm_interval_is_evenly_spaced() {
        assert_eq!(interval_for_rpm(5), Duration::from_secs(12));
    }

    #[test]
    fn request_token_estimator_counts_japanese_conservatively() {
        assert!(estimate_request_tokens("日本語のprompt 123") >= 8);
    }
}
