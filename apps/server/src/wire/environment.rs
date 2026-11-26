use serde::{Deserialize, Serialize};
use domovoi_model::typed::{
    EnvironmentMachineKind, ExecutionEnvironmentCapabilities, ExecutionEnvironmentDescriptor, ExecutionEnvironmentPlatform,
};

/// `ExecutionEnvironmentDescriptor` (served at `/.well-known/t3/environment`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDescriptor {
    pub environment_id: String,
    pub label: String,
    pub platform: Platform,
    pub server_version: String,
    pub orchestration_protocol_version: u32,
    pub database_migration: u32,
    pub capabilities: ExecutionEnvironmentCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    pub machine: String,
}

impl Platform {
    pub fn current() -> Self {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            other => other,
        };
        Platform { os: os.into(), arch: arch.into(), machine: "laptop".into() }
    }
}

impl From<&EnvironmentDescriptor> for ExecutionEnvironmentDescriptor {
    /// The descriptor as the record clients read, which the server's config
    /// and lifecycle events carry: the same JSON.
    fn from(environment: &EnvironmentDescriptor) -> Self {
        ExecutionEnvironmentDescriptor {
            environment_id: environment.environment_id.as_str().into(),
            label: environment.label.clone(),
            platform: ExecutionEnvironmentPlatform {
                os: environment.platform.os.as_str().into(),
                arch: environment.platform.arch.as_str().into(),
                machine: Some(EnvironmentMachineKind::from(environment.platform.machine.as_str())),
            },
            server_version: environment.server_version.clone(),
            orchestration_protocol_version: Some(environment.orchestration_protocol_version.into()),
            database_migration: Some(environment.database_migration.into()),
            capabilities: environment.capabilities.clone(),
        }
    }
}

/// The version domovoi-server reports: its own.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ORCHESTRATION_PROTOCOL_VERSION: u32 = domovoi_model::PROTOCOL_VERSION;
/// Highest t3code migration whose schema the client may rely on.
pub const DATABASE_MIGRATION: u32 = 89;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_and_the_record_clients_read_are_the_same_json() {
        let environment = EnvironmentDescriptor {
            environment_id: "env-1".into(),
            label: "Mac".into(),
            platform: Platform { os: "freebsd".into(), arch: "riscv64".into(), machine: "laptop".into() },
            server_version: SERVER_VERSION.into(),
            orchestration_protocol_version: ORCHESTRATION_PROTOCOL_VERSION,
            database_migration: DATABASE_MIGRATION,
            capabilities: crate::defaults::capabilities(),
        };
        assert_eq!(serde_json::json!(ExecutionEnvironmentDescriptor::from(&environment)), serde_json::json!(environment));
        let current = EnvironmentDescriptor { platform: Platform::current(), ..environment };
        assert_eq!(serde_json::json!(ExecutionEnvironmentDescriptor::from(&current)), serde_json::json!(current));
    }
}
