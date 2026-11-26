//! The records Domovoi stores and exchanges with its clients, as Rust types, and
//! the thread read model built on them. Shared by domovoi-server, which keeps the
//! records in SQLite and serves them, and the client core, which holds them
//! in memory.
//!
//! Nearly all of it is [`typed`]: a type for every record of t3code's wire
//! schemas (threads and what they are made of, events and commands, settings,
//! server configuration, projects, version control and the rest), each
//! decoding from the JSON a server sends and encoding back to the same JSON,
//! and [`typed::Thread`], one thread kept current event by event and the
//! source of what is derived from it. The crate does no I/O.
//!
//! The crate root holds the two things that are not records:
//! [`ProviderDriver`], the provider runtimes Domovoi drives, and [`enc`].

#![allow(clippy::disallowed_types, clippy::disallowed_macros, reason = "this crate is where JSON becomes records: its decoders read JSON values, and a field the schema leaves open holds one")]

pub mod typed;

/// The version of what the server and its clients send each other. It goes
/// up with every change one side makes that the other must know about, so a
/// server and a client of different versions say which of them needs
/// updating instead of misreading each other. 2 was the last version shared
/// with t3code.
pub const PROTOCOL_VERSION: u32 = 3;

/// `encodeURIComponent` for the id characters t3code uses (`:` and `%`).
pub fn enc(s: &str) -> String {
    s.replace('%', "%25").replace(':', "%3A")
}

/// The provider runtimes Domovoi drives (an instance's `driver`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderDriver {
    Codex,
    ClaudeAgent,
    OpenCode,
    Pi,
    /// An agent speaking the Agent Client Protocol (t3code's `acpRegistry`
    /// driver): from the ACP registry, or a local command.
    AcpRegistry,
    /// Google's Antigravity CLI over ACP (t3code's `antigravity` driver).
    Antigravity,
    /// xAI's Grok CLI over ACP (`grok agent stdio`, t3code's `grok` driver).
    Grok,
    /// AWS's Kiro CLI over ACP (`kiro-cli acp`, t3code's `kiro` driver).
    Kiro,
    /// Vercel's fx CLI over ACP (`fx acp`, t3code's `fx` driver).
    Fx,
}

impl ProviderDriver {
    /// From its wire name; `None` for a driver Domovoi cannot run.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "codex" => Some(Self::Codex),
            "claudeAgent" => Some(Self::ClaudeAgent),
            "opencode" => Some(Self::OpenCode),
            "pi" => Some(Self::Pi),
            "acpRegistry" => Some(Self::AcpRegistry),
            "antigravity" => Some(Self::Antigravity),
            "grok" => Some(Self::Grok),
            "kiro" => Some(Self::Kiro),
            "fx" => Some(Self::Fx),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeAgent => "claudeAgent",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
            Self::AcpRegistry => "acpRegistry",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
            Self::Kiro => "kiro",
            Self::Fx => "fx",
        }
    }

    /// Its live turns take more input (Claude's `priority: now` messages,
    /// Codex `turn/steer`, Pi's `prompt` with `streamingBehavior: steer`,
    /// Grok's `_x.ai/interject`, Kiro's `_session/steer`).
    pub const fn can_steer(self) -> bool {
        matches!(self, Self::Codex | Self::ClaudeAgent | Self::Pi | Self::Grok | Self::Kiro)
    }

    /// It runs over the Agent Client Protocol (one agent process per thread).
    pub const fn is_acp(self) -> bool {
        matches!(self, Self::AcpRegistry | Self::Antigravity | Self::Grok | Self::Kiro | Self::Fx)
    }

    /// It can fork its own conversation (rather than be handed one as text).
    pub const fn can_native_fork(self) -> bool {
        matches!(self, Self::Codex | Self::ClaudeAgent | Self::Pi | Self::OpenCode)
    }
}

impl std::fmt::Display for ProviderDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
