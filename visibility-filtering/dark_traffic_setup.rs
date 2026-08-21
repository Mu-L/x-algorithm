use std::sync::Arc;
use tonic::async_trait;
use tower::util::Either;
use tracing::info;

use xai_dark_traffic::{DarkTrafficLayer, ReloadableDarkTrafficConfigBuilder};
use xai_x_rpc::dynamic_channel_manager::{DynamicChannelManager, EndpointDiscovery, EndpointInfo};
use xai_x_rpc::grpc_client::TlsMode;
use xai_x_rpc::xds_channel_factory::XdsChannelFactory;

const CONFIG_PATH: &str = "/config/dark-traffic/dark_traffic.yaml";
pub const STAGING_XDS_DEST: &str = "xai-vf-service.staging.visibility:grpc";

const FORWARDER_NAME: &str = "staging";

pub fn staging_tls_domain(dc: &str) -> String {
    format!("visibility.visibility-filtering-service.staging.{dc}.s2s.twttr.net")
}

pub type DarkLayer = Either<DarkTrafficLayer, tower::layer::util::Identity>;

struct StaticStagingDiscovery;

#[async_trait]
impl EndpointDiscovery for StaticStagingDiscovery {
    async fn discover(&self) -> anyhow::Result<Vec<EndpointInfo>> {
        Ok(vec![EndpointInfo {
            name: FORWARDER_NAME.to_string(),
            xds_dest: STAGING_XDS_DEST.to_string(),
        }])
    }
}

pub fn resolve_layer() -> DarkLayer {
    if !std::env::var("DARK_TRAFFIC_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        info!("dark_traffic: disabled");
        return Either::Right(tower::layer::util::Identity::new());
    }

    let max_ordinal: Option<u32> = std::env::var("DARK_TRAFFIC_MAX_ORDINAL")
        .ok()
        .and_then(|s| s.parse().ok());
    let ordinal: Option<u32> = std::env::var("ORDINAL_NUMBER")
        .ok()
        .and_then(|s| s.parse().ok());
    if !should_enable(ordinal, max_ordinal) {
        info!(
            ?ordinal,
            ?max_ordinal,
            "dark_traffic: disabled (ordinal >= max)"
        );
        return Either::Right(tower::layer::util::Identity::new());
    }

    let dc = std::env::var("DATACENTER").unwrap_or_else(|_| "atla".to_string());
    let domain = staging_tls_domain(&dc);
    info!(domain, "dark_traffic: enabled");

    let factory = XdsChannelFactory::new(
        TlsMode::mtls_from_env()
            .expect("S2S TLS config required")
            .with_domain_override(&domain),
    );

    let channels = DynamicChannelManager::new(Arc::new(factory), Arc::new(StaticStagingDiscovery));

    let config = ReloadableDarkTrafficConfigBuilder::new(CONFIG_PATH)
        .forwarders({
            let ch = Arc::clone(&channels);
            move || ch.channels()
        })
        .build();

    Either::Left(DarkTrafficLayer::new(config))
}

fn should_enable(ordinal: Option<u32>, max_ordinal: Option<u32>) -> bool {
    let max = max_ordinal.unwrap_or(1);
    let ord = ordinal.unwrap_or(u32::MAX);
    ord < max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_only_pod0() {
        assert!(should_enable(Some(0), None));
        assert!(!should_enable(Some(1), None));
        assert!(!should_enable(Some(99), None));
    }

    #[test]
    fn no_ordinal_disables() {
        assert!(!should_enable(None, None));
        assert!(!should_enable(None, Some(3)));
    }

    #[test]
    fn max_ordinal_threshold() {
        assert!(should_enable(Some(0), Some(3)));
        assert!(should_enable(Some(2), Some(3)));
        assert!(!should_enable(Some(3), Some(3)));
        assert!(!should_enable(Some(4), Some(3)));
    }

    #[test]
    fn max_ordinal_zero_disables_all() {
        assert!(!should_enable(Some(0), Some(0)));
    }
}
