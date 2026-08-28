//! Semantic results for the settings-page network connectivity test.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyTestRoute {
    Direct,
    DirectAddress,
    CustomCommand,
    ProxyServer(String),
    SshJump(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyTestFailure {
    LoadSettings(String),
    InvalidSettings(String),
    Timeout { seconds: u64 },
    SendRequest(String),
    ReadResponse(String),
    InvalidHttpStatusLine(String),
    HttpStatus { status: u16 },
    ProxyServer { server: String, error: String },
    CustomCommand(String),
    JumpTask(String),
    JumpResolve { target: String, error: String },
    JumpConnect { target: String, error: String },
    JumpChannel { target: String, error: String },
    Direct(String),
    Start(String),
    UnexpectedEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyTestOutcome {
    Success(ProxyTestRoute),
    Failed(ProxyTestFailure),
}

impl ProxyTestOutcome {
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTestResult {
    pub request_id: u64,
    pub outcome: ProxyTestOutcome,
    pub elapsed_ms: u64,
}
