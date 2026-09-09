//! Sanitized provider rate-limit dimensions used by transport and progress output.

/// Provider quota dimension inferred from structured error metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitKind {
    RequestsPerMinute,
    TokensPerMinute,
    RequestsPerDay,
    TokensPerDay,
    Spend,
    Unknown,
}

impl RateLimitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestsPerMinute => "rpm",
            Self::TokensPerMinute => "tpm",
            Self::RequestsPerDay => "rpd",
            Self::TokensPerDay => "tpd",
            Self::Spend => "spend",
            Self::Unknown => "unknown",
        }
    }

    pub const fn is_daily(self) -> bool {
        matches!(self, Self::RequestsPerDay | Self::TokensPerDay)
    }
}
