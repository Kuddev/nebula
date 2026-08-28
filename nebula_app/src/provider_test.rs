//! Provider connectivity test results shared by the business and presentation layers.
//!
//! The test worker returns stable meaning and interpolation parameters only. UI
//! runtimes turn these variants into localized text at their display boundary.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTestOutcome {
    Success { status: u16 },
    InvalidEndpoint,
    MissingModel,
    MissingApiKey,
    CredentialReadFailed,
    InvalidCredentialEncoding,
    Timeout,
    HostNotFound,
    ConnectionFailed,
    Io { kind: String },
    Tls,
    RequestFailed,
    AuthFailed { status: u16 },
    EndpointNotFound { status: u16 },
    RateLimited { status: u16 },
    HttpStatus { status: u16 },
    StartFailed { error: String },
}

impl ProviderTestOutcome {
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}
