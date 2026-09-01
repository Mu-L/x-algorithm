pub const ENV_GRPC_MTLS_ENABLED: &str = "GRPC_MTLS_ENABLED";
pub const ENV_GRPC_MTLS_SERVER_KEY_PATH: &str = "GRPC_MTLS_SERVER_KEY_PATH";
pub const ENV_GRPC_MTLS_SERVER_CRT_PATH: &str = "GRPC_MTLS_SERVER_CRT_PATH";
pub const ENV_GRPC_MTLS_SERVER_CHAIN_PATH: &str = "GRPC_MTLS_SERVER_CHAIN_PATH";
pub const ENV_GRPC_MTLS_CLIENT_CA_PATH: &str = "GRPC_MTLS_CLIENT_CA_PATH";
pub const ENV_DUAL_CALL_HARNESS_ENABLED: &str = "VF_DUAL_CALL_HARNESS_ENABLED";
pub const ENV_FALLBACK_CACHE_SERVE_STALE_ENABLED: &str = "VF_FALLBACK_CACHE_SERVE_STALE_ENABLED";
pub const ENV_FALLBACK_CACHE_POPULATE_ENABLED: &str = "VF_FALLBACK_CACHE_POPULATE_ENABLED";
pub const ENV_CACHE_WARM_SAMPLE_PCT: &str = "VF_CACHE_WARM_SAMPLE_PCT";
pub const ENV_APP_ENV: &str = "APP_ENV";
pub const ENV_FS_PATH: &str = "VF_FS_PATH";
pub const ENV_GIZMODUCK_CLIENT_ID: &str = "VF_GIZMODUCK_CLIENT_ID";
pub const ENV_TWEMCACHE_CLIENT_NAME: &str = "VF_TWEMCACHE_CLIENT_NAME";

pub const DEFAULT_FS_PATH: &str = "/usr/local/config/features/visibility/main/rust_vf.yml";

pub fn fs_path() -> String {
    std::env::var(ENV_FS_PATH).unwrap_or_else(|_| DEFAULT_FS_PATH.to_string())
}

pub fn gizmoduck_client_id() -> String {
    resolve_gizmoduck_client_id(
        std::env::var(ENV_GIZMODUCK_CLIENT_ID).ok().as_deref(),
        std::env::var(ENV_APP_ENV).ok().as_deref(),
    )
}

pub fn twemcache_client_name() -> String {
    resolve_twemcache_client_name(std::env::var(ENV_TWEMCACHE_CLIENT_NAME).ok().as_deref())
}

pub fn resolve_gizmoduck_client_id(configured: Option<&str>, app_env: Option<&str>) -> String {
    match configured {
        Some(id) => id.to_string(),
        None => format!("visibility-filtering-service.{}", app_env.unwrap_or("prod")),
    }
}

pub fn resolve_twemcache_client_name(configured: Option<&str>) -> String {
    configured
        .unwrap_or("visibility-filtering-service")
        .to_string()
}

pub fn dual_call_harness_enabled() -> bool {
    parse_env_flag(std::env::var(ENV_DUAL_CALL_HARNESS_ENABLED).ok().as_deref())
}

pub fn fallback_cache_serve_stale_enabled() -> bool {
    parse_env_flag(
        std::env::var(ENV_FALLBACK_CACHE_SERVE_STALE_ENABLED)
            .ok()
            .as_deref(),
    )
}

pub fn fallback_cache_populate_enabled() -> bool {
    parse_env_flag(
        std::env::var(ENV_FALLBACK_CACHE_POPULATE_ENABLED)
            .ok()
            .as_deref(),
    )
}

pub fn cache_warm_sample_pct() -> u8 {
    parse_sample_pct(std::env::var(ENV_CACHE_WARM_SAMPLE_PCT).ok().as_deref())
        .unwrap_or_else(|error| panic!("{ENV_CACHE_WARM_SAMPLE_PCT}: {error}"))
}

fn parse_sample_pct(value: Option<&str>) -> Result<u8, String> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(0);
    };
    match value.parse::<u8>() {
        Ok(pct) if pct <= 100 => Ok(pct),
        _ => Err(format!("expected an integer 0-100, got {value:?}")),
    }
}

fn parse_env_flag(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Debug, Clone)]
pub struct GrpcMtlsConfig {
    pub server_key_path: String,
    pub server_crt_path: String,
    pub server_chain_path: Option<String>,
    pub client_ca_path: String,
}

impl GrpcMtlsConfig {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let enabled = parse_env_flag(std::env::var(ENV_GRPC_MTLS_ENABLED).ok().as_deref());

        if !enabled {
            return Ok(None);
        }

        let server_key_path = std::env::var(ENV_GRPC_MTLS_SERVER_KEY_PATH)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{ENV_GRPC_MTLS_SERVER_KEY_PATH} must be set"))?;

        let server_crt_path = std::env::var(ENV_GRPC_MTLS_SERVER_CRT_PATH)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{ENV_GRPC_MTLS_SERVER_CRT_PATH} must be set"))?;

        let server_chain_path = std::env::var(ENV_GRPC_MTLS_SERVER_CHAIN_PATH)
            .ok()
            .filter(|v| !v.is_empty());

        let client_ca_path = std::env::var(ENV_GRPC_MTLS_CLIENT_CA_PATH)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{ENV_GRPC_MTLS_CLIENT_CA_PATH} must be set"))?;

        Ok(Some(Self {
            server_key_path,
            server_crt_path,
            server_chain_path,
            client_ca_path,
        }))
    }

    pub fn server_tls_config(&self) -> anyhow::Result<tonic::transport::ServerTlsConfig> {
        let mut cert_pem = std::fs::read(&self.server_crt_path)?;
        let key_pem = std::fs::read(&self.server_key_path)?;
        let client_ca_pem = std::fs::read(&self.client_ca_path)?;

        if let Some(chain_path) = self.server_chain_path.as_ref() {
            let chain_pem = std::fs::read(chain_path)?;
            if !cert_pem.ends_with(b"\n") {
                cert_pem.push(b'\n');
            }
            cert_pem.extend_from_slice(&chain_pem);
        }

        let identity = tonic::transport::Identity::from_pem(cert_pem, key_pem);
        let client_ca = tonic::transport::Certificate::from_pem(client_ca_pem);

        Ok(tonic::transport::ServerTlsConfig::new()
            .identity(identity)
            .client_ca_root(client_ca))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_env_flag, parse_sample_pct, resolve_gizmoduck_client_id,
        resolve_twemcache_client_name,
    };

    #[test]
    fn parses_sample_pct_range() {
        assert_eq!(parse_sample_pct(None), Ok(0));
        assert_eq!(parse_sample_pct(Some("")), Ok(0));
        assert_eq!(parse_sample_pct(Some("100")), Ok(100));
    }

    #[test]
    fn rejects_invalid_sample_pct() {
        for value in ["101", "on"] {
            assert!(parse_sample_pct(Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn client_ids_default_to_historical_values_and_overrides_win() {
        assert_eq!(
            resolve_gizmoduck_client_id(None, Some("prod")),
            "visibility-filtering-service.prod"
        );
        assert_eq!(
            resolve_gizmoduck_client_id(None, Some("staging")),
            "visibility-filtering-service.staging"
        );
        assert_eq!(
            resolve_gizmoduck_client_id(None, None),
            "visibility-filtering-service.prod"
        );
        assert_eq!(
            resolve_twemcache_client_name(None),
            "visibility-filtering-service"
        );
        assert_eq!(
            resolve_gizmoduck_client_id(Some("xai-vf-service.staging"), Some("staging")),
            "xai-vf-service.staging"
        );
    }

    #[test]
    fn parses_enabled_environment_values() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert!(parse_env_flag(Some(value)), "{value}");
        }
    }

    #[test]
    fn missing_or_disabled_environment_values_are_off() {
        assert!(!parse_env_flag(None));
        for value in ["", "0", "false", "FALSE", "no", "off", "other"] {
            assert!(!parse_env_flag(Some(value)), "{value}");
        }
    }
}
