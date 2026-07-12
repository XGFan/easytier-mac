//! Thin wrapper over the easytier RPC portal (DESIGN §8).
//!
//! Connects to `127.0.0.1:<rpc_port>` (the port the supervisor reports for the
//! managed core) using the crate's public `StandAloneClient` and drives the
//! same RPC surface as easytier-cli: run/delete/list network instances and
//! per-instance peer/route queries. The active port is set from supervisor
//! events; each call opens a short-lived client so a core restart (new port) is
//! picked up transparently.

use std::sync::Mutex;

use easytier::common::config::{ConfigLoader, ConfigSource, TomlConfigLoader};
use easytier::launcher::NetworkConfig;
use easytier::proto::api::instance::{
    InstanceIdentifier, ListPeerRequest, ListRouteRequest, PeerManageRpc,
    PeerManageRpcClientFactory, ShowNodeInfoRequest, instance_identifier::Selector,
    list_peer_route_pair,
};
use easytier::proto::api::manage::{
    DeleteNetworkInstanceRequest, RunNetworkInstanceRequest, WebClientService,
    WebClientServiceClientFactory,
};
use easytier::proto::rpc_impl::standalone::StandAloneClient;
use easytier::proto::rpc_types::controller::BaseController;
use easytier::tunnel::tcp::TcpTunnelConnector;
use easytier::utils::PeerRoutePair;
use serde::Serialize;
use uuid::Uuid;

type Client = StandAloneClient<TcpTunnelConnector>;

/// One row in the status table (a peer, or the local node).
#[derive(Debug, Clone, Serialize)]
pub struct PeerRow {
    pub peer_id: u32,
    pub hostname: String,
    pub ipv4: String,
    pub cost: String,
    pub latency_ms: f64,
    pub loss_rate: f64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub nat_type: String,
    pub version: String,
    pub is_local: bool,
    /// Transport protocols seen on this peer's connections (`udp`/`tcp`/...),
    /// from `PeerRoutePair::get_conn_protos()`. Empty for the local row.
    pub protos: Vec<String>,
}

/// Peer/route snapshot for one running instance.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    pub instance_id: String,
    pub rows: Vec<PeerRow>,
}

/// RPC client bound to the current core's portal port.
pub struct RpcClient {
    port: Mutex<Option<u16>>,
}

impl RpcClient {
    pub fn new() -> Self {
        Self {
            port: Mutex::new(None),
        }
    }

    pub fn set_port(&self, port: Option<u16>) {
        *self.port.lock().unwrap() = port;
    }

    pub fn port(&self) -> Option<u16> {
        *self.port.lock().unwrap()
    }

    fn new_client(&self) -> Result<Client, String> {
        let port = self.port().ok_or("core is not running (no rpc port)".to_string())?;
        let url = format!("tcp://127.0.0.1:{port}")
            .parse()
            .map_err(|e| format!("invalid rpc url: {e}"))?;
        Ok(StandAloneClient::new(TcpTunnelConnector::new(url)))
    }

    async fn manage_client(
        client: &mut Client,
    ) -> Result<Box<dyn WebClientService<Controller = BaseController> + Send + Sync>, String> {
        client
            .scoped_client::<WebClientServiceClientFactory<BaseController>>("".to_string())
            .await
            .map_err(|e| format!("connect rpc portal: {e}"))
    }

    async fn peer_client(
        client: &mut Client,
    ) -> Result<Box<dyn PeerManageRpc<Controller = BaseController> + Send + Sync>, String> {
        client
            .scoped_client::<PeerManageRpcClientFactory<BaseController>>("".to_string())
            .await
            .map_err(|e| format!("connect rpc portal: {e}"))
    }

    /// Start (or overwrite) a network instance from raw profile TOML. Returns the
    /// resolved instance id so the caller can track and later stop it.
    ///
    /// The supervisor returns the core's `rpc_port` as soon as the process is
    /// spawned, before the core has bound its RPC portal listener. When we are
    /// invoked right after a cold start (manual toggle or autostart restore), the
    /// first connect therefore races the portal and fails. We retry the connect +
    /// RPC call with a bounded backoff so a freshly-spawned core is picked up once
    /// its portal is ready; config parsing happens once up front and is never
    /// retried (a bad config must fail fast).
    pub async fn run_network_instance(&self, toml_text: &str) -> Result<Uuid, String> {
        let loader = TomlConfigLoader::new_from_str(toml_text)
            .map_err(|e| friendly_config_error(toml_text, e))?;
        let inst_id = loader.get_id();
        let config = NetworkConfig::new_from_config(&loader)
            .map_err(|e| friendly_config_error(toml_text, e))?;

        // ~6s total: covers core process startup + RPC portal bind on a cold start.
        const MAX_ATTEMPTS: u32 = 30;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
        let mut last_err = String::new();
        for attempt in 0..MAX_ATTEMPTS {
            match self.try_run_network_instance(inst_id, &config).await {
                Ok(()) => return Ok(inst_id),
                Err(e) => {
                    last_err = e;
                    if attempt + 1 < MAX_ATTEMPTS {
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
        }
        Err(format!(
            "run network instance failed after {MAX_ATTEMPTS} attempts: {last_err}"
        ))
    }

    async fn try_run_network_instance(
        &self,
        inst_id: Uuid,
        config: &NetworkConfig,
    ) -> Result<(), String> {
        let mut client = self.new_client()?;
        let manage = Self::manage_client(&mut client).await?;
        manage
            .run_network_instance(
                BaseController::default(),
                RunNetworkInstanceRequest {
                    inst_id: Some(inst_id.into()),
                    config: Some(config.clone()),
                    overwrite: true,
                    source: ConfigSource::User.to_rpc(),
                },
            )
            .await
            .map_err(|e| format!("run network instance: {e}"))?;
        Ok(())
    }

    pub async fn delete_network_instance(&self, inst_id: Uuid) -> Result<(), String> {
        let mut client = self.new_client()?;
        let manage = Self::manage_client(&mut client).await?;
        manage
            .delete_network_instance(
                BaseController::default(),
                DeleteNetworkInstanceRequest {
                    inst_ids: vec![inst_id.into()],
                },
            )
            .await
            .map_err(|e| format!("delete network instance: {e}"))?;
        Ok(())
    }

    /// Fetch peer/route pairs for one instance and shape them into status rows.
    pub async fn network_status(&self, inst_id: Uuid) -> Result<NetworkStatus, String> {
        let selector = InstanceIdentifier {
            selector: Some(Selector::Id(inst_id.into())),
        };
        let mut client = self.new_client()?;
        let peer = Self::peer_client(&mut client).await?;

        let peers = peer
            .list_peer(
                BaseController::default(),
                ListPeerRequest {
                    instance: Some(selector.clone()),
                },
            )
            .await
            .map_err(|e| format!("list peers: {e}"))?
            .peer_infos;
        let routes = peer
            .list_route(
                BaseController::default(),
                ListRouteRequest {
                    instance: Some(selector.clone()),
                },
            )
            .await
            .map_err(|e| format!("list routes: {e}"))?
            .routes;

        let mut rows = Vec::new();
        if let Ok(resp) = peer
            .show_node_info(
                BaseController::default(),
                ShowNodeInfoRequest {
                    instance: Some(selector),
                },
            )
            .await
        {
            if let Some(node) = resp.node_info {
                rows.push(PeerRow {
                    peer_id: node.peer_id,
                    hostname: node.hostname,
                    ipv4: node.ipv4_addr,
                    cost: "local".to_string(),
                    latency_ms: 0.0,
                    loss_rate: 0.0,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    nat_type: node
                        .stun_info
                        .map(|s| s.udp_nat_type().as_str_name().to_string())
                        .unwrap_or_default(),
                    version: node.version,
                    is_local: true,
                    protos: Vec::new(),
                });
            }
        }

        for pair in list_peer_route_pair(peers, routes) {
            rows.push(peer_row_from_pair(pair));
        }

        Ok(NetworkStatus {
            instance_id: inst_id.to_string(),
            rows,
        })
    }
}

impl Default for RpcClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Shape a `PeerRoutePair` into a status row, mirroring easytier-cli's peer
/// table derivation (latency uses direct measurement for cost-1 peers, else the
/// route's latency-first path cost).
fn peer_row_from_pair(pair: PeerRoutePair) -> PeerRow {
    let route = pair.route.clone().unwrap_or_default();
    let latency_ms = if route.cost == 1 {
        pair.get_latency_ms().unwrap_or(0.0)
    } else {
        route.path_latency_latency_first() as f64
    };
    let cost = match route.cost {
        1 => "direct".to_string(),
        n => format!("relay({n})"),
    };
    PeerRow {
        peer_id: route.peer_id,
        hostname: route.hostname.clone(),
        ipv4: route
            .ipv4_addr
            .map(|ip| ip.to_string())
            .unwrap_or_default(),
        cost,
        latency_ms,
        loss_rate: pair.get_loss_rate().unwrap_or(0.0) as f64,
        rx_bytes: pair.get_rx_bytes().unwrap_or(0),
        tx_bytes: pair.get_tx_bytes().unwrap_or(0),
        nat_type: pair.get_udp_nat_type(),
        version: if route.version.is_empty() {
            "unknown".to_string()
        } else {
            route.version
        },
        is_local: false,
        protos: pair.get_conn_protos().unwrap_or_default(),
    }
}

/// Two-layer config validation used by `etb_validate` (DESIGN §9): parse the TOML
/// with `TomlConfigLoader`, then build a `NetworkConfig` from it. Touches neither
/// disk nor any running instance. Returns the first error message on failure.
pub fn validate_config(toml_text: &str) -> Result<(), String> {
    let loader = TomlConfigLoader::new_from_str(toml_text)
        .map_err(|e| friendly_config_error(toml_text, e))?;
    NetworkConfig::new_from_config(&loader).map_err(|e| friendly_config_error(toml_text, e))?;
    Ok(())
}

/// `TomlConfigLoader` 的解析错误会把整份配置原文嵌进错误串(含 network_secret),
/// 对 UI 既冗长又泄密,而真正的行列号诊断反而缺失。这里重建一条干净的单行诊断:
/// 语法错误用 toml crate 重新解析拿 "TOML parse error at line N, column M";
/// 语义错误则把配置原文从消息里剥掉。配置原文在任何分支都不会出现在返回值里。
pub(crate) fn friendly_config_error(toml_text: &str, raw: impl ToString) -> String {
    if let Err(e) = toml_text.parse::<toml::Table>() {
        let first = e.to_string();
        let first = first.lines().next().unwrap_or("TOML parse error").to_owned();
        return format!("配置解析失败: {first}");
    }
    let raw = raw.to_string();
    let stripped = raw
        .replace(toml_text, "")
        .replace(toml_text.trim(), "")
        .replace("failed to parse config file:", "")
        .replace("invalid config:", "");
    let msg = stripped.trim().trim_end_matches(':').trim();
    if msg.is_empty() {
        "配置无效(字段校验失败)".to_owned()
    } else {
        format!("配置无效: {msg}")
    }
}

#[cfg(test)]
mod friendly_error_tests {
    use super::*;

    const SECRET: &str = "super-secret-value-xyz";

    fn bad_syntax_config() -> String {
        format!(
            "hostname = \"mbp\"\n\n[network_identity]\nnetwork_secret = \"{SECRET}\"\n\nthis is [ not valid toml\n"
        )
    }

    #[test]
    fn syntax_error_reports_line_and_never_leaks_config() {
        let cfg = bad_syntax_config();
        let err = validate_config(&cfg).unwrap_err();
        // 单行、带行号定位、不含配置原文与密钥
        assert_eq!(err.lines().count(), 1, "must be single line: {err:?}");
        assert!(err.contains("line 6"), "expect line diagnostics: {err}");
        assert!(!err.contains(SECRET), "secret leaked: {err}");
        assert!(!err.contains("hostname"), "config text leaked: {err}");
    }

    #[test]
    fn semantic_error_strips_config_text() {
        // 语法合法但字段类型错误(ipv4 需要字符串)
        let cfg = format!(
            "ipv4 = 12345\n\n[network_identity]\nnetwork_secret = \"{SECRET}\"\n"
        );
        let err = validate_config(&cfg).unwrap_err();
        assert!(!err.contains(SECRET), "secret leaked: {err}");
    }

    #[test]
    fn valid_config_passes() {
        assert!(validate_config("hostname = \"x\"\n[network_identity]\nnetwork_name = \"n\"\n").is_ok());
    }
}
