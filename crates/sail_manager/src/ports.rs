use std::collections::BTreeMap;
use std::fmt;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use collections::{HashMap, HashSet};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::compose::{ComposeConfig, PortConfig, validate_service_name};

pub const ASSIGNMENTS_KEY: &str = "sail_manager_port_assignments";
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
const PRIVILEGED_PORT_LIMIT: u16 = 1024;
const PRIVILEGED_PORT_OFFSET: u32 = 8000;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortKey {
    pub service: String,
    pub container_port: u16,
    pub protocol: String,
}

impl PortKey {
    pub fn pin_key(&self) -> String {
        format!("{}:{}/{}", self.service, self.container_port, self.protocol)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRequest {
    pub key: PortKey,
    pub host_ip: Option<String>,
    pub original_host_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortAssignment {
    pub key: PortKey,
    pub host_ip: Option<String>,
    pub host_port: u16,
    pub original_host_port: u16,
    pub pinned: bool,
}

impl PortAssignment {
    pub fn is_remapped(&self) -> bool {
        self.host_port != self.original_host_port
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConflict {
    pub key: PortKey,
    pub port: u16,
}

impl fmt::Display for PortConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Port {} for {} is in use",
            self.port, self.key.service
        )
    }
}

impl std::error::Error for PortConflict {}

pub fn requests_from_config(config: &ComposeConfig) -> Vec<PortRequest> {
    config
        .services
        .iter()
        .flat_map(|(service, service_config)| {
            service_config.ports.iter().filter_map(move |port| {
                Some(PortRequest {
                    key: PortKey {
                        service: service.clone(),
                        container_port: port.target,
                        protocol: port.protocol.clone(),
                    },
                    host_ip: port.host_ip.clone().filter(|host_ip| !host_ip.is_empty()),
                    original_host_port: port.published_port()?,
                })
            })
        })
        .collect()
}

fn first_candidate(original: u16) -> u32 {
    if original < PRIVILEGED_PORT_LIMIT {
        u32::from(original) + PRIVILEGED_PORT_OFFSET
    } else {
        u32::from(original) + 1
    }
}

pub fn assign_ports(
    requests: &[PortRequest],
    previous: &[PortAssignment],
    pins: &HashMap<String, u16>,
    reserved_by_others: &HashSet<u16>,
    is_free: &dyn Fn(u16) -> bool,
) -> Result<Vec<PortAssignment>, PortConflict> {
    let mut used: HashSet<u16> = HashSet::default();
    let mut assigned: Vec<Option<PortAssignment>> = vec![None; requests.len()];

    for (index, request) in requests.iter().enumerate() {
        let Some(&port) = pins.get(&request.key.pin_key()) else {
            continue;
        };
        if reserved_by_others.contains(&port) || used.contains(&port) || !is_free(port) {
            return Err(PortConflict {
                key: request.key.clone(),
                port,
            });
        }
        used.insert(port);
        if let Some(slot) = assigned.get_mut(index) {
            *slot = Some(PortAssignment {
                key: request.key.clone(),
                host_ip: request.host_ip.clone(),
                host_port: port,
                original_host_port: request.original_host_port,
                pinned: true,
            });
        }
    }

    let available = |port: u16, used: &HashSet<u16>| {
        !reserved_by_others.contains(&port) && !used.contains(&port) && is_free(port)
    };

    for (index, request) in requests.iter().enumerate() {
        if assigned.get(index).is_some_and(Option::is_some) {
            continue;
        }
        let previous_port = previous
            .iter()
            .find(|assignment| assignment.key == request.key && !assignment.pinned)
            .map(|assignment| assignment.host_port)
            .filter(|port| available(*port, &used));
        let chosen = if let Some(port) = previous_port {
            Some(port)
        } else if available(request.original_host_port, &used) {
            Some(request.original_host_port)
        } else {
            (first_candidate(request.original_host_port)..=u32::from(u16::MAX))
                .filter_map(|candidate| u16::try_from(candidate).ok())
                .find(|candidate| available(*candidate, &used))
        };
        let Some(port) = chosen else {
            return Err(PortConflict {
                key: request.key.clone(),
                port: request.original_host_port,
            });
        };
        used.insert(port);
        if let Some(slot) = assigned.get_mut(index) {
            *slot = Some(PortAssignment {
                key: request.key.clone(),
                host_ip: request.host_ip.clone(),
                host_port: port,
                original_host_port: request.original_host_port,
                pinned: false,
            });
        }
    }

    Ok(assigned.into_iter().flatten().collect())
}

fn render_port(port: &PortConfig, host_port: Option<String>) -> String {
    let mut rendered = String::new();
    if let Some(host_ip) = port
        .host_ip
        .as_deref()
        .filter(|host_ip| !host_ip.is_empty())
    {
        if host_ip.contains(':') {
            rendered.push_str(&format!("[{host_ip}]:"));
        } else {
            rendered.push_str(&format!("{host_ip}:"));
        }
    }
    if let Some(host_port) = host_port {
        rendered.push_str(&host_port);
        rendered.push(':');
    }
    rendered.push_str(&port.target.to_string());
    if port.protocol != "tcp" {
        rendered.push('/');
        rendered.push_str(&port.protocol);
    }
    rendered
}

pub fn override_yaml(
    assignments: &[PortAssignment],
    ports_by_service: &BTreeMap<String, Vec<PortConfig>>,
) -> Result<Option<String>> {
    let changed_services: HashSet<&str> = assignments
        .iter()
        .filter(|assignment| assignment.is_remapped())
        .map(|assignment| assignment.key.service.as_str())
        .collect();
    let mut output = String::new();
    for (service, ports) in ports_by_service {
        if !changed_services.contains(service.as_str()) {
            continue;
        }
        validate_service_name(service)?;
        if output.is_empty() {
            output.push_str("services:\n");
        }
        output.push_str(&format!("  {service}:\n    ports: !override\n"));
        for port in ports {
            let host_port = match port.published_port() {
                Some(original) => {
                    let assignment = assignments.iter().find(|assignment| {
                        assignment.key.service == *service
                            && assignment.key.container_port == port.target
                            && assignment.key.protocol == port.protocol
                            && assignment.original_host_port == original
                            && assignment.host_ip.as_deref().filter(|ip| !ip.is_empty())
                                == port.host_ip.as_deref().filter(|ip| !ip.is_empty())
                    });
                    Some(
                        assignment
                            .map(|assignment| assignment.host_port)
                            .unwrap_or(original)
                            .to_string(),
                    )
                }
                None => port
                    .published
                    .clone()
                    .filter(|published| !published.is_empty()),
            };
            output.push_str(&format!("      - \"{}\"\n", render_port(port, host_port)));
        }
    }
    if output.is_empty() {
        Ok(None)
    } else {
        Ok(Some(output))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerPsEntry {
    #[serde(default)]
    labels: String,
    #[serde(default)]
    ports: String,
}

fn project_label(labels: &str) -> Option<String> {
    labels.split(',').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == COMPOSE_PROJECT_LABEL).then(|| value.trim().to_string())
    })
}

fn published_host_ports(ports: &str) -> Vec<u16> {
    let mut host_ports = Vec::new();
    for entry in ports.split(',') {
        let Some((host_side, _)) = entry.trim().split_once("->") else {
            continue;
        };
        let Some((_, host_port)) = host_side.rsplit_once(':') else {
            continue;
        };
        let range = match host_port.split_once('-') {
            Some((start, end)) => start.parse::<u16>().ok().zip(end.parse::<u16>().ok()),
            None => host_port.parse::<u16>().ok().map(|port| (port, port)),
        };
        if let Some((start, end)) = range {
            for port in start..=end {
                if !host_ports.contains(&port) {
                    host_ports.push(port);
                }
            }
        }
    }
    host_ports
}

pub fn parse_docker_ps_ports(output: &str) -> Result<Vec<(Option<String>, u16)>> {
    let mut published = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let entry: DockerPsEntry =
            serde_json::from_str(line).context("parsing `docker ps` output")?;
        let label = project_label(&entry.labels);
        for port in published_host_ports(&entry.ports) {
            published.push((label.clone(), port));
        }
    }
    Ok(published)
}

pub trait PortProbe: Send + Sync {
    fn is_free(&self, port: u16) -> bool;
}

pub struct SystemPortProbe;

impl PortProbe for SystemPortProbe {
    fn is_free(&self, port: u16) -> bool {
        TcpListener::bind(("0.0.0.0", port)).is_ok()
    }
}

pub type AssignmentRegistry = BTreeMap<String, Vec<PortAssignment>>;

pub trait AssignmentStore: Send + Sync + 'static {
    fn load(&self) -> BoxFuture<'static, Result<AssignmentRegistry>>;
    fn save(&self, registry: AssignmentRegistry) -> BoxFuture<'static, Result<()>>;
}

pub struct KvpAssignmentStore {
    store: db::kvp::KeyValueStore,
}

impl KvpAssignmentStore {
    pub fn new(store: db::kvp::KeyValueStore) -> Self {
        Self { store }
    }
}

impl AssignmentStore for KvpAssignmentStore {
    fn load(&self) -> BoxFuture<'static, Result<AssignmentRegistry>> {
        let stored = self.store.read_kvp(ASSIGNMENTS_KEY);
        Box::pin(async move {
            match stored? {
                Some(value) => serde_json::from_str(&value)
                    .context("parsing the stored Sail Manager port assignments"),
                None => Ok(AssignmentRegistry::new()),
            }
        })
    }

    fn save(&self, registry: AssignmentRegistry) -> BoxFuture<'static, Result<()>> {
        let store = self.store.clone();
        Box::pin(async move {
            if registry.is_empty() {
                store.delete_kvp(ASSIGNMENTS_KEY.to_string()).await
            } else {
                store
                    .write_kvp(
                        ASSIGNMENTS_KEY.to_string(),
                        serde_json::to_string(&registry)?,
                    )
                    .await
            }
        })
    }
}

pub fn reserved_by_other_roots(registry: &AssignmentRegistry, root_key: &str) -> HashSet<u16> {
    registry
        .iter()
        .filter(|(other_root, _)| other_root.as_str() != root_key)
        .flat_map(|(_, assignments)| assignments.iter().map(|assignment| assignment.host_port))
        .collect()
}

fn stable_hash(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn override_file_path(data_directory: &Path, canonical_root: &Path) -> PathBuf {
    data_directory
        .join("sail_manager")
        .join(format!(
            "{:016x}",
            stable_hash(&canonical_root.to_string_lossy())
        ))
        .join("ports.override.yaml")
}

pub fn generated_override_path(canonical_root: &Path) -> PathBuf {
    override_file_path(paths::data_dir(), canonical_root)
}

pub fn compose_supports_port_override(version: &str) -> bool {
    let numeric: Vec<u32> = version
        .trim()
        .trim_start_matches('v')
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .take(3)
        .filter_map(|part| part.parse().ok())
        .collect();
    let mut parts = numeric.into_iter();
    let found = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    found >= (2, 24, 4)
}

pub fn moved_ports_message(assignments: &[PortAssignment]) -> Option<String> {
    let moved: Vec<String> = assignments
        .iter()
        .filter(|assignment| assignment.is_remapped())
        .map(|assignment| {
            format!(
                "{}: {} \u{2192} {}",
                assignment.key.service, assignment.original_host_port, assignment.host_port
            )
        })
        .collect();
    if moved.is_empty() {
        None
    } else {
        Some(moved.join(", "))
    }
}

#[cfg(test)]
pub(crate) struct InMemoryAssignmentStore {
    registry: std::sync::Arc<std::sync::Mutex<AssignmentRegistry>>,
}

#[cfg(test)]
impl InMemoryAssignmentStore {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            registry: std::sync::Arc::new(std::sync::Mutex::new(AssignmentRegistry::new())),
        })
    }

    pub(crate) fn snapshot(&self) -> AssignmentRegistry {
        self.registry
            .lock()
            .map(|registry| registry.clone())
            .unwrap_or_default()
    }

    pub(crate) fn seed(&self, root: &str, assignments: Vec<PortAssignment>) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.insert(root.to_string(), assignments);
        }
    }
}

#[cfg(test)]
impl AssignmentStore for InMemoryAssignmentStore {
    fn load(&self) -> BoxFuture<'static, Result<AssignmentRegistry>> {
        let registry = self.snapshot();
        Box::pin(async move { Ok(registry) })
    }

    fn save(&self, registry: AssignmentRegistry) -> BoxFuture<'static, Result<()>> {
        if let Ok(mut stored) = self.registry.lock() {
            *stored = registry;
        }
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
pub(crate) struct FakePortProbe {
    pub(crate) busy: std::sync::Arc<std::sync::Mutex<HashSet<u16>>>,
}

#[cfg(test)]
impl FakePortProbe {
    pub(crate) fn new(busy: &[u16]) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            busy: std::sync::Arc::new(std::sync::Mutex::new(busy.iter().copied().collect())),
        })
    }
}

#[cfg(test)]
impl PortProbe for FakePortProbe {
    fn is_free(&self, port: u16) -> bool {
        self.busy
            .lock()
            .map(|busy| !busy.contains(&port))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(service: &str, container_port: u16) -> PortKey {
        PortKey {
            service: service.to_string(),
            container_port,
            protocol: "tcp".to_string(),
        }
    }

    fn request(service: &str, container_port: u16, original: u16) -> PortRequest {
        PortRequest {
            key: key(service, container_port),
            host_ip: None,
            original_host_port: original,
        }
    }

    fn assignment(
        service: &str,
        container_port: u16,
        original: u16,
        host_port: u16,
        pinned: bool,
    ) -> PortAssignment {
        PortAssignment {
            key: key(service, container_port),
            host_ip: None,
            host_port,
            original_host_port: original,
            pinned,
        }
    }

    fn ports_of(assignments: &[PortAssignment]) -> Vec<u16> {
        assignments
            .iter()
            .map(|assignment| assignment.host_port)
            .collect()
    }

    fn assign(
        requests: &[PortRequest],
        previous: &[PortAssignment],
        pins: &[(&str, u16)],
        reserved: &[u16],
        busy: &[u16],
    ) -> Result<Vec<PortAssignment>, PortConflict> {
        let pins: HashMap<String, u16> = pins
            .iter()
            .map(|(name, port)| (name.to_string(), *port))
            .collect();
        let reserved: HashSet<u16> = reserved.iter().copied().collect();
        let busy: HashSet<u16> = busy.iter().copied().collect();
        assign_ports(requests, previous, &pins, &reserved, &|port| {
            !busy.contains(&port)
        })
    }

    #[test]
    fn test_original_port_is_kept_when_free() {
        let result = assign(&[request("web", 8000, 8000)], &[], &[], &[], &[]).unwrap();
        assert_eq!(ports_of(&result), [8000]);
        assert!(!result[0].is_remapped());
        assert!(!result[0].pinned);
    }

    #[test]
    fn test_previous_assignment_is_kept_when_still_free() {
        let previous = [assignment("web", 8000, 8000, 8005, false)];
        let result = assign(&[request("web", 8000, 8000)], &previous, &[], &[], &[]).unwrap();
        assert_eq!(ports_of(&result), [8005]);
    }

    #[test]
    fn test_previous_assignment_that_is_now_busy_gets_a_new_port() {
        let previous = [assignment("web", 8000, 8000, 8005, false)];
        let result = assign(&[request("web", 8000, 8000)], &previous, &[], &[], &[8005]).unwrap();
        assert_eq!(ports_of(&result), [8000]);
        let result = assign(
            &[request("web", 8000, 8000)],
            &previous,
            &[],
            &[],
            &[8005, 8000],
        )
        .unwrap();
        assert_eq!(ports_of(&result), [8001]);
    }

    #[test]
    fn test_pinned_previous_assignment_is_not_reused_as_automatic() {
        let previous = [assignment("web", 8000, 8000, 8005, true)];
        let result = assign(&[request("web", 8000, 8000)], &previous, &[], &[], &[]).unwrap();
        assert_eq!(ports_of(&result), [8000]);
    }

    #[test]
    fn test_port_reserved_by_another_project_moves_to_the_next_free() {
        let result = assign(&[request("web", 8000, 8000)], &[], &[], &[8000, 8001], &[]).unwrap();
        assert_eq!(ports_of(&result), [8002]);
    }

    #[test]
    fn test_busy_system_port_moves_to_the_next_free() {
        let result = assign(&[request("db", 3306, 3306)], &[], &[], &[], &[3306, 3307]).unwrap();
        assert_eq!(ports_of(&result), [3308]);
    }

    #[test]
    fn test_privileged_port_moves_above_8000() {
        let result = assign(&[request("web", 80, 80)], &[], &[], &[], &[80]).unwrap();
        assert_eq!(ports_of(&result), [8080]);
        let result = assign(&[request("web", 80, 80)], &[], &[], &[8080], &[80]).unwrap();
        assert_eq!(ports_of(&result), [8081]);
    }

    #[test]
    fn test_no_duplicates_inside_the_project() {
        let requests = [
            request("web", 80, 80),
            request("alt", 81, 80),
            request("db", 3306, 3306),
            request("db-two", 3307, 3306),
        ];
        let result = assign(&requests, &[], &[], &[], &[]).unwrap();
        assert_eq!(ports_of(&result), [80, 8080, 3306, 3307]);
        let unique: HashSet<u16> = ports_of(&result).into_iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn test_search_does_not_go_past_the_last_port() {
        let result = assign(&[request("web", 8000, 65535)], &[], &[], &[], &[65535]);
        assert_eq!(
            result,
            Err(PortConflict {
                key: key("web", 8000),
                port: 65535
            })
        );
    }

    #[test]
    fn test_pin_is_respected_and_wins_over_automatic_ports() {
        let requests = [request("web", 80, 80), request("db", 3306, 3306)];
        let result = assign(&requests, &[], &[("db:3306/tcp", 80)], &[], &[]).unwrap();
        assert_eq!(ports_of(&result), [8080, 80]);
        assert!(result[1].pinned);
        assert!(!result[0].pinned);
    }

    #[test]
    fn test_pin_in_conflict_is_an_error() {
        let requests = [request("web", 80, 80)];
        let pins = [("web:80/tcp", 9000)];
        for (reserved, busy) in [(vec![9000], vec![]), (vec![], vec![9000])] {
            let result = assign(&requests, &[], &pins, &reserved, &busy);
            assert_eq!(
                result,
                Err(PortConflict {
                    key: key("web", 80),
                    port: 9000
                })
            );
        }
        assert_eq!(
            assign(&requests, &[], &pins, &[9000], &[])
                .unwrap_err()
                .to_string(),
            "Port 9000 for web is in use"
        );
    }

    #[test]
    fn test_two_pins_on_the_same_port_conflict() {
        let requests = [request("web", 80, 80), request("db", 3306, 3306)];
        let result = assign(
            &requests,
            &[],
            &[("web:80/tcp", 9000), ("db:3306/tcp", 9000)],
            &[],
            &[],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_own_running_ports_count_as_free_when_the_caller_says_so() {
        let own: HashSet<u16> = [80, 3306].into_iter().collect();
        let busy: HashSet<u16> = [80, 3306, 6379].into_iter().collect();
        let requests = [
            request("web", 80, 80),
            request("db", 3306, 3306),
            request("cache", 6379, 6379),
        ];
        let result = assign_ports(
            &requests,
            &[],
            &HashMap::default(),
            &HashSet::default(),
            &|port| own.contains(&port) || !busy.contains(&port),
        )
        .unwrap();
        assert_eq!(ports_of(&result), [80, 3306, 6380]);
    }

    fn config_ports(entries: &[(&str, u16, Option<&str>, &str, Option<&str>)]) -> ComposeConfig {
        let mut config = ComposeConfig::default();
        for (service, target, published, protocol, host_ip) in entries {
            config
                .services
                .entry(service.to_string())
                .or_default()
                .ports
                .push(PortConfig {
                    host_ip: host_ip.map(str::to_string),
                    target: *target,
                    published: published.map(str::to_string),
                    protocol: protocol.to_string(),
                });
        }
        config
    }

    fn ports_by_service(config: &ComposeConfig) -> BTreeMap<String, Vec<PortConfig>> {
        config
            .services
            .iter()
            .map(|(name, service)| (name.clone(), service.ports.clone()))
            .collect()
    }

    #[test]
    fn test_requests_ignore_unpublished_and_range_ports() {
        let config = config_ports(&[
            ("web", 80, Some("8080"), "tcp", None),
            ("web", 443, None, "tcp", None),
            ("web", 9000, Some("9000-9010"), "tcp", None),
            ("dns", 53, Some("5353"), "udp", Some("127.0.0.1")),
        ]);
        let requests = requests_from_config(&config);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].key.service, "dns");
        assert_eq!(requests[0].key.protocol, "udp");
        assert_eq!(requests[0].host_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(requests[1].key.pin_key(), "web:80/tcp");
        assert_eq!(requests[1].original_host_port, 8080);
    }

    #[test]
    fn test_override_yaml_exact_format() {
        let config = config_ports(&[
            ("laravel.test", 80, Some("80"), "tcp", None),
            ("laravel.test", 5173, Some("5173"), "tcp", None),
            ("mysql", 3306, Some("3306"), "tcp", None),
        ]);
        let assignments = [
            assignment("laravel.test", 80, 80, 8081, false),
            assignment("laravel.test", 5173, 5173, 5173, false),
            assignment("mysql", 3306, 3306, 3306, false),
        ];
        let yaml = override_yaml(&assignments, &ports_by_service(&config))
            .unwrap()
            .unwrap();
        assert_eq!(
            yaml,
            "services:\n  laravel.test:\n    ports: !override\n      - \"8081:80\"\n      - \"5173:5173\"\n"
        );
    }

    #[test]
    fn test_override_yaml_keeps_host_ip_udp_and_unpublished_ports() {
        let config = config_ports(&[
            ("dns", 53, Some("5353"), "udp", Some("127.0.0.1")),
            ("dns", 54, Some("5454"), "tcp", Some("::1")),
            ("dns", 443, None, "tcp", None),
            ("dns", 9000, Some("9000-9010"), "tcp", None),
        ]);
        let assignments = [
            PortAssignment {
                key: PortKey {
                    service: "dns".to_string(),
                    container_port: 53,
                    protocol: "udp".to_string(),
                },
                host_ip: Some("127.0.0.1".to_string()),
                host_port: 5354,
                original_host_port: 5353,
                pinned: false,
            },
            PortAssignment {
                key: key("dns", 54),
                host_ip: Some("::1".to_string()),
                host_port: 5454,
                original_host_port: 5454,
                pinned: false,
            },
        ];
        let yaml = override_yaml(&assignments, &ports_by_service(&config))
            .unwrap()
            .unwrap();
        assert_eq!(
            yaml,
            "services:\n  dns:\n    ports: !override\n      - \"127.0.0.1:5354:53/udp\"\n      - \"[::1]:5454:54\"\n      - \"443\"\n      - \"9000-9010:9000\"\n"
        );
    }

    #[test]
    fn test_override_yaml_is_none_when_nothing_changes() {
        let config = config_ports(&[("web", 80, Some("80"), "tcp", None)]);
        let assignments = [assignment("web", 80, 80, 80, false)];
        assert_eq!(
            override_yaml(&assignments, &ports_by_service(&config)).unwrap(),
            None
        );
    }

    #[test]
    fn test_override_yaml_only_lists_changed_services() {
        let config = config_ports(&[
            ("web", 80, Some("80"), "tcp", None),
            ("db", 3306, Some("3306"), "tcp", None),
        ]);
        let assignments = [
            assignment("web", 80, 80, 8080, false),
            assignment("db", 3306, 3306, 3306, false),
        ];
        let yaml = override_yaml(&assignments, &ports_by_service(&config))
            .unwrap()
            .unwrap();
        assert!(yaml.contains("web:"));
        assert!(!yaml.contains("db:"));
    }

    #[test]
    fn test_override_yaml_rejects_invalid_service_names() {
        let config = config_ports(&[("bad\nname", 80, Some("80"), "tcp", None)]);
        let assignments = [assignment("bad\nname", 80, 80, 8080, false)];
        assert!(override_yaml(&assignments, &ports_by_service(&config)).is_err());
    }

    #[test]
    fn test_parse_docker_ps_ports() {
        let output = concat!(
            r#"{"Labels":"com.docker.compose.project=blog,other=x","Ports":"0.0.0.0:8080->80/tcp, :::8080->80/tcp, 3306/tcp"}"#,
            "\n",
            r#"{"Labels":"","Ports":"127.0.0.1:5432->5432/tcp"}"#,
            "\n",
            r#"{"Labels":"com.docker.compose.project=shop","Ports":"0.0.0.0:8000-8001->8000-8001/tcp"}"#,
            "\n",
            r#"{"Labels":"a=b","Ports":"3306/tcp, 33060/tcp"}"#,
            "\n",
            r#"{"Labels":"com.docker.compose.project=v6","Ports":"[::]:9090->9090/tcp"}"#,
            "\n\n",
        );
        let parsed = parse_docker_ps_ports(output).unwrap();
        assert_eq!(
            parsed,
            vec![
                (Some("blog".to_string()), 8080),
                (None, 5432),
                (Some("shop".to_string()), 8000),
                (Some("shop".to_string()), 8001),
                (Some("v6".to_string()), 9090),
            ]
        );
    }

    #[test]
    fn test_parse_docker_ps_ports_empty_and_invalid() {
        assert!(parse_docker_ps_ports("").unwrap().is_empty());
        assert!(parse_docker_ps_ports("not json").is_err());
        assert!(
            parse_docker_ps_ports(r#"{"Names":"x"}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_compose_supports_port_override() {
        for supported in [
            "2.24.4",
            "v2.24.4",
            "2.29.1",
            "2.24.4-desktop.1",
            "3.0.0",
            "5.6.0",
            "v5.6.0",
            "2.30",
        ] {
            assert!(compose_supports_port_override(supported), "{supported}");
        }
        for unsupported in ["2.24.3", "2.23.9", "1.29.2", "", "garbage", "2.24"] {
            assert!(
                !compose_supports_port_override(unsupported),
                "{unsupported}"
            );
        }
    }

    #[test]
    fn test_moved_ports_message() {
        let assignments = [
            assignment("laravel.test", 80, 80, 8080, false),
            assignment("mysql", 3306, 3306, 3306, false),
        ];
        assert_eq!(
            moved_ports_message(&assignments).as_deref(),
            Some("laravel.test: 80 \u{2192} 8080")
        );
        assert_eq!(moved_ports_message(&assignments[1..]), None);
    }

    #[test]
    fn test_override_file_path_is_stable_and_per_root() {
        let data = Path::new("/data");
        let first = override_file_path(data, Path::new("/work/blog"));
        assert_eq!(first, override_file_path(data, Path::new("/work/blog")));
        assert_ne!(first, override_file_path(data, Path::new("/work/shop")));
        assert!(first.starts_with("/data/sail_manager"));
        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("ports.override.yaml")
        );
        assert_eq!(stable_hash("/work/blog"), stable_hash("/work/blog"));
    }

    #[test]
    fn test_reserved_by_other_roots_skips_the_current_one() {
        let mut registry = AssignmentRegistry::new();
        registry.insert(
            "/a".to_string(),
            vec![assignment("web", 80, 80, 8080, false)],
        );
        registry.insert(
            "/b".to_string(),
            vec![assignment("web", 80, 80, 8081, false)],
        );
        let reserved = reserved_by_other_roots(&registry, "/a");
        assert_eq!(reserved, [8081].into_iter().collect());
    }

    #[test]
    fn test_registry_roundtrips_through_json() {
        let mut registry = AssignmentRegistry::new();
        registry.insert(
            "/work/blog".to_string(),
            vec![assignment("web", 80, 80, 8080, true)],
        );
        let json = serde_json::to_string(&registry).unwrap();
        assert!(json.contains("\"/work/blog\""));
        assert!(json.contains("\"host_port\":8080"));
        let parsed: AssignmentRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, registry);
    }

    #[test]
    fn test_in_memory_store_roundtrip() {
        let store = InMemoryAssignmentStore::new();
        let mut registry = AssignmentRegistry::new();
        registry.insert(
            "/a".to_string(),
            vec![assignment("web", 80, 80, 8080, false)],
        );
        futures::executor::block_on(store.save(registry.clone())).unwrap();
        assert_eq!(futures::executor::block_on(store.load()).unwrap(), registry);
    }
}
