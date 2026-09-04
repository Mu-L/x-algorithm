use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use envoy_types::pb::envoy::config::core::v3::Node;
use envoy_types::pb::envoy::config::listener::v3::Listener;
use envoy_types::pb::envoy::service::discovery::v3::aggregated_discovery_service_client::AggregatedDiscoveryServiceClient;
use envoy_types::pb::envoy::service::discovery::v3::DiscoveryRequest;
use prost::Message;
use tower::util::Either;
use tracing::info;

use tonic::transport::Channel;
use xai_dark_traffic::{DarkTrafficLayer, ReloadableDarkTrafficConfigBuilder};
use xai_x_rpc::dynamic_channel_manager::{
    ChannelFactory, DynamicChannelManager, EndpointDiscovery, EndpointInfo,
};
use xai_x_rpc::grpc_client::TlsMode;
use xai_x_rpc::xds_channel_factory::XdsChannelFactory;

const CONFIG_PATH: &str = "/config/dark-traffic/dark_traffic.yaml";

pub const STAGING_NAMESPACE: &str = "visibility";
pub const STAGING_APP_ENV: &str = "staging";
pub const STAGING_PORT_ID: &str = "grpc";
pub const STAGING_WORKLOAD_PREFIX: &str = "xai-vf-service";

const LISTENER_TYPE_URL: &str = "type.googleapis.com/envoy.config.listener.v3.Listener";
const LDS_MAX_DECODING_MESSAGE_SIZE: usize = 256 * 1024 * 1024;
const LDS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const CHANNEL_CREATE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn staging_tls_domain(dc: &str) -> String {
    format!("visibility.visibility-filtering-service.staging.{dc}.s2s.twttr.net")
}

pub type DarkLayer = Either<DarkTrafficLayer, tower::layer::util::Identity>;

pub fn parse_staging_listener(listener_name: &str) -> Option<EndpointInfo> {
    let dest = listener_name.rsplit('/').next().unwrap_or(listener_name);
    let dest = dest.split('?').next().unwrap_or(dest);
    let suffix = format!(".{STAGING_APP_ENV}.{STAGING_NAMESPACE}:{STAGING_PORT_ID}");
    let workload = dest.strip_suffix(suffix.as_str())?;
    if !workload.starts_with(STAGING_WORKLOAD_PREFIX) || workload.contains('.') {
        return None;
    }
    Some(EndpointInfo {
        name: workload.to_string(),
        xds_dest: dest.to_string(),
    })
}

struct XdsStagingDiscovery {
    server_uri: String,
}

#[async_trait::async_trait]
impl EndpointDiscovery for XdsStagingDiscovery {
    async fn discover(&self) -> anyhow::Result<Vec<EndpointInfo>> {
        tokio::time::timeout(LDS_RESPONSE_TIMEOUT, self.fetch())
            .await
            .context("wildcard LDS exchange timed out")?
    }
}

impl XdsStagingDiscovery {
    async fn fetch(&self) -> anyhow::Result<Vec<EndpointInfo>> {
        let channel = tonic::transport::Endpoint::from_shared(self.server_uri.clone())
            .context("invalid kube-discovery URI")?
            .connect()
            .await
            .context("failed to connect to kube-discovery")?;

        let mut client = AggregatedDiscoveryServiceClient::new(channel)
            .max_decoding_message_size(LDS_MAX_DECODING_MESSAGE_SIZE);

        let request = DiscoveryRequest {
            type_url: LISTENER_TYPE_URL.to_string(),
            resource_names: vec!["*".to_string()],
            node: Some(Node {
                id: format!("{STAGING_WORKLOAD_PREFIX}-dark-traffic"),
                cluster: STAGING_NAMESPACE.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        use futures::StreamExt;
        let requests = futures::stream::iter([request]).chain(futures::stream::pending());
        let mut stream = client
            .stream_aggregated_resources(requests)
            .await
            .context("wildcard LDS stream failed to open")?
            .into_inner();

        let response = loop {
            let response = stream
                .message()
                .await
                .context("wildcard LDS stream errored")?
                .context("wildcard LDS stream ended without a listener-carrying response")?;
            if !response.resources.is_empty() {
                break response;
            }
        };

        let names: Vec<String> = response
            .resources
            .iter()
            .filter_map(|any| Some(Listener::decode(any.value.as_ref()).ok()?.name))
            .collect();
        anyhow::ensure!(
            !names.is_empty(),
            "none of {} LDS resources decoded as Listener",
            response.resources.len()
        );
        let endpoints: Vec<EndpointInfo> = names
            .iter()
            .filter_map(|name| parse_staging_listener(name))
            .collect();

        if endpoints.is_empty() {
            tracing::warn!(
                listeners = names.len(),
                "dark_traffic: no staging listeners matched"
            );
        } else {
            info!(
                names = %endpoints.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", "),
                "dark_traffic: discovery complete"
            );
        }

        Ok(endpoints)
    }
}

struct TimeoutChannelFactory {
    inner: XdsChannelFactory,
}

#[async_trait::async_trait]
impl ChannelFactory for TimeoutChannelFactory {
    async fn create_channel(&self, ep: &EndpointInfo) -> anyhow::Result<Channel> {
        tokio::time::timeout(CHANNEL_CREATE_TIMEOUT, self.inner.create_channel(ep))
            .await
            .with_context(|| format!("channel dial timed out for {}", ep.xds_dest))?
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

    let workload = std::env::var("WORKLOAD_NAME").ok();
    let max_ordinal: Option<u32> = std::env::var("DARK_TRAFFIC_MAX_ORDINAL")
        .ok()
        .and_then(|s| s.parse().ok());
    let ordinal: Option<u32> = std::env::var("ORDINAL_NUMBER")
        .ok()
        .and_then(|s| s.parse().ok());
    if !should_mirror(workload.as_deref(), ordinal, max_ordinal) {
        info!(
            ?workload,
            ?ordinal,
            ?max_ordinal,
            "dark_traffic: disabled (not a mirror host)"
        );
        return Either::Right(tower::layer::util::Identity::new());
    }

    let dc = std::env::var("DATACENTER").unwrap_or_else(|_| "atla".to_string());
    let discovery = XdsStagingDiscovery {
        server_uri: format!("http://frontend.kube-discovery.prod.svc.{dc}.kube.int-x.ai:8082"),
    };
    let domain = staging_tls_domain(&dc);
    info!(domain, "dark_traffic: enabled");

    #[expect(clippy::expect_used, reason = "startup fail-fast: TLS is required")]
    let factory = XdsChannelFactory::new(
        TlsMode::mtls_from_env()
            .expect("S2S TLS config required")
            .with_domain_override(&domain),
    );

    let channels = DynamicChannelManager::new(
        Arc::new(TimeoutChannelFactory { inner: factory }),
        Arc::new(discovery),
    );

    let config = ReloadableDarkTrafficConfigBuilder::new(CONFIG_PATH)
        .forwarders({
            let ch = Arc::clone(&channels);
            move || ch.channels()
        })
        .build();

    Either::Left(DarkTrafficLayer::new(config))
}

fn should_mirror(workload: Option<&str>, ordinal: Option<u32>, max_ordinal: Option<u32>) -> bool {
    let Some(workload) = workload else {
        return false;
    };
    if workload.ends_with("-canary") {
        return false;
    }
    let Some(ord) = ordinal else {
        return false;
    };
    ord < max_ordinal.unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROD: Option<&str> = Some("xai-vf-service");
    const CANARY: Option<&str> = Some("xai-vf-service-canary");

    #[test]
    fn default_only_pod0() {
        assert!(should_mirror(PROD, Some(0), None));
        assert!(!should_mirror(PROD, Some(1), None));
        assert!(!should_mirror(PROD, Some(99), None));
    }

    #[test]
    fn no_ordinal_disables() {
        assert!(!should_mirror(PROD, None, None));
        assert!(!should_mirror(PROD, None, Some(3)));
    }

    #[test]
    fn max_ordinal_threshold() {
        assert!(should_mirror(PROD, Some(0), Some(3)));
        assert!(should_mirror(PROD, Some(1), Some(3)));
        assert!(should_mirror(PROD, Some(2), Some(3)));
        assert!(!should_mirror(PROD, Some(3), Some(3)));
        assert!(!should_mirror(PROD, Some(4), Some(3)));
    }

    #[test]
    fn max_ordinal_zero_disables_all() {
        assert!(!should_mirror(PROD, Some(0), Some(0)));
    }

    #[test]
    fn canary_never_mirrors() {
        assert!(!should_mirror(CANARY, Some(0), Some(11)));
        assert!(!should_mirror(CANARY, Some(0), Some(u32::MAX)));
    }

    #[test]
    fn missing_workload_name_fails_closed() {
        assert!(!should_mirror(None, Some(0), Some(11)));
    }

    #[test]
    fn parse_accepts_vf_staging_listeners() {
        for (listener, workload) in [
            ("xai-vf-service.staging.visibility:grpc", "xai-vf-service"),
            (
                "xai-vf-service-user1-foo.staging.visibility:grpc",
                "xai-vf-service-user1-foo",
            ),
            (
                "xdstp://kube-discovery/envoy.config.listener.v3.Listener/xai-vf-service.staging.visibility:grpc?key=val",
                "xai-vf-service",
            ),
        ] {
            let ep = parse_staging_listener(listener).expect(listener);
            assert_eq!(ep.name, workload);
            assert_eq!(ep.xds_dest, format!("{workload}.staging.visibility:grpc"));
        }
    }

    #[test]
    fn parse_rejects_out_of_scope_listeners() {
        for name in [
            "xai-vf-service.prod.visibility:grpc",
            "other-svc.staging.visibility:grpc",
            "xai-vf-service.staging.other:grpc",
            "xai-vf-service.staging.visibility:metrics",
            "evil.xai-vf-service.staging.visibility:grpc",
            "xai-vf-service.staging.visibility",
            "",
        ] {
            assert!(parse_staging_listener(name).is_none(), "{name}");
        }
    }
}
