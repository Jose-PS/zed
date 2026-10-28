use anyhow::{Context as _, Result, anyhow, bail, ensure};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const COMPOSE_FILE_NAMES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

pub const COMPOSE_OVERRIDE_FILE_NAMES: [&str; 4] = [
    "compose.override.yaml",
    "compose.override.yml",
    "docker-compose.override.yaml",
    "docker-compose.override.yml",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeFiles {
    pub primary: String,
    pub overrides: Vec<String>,
}

impl ComposeFiles {
    pub fn all(&self) -> Vec<String> {
        std::iter::once(self.primary.clone())
            .chain(self.overrides.iter().cloned())
            .collect()
    }
}

pub fn detect_compose_files(root_file_names: &[&str]) -> Option<ComposeFiles> {
    let primary = COMPOSE_FILE_NAMES
        .iter()
        .find(|name| root_file_names.contains(name))?;
    let override_prefix = if primary.starts_with("docker-compose") {
        "docker-compose.override."
    } else {
        "compose.override."
    };
    let overrides = COMPOSE_OVERRIDE_FILE_NAMES
        .iter()
        .filter(|name| name.starts_with(override_prefix) && root_file_names.contains(name))
        .map(|name| name.to_string())
        .collect();
    Some(ComposeFiles {
        primary: primary.to_string(),
        overrides,
    })
}

pub fn is_sail_project(composer_json: Option<&str>, has_vendor_sail_binary: bool) -> bool {
    if has_vendor_sail_binary {
        return true;
    }
    let Some(composer_json) = composer_json else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(composer_json) else {
        return false;
    };
    ["require", "require-dev"].iter().any(|section| {
        manifest
            .get(section)
            .and_then(|dependencies| dependencies.as_object())
            .is_some_and(|dependencies| dependencies.contains_key("laravel/sail"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodePackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl NodePackageManager {
    pub fn program(&self) -> &'static str {
        match self {
            NodePackageManager::Npm => "npm",
            NodePackageManager::Pnpm => "pnpm",
            NodePackageManager::Yarn => "yarn",
            NodePackageManager::Bun => "bun",
        }
    }

    pub fn run_script_args(&self, script: &str) -> Vec<String> {
        match self {
            NodePackageManager::Yarn => vec![script.to_string()],
            NodePackageManager::Npm | NodePackageManager::Pnpm | NodePackageManager::Bun => {
                vec!["run".to_string(), script.to_string()]
            }
        }
    }

    pub fn install_args(&self) -> Vec<String> {
        vec!["install".to_string()]
    }
}

pub fn detect_node_package_manager(root_file_names: &[&str]) -> Option<NodePackageManager> {
    let has = |name: &str| root_file_names.contains(&name);
    if has("pnpm-lock.yaml") {
        Some(NodePackageManager::Pnpm)
    } else if has("yarn.lock") {
        Some(NodePackageManager::Yarn)
    } else if has("bun.lock") || has("bun.lockb") {
        Some(NodePackageManager::Bun)
    } else if has("package-lock.json") || has("package.json") {
        Some(NodePackageManager::Npm)
    } else {
        None
    }
}

fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrNumber {
    Text(String),
    Number(u64),
}

fn optional_string_or_number<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        Option::<StringOrNumber>::deserialize(deserializer)?.map(|value| match value {
            StringOrNumber::Text(text) => text,
            StringOrNumber::Number(number) => number.to_string(),
        }),
    )
}

fn default_protocol() -> String {
    "tcp".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PortConfig {
    #[serde(default)]
    pub host_ip: Option<String>,
    pub target: u16,
    #[serde(default, deserialize_with = "optional_string_or_number")]
    pub published: Option<String>,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

impl PortConfig {
    pub fn published_port(&self) -> Option<u16> {
        self.published.as_deref()?.trim().parse().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct ServiceConfig {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub container_name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub ports: Vec<PortConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct ComposeConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub services: BTreeMap<String, ServiceConfig>,
}

pub fn parse_compose_config(json: &str) -> Result<ComposeConfig> {
    serde_json::from_str(json).context("parsing `docker compose config` output")
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Publisher {
    #[serde(rename = "URL", default)]
    pub url: Option<String>,
    #[serde(rename = "TargetPort", default)]
    pub target_port: u16,
    #[serde(rename = "PublishedPort", default)]
    pub published_port: u16,
    #[serde(rename = "Protocol", default = "default_protocol")]
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ContainerEntry {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Service", default)]
    pub service: String,
    #[serde(rename = "State", default)]
    pub state: String,
    #[serde(rename = "Status", default)]
    pub status: String,
    #[serde(rename = "Health", default)]
    pub health: Option<String>,
    #[serde(rename = "ExitCode", default)]
    pub exit_code: i64,
    #[serde(rename = "Image", default)]
    pub image: Option<String>,
    #[serde(rename = "Publishers", default, deserialize_with = "null_as_default")]
    pub publishers: Vec<Publisher>,
}

pub fn parse_compose_ps(output: &str) -> Result<Vec<ContainerEntry>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).context("parsing `docker compose ps` JSON array");
    }
    trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).context("parsing `docker compose ps` JSON line"))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Unhealthy,
    Starting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    Running { health: Option<Health> },
    Starting,
    Restarting,
    Paused,
    Exited { exit_code: i64 },
    Created,
    NotCreated,
    Unknown(String),
}

impl ServiceState {
    pub fn is_running(&self) -> bool {
        matches!(self, ServiceState::Running { .. })
    }

    fn from_container(container: &ContainerEntry) -> Self {
        match container.state.to_lowercase().as_str() {
            "running" => {
                let health = match container
                    .health
                    .as_deref()
                    .map(|health| health.to_lowercase())
                    .as_deref()
                {
                    Some("healthy") => Some(Health::Healthy),
                    Some("unhealthy") => Some(Health::Unhealthy),
                    Some("starting") => Some(Health::Starting),
                    _ => None,
                };
                ServiceState::Running { health }
            }
            "restarting" => ServiceState::Restarting,
            "paused" => ServiceState::Paused,
            "exited" => ServiceState::Exited {
                exit_code: container.exit_code,
            },
            "created" => ServiceState::Created,
            _ => ServiceState::Unknown(container.state.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    pub host_ip: Option<String>,
    pub host_port: Option<u16>,
    pub container_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    App,
    Mysql,
    Mariadb,
    Postgres,
    Redis,
    Mailpit,
    PhpMyAdmin,
    Other,
}

impl ServiceKind {
    pub fn default_client_command(&self) -> Option<Vec<&'static str>> {
        match self {
            ServiceKind::Mysql => Some(vec!["mysql"]),
            ServiceKind::Mariadb => Some(vec!["mariadb"]),
            ServiceKind::Postgres => Some(vec!["psql"]),
            ServiceKind::Redis => Some(vec!["redis-cli"]),
            _ => None,
        }
    }
}

pub fn classify_service(service_name: &str, image: Option<&str>, app_service: &str) -> ServiceKind {
    let normalized_image = image.map(|image| image.trim().to_lowercase());
    if service_name == app_service
        || normalized_image
            .as_deref()
            .is_some_and(|image| image.starts_with("sail-"))
    {
        return ServiceKind::App;
    }
    let Some(normalized_image) = normalized_image else {
        return ServiceKind::Other;
    };
    let without_digest = normalized_image
        .split('@')
        .next()
        .unwrap_or(&normalized_image);
    let repository = without_digest.rsplit('/').next().unwrap_or(without_digest);
    let repository = repository.split(':').next().unwrap_or(repository);
    match repository {
        "mysql" | "mysql-server" => ServiceKind::Mysql,
        "mariadb" => ServiceKind::Mariadb,
        "postgres" | "postgis" => ServiceKind::Postgres,
        "redis" | "valkey" => ServiceKind::Redis,
        "mailpit" => ServiceKind::Mailpit,
        "phpmyadmin" => ServiceKind::PhpMyAdmin,
        _ => ServiceKind::Other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub image: Option<String>,
    pub kind: ServiceKind,
    pub state: ServiceState,
    pub status_text: Option<String>,
    pub container_name: Option<String>,
    pub ports: Vec<PortMapping>,
}

fn ports_from_config(ports: &[PortConfig]) -> Vec<PortMapping> {
    ports
        .iter()
        .map(|port| PortMapping {
            host_ip: port.host_ip.clone(),
            host_port: port.published_port(),
            container_port: port.target,
            protocol: port.protocol.clone(),
        })
        .collect()
}

fn ports_from_publishers(publishers: &[Publisher]) -> Vec<PortMapping> {
    let mut ports: Vec<PortMapping> = Vec::new();
    for publisher in publishers {
        if publisher.published_port == 0 {
            continue;
        }
        let already_listed = ports.iter().any(|port| {
            port.host_port == Some(publisher.published_port)
                && port.container_port == publisher.target_port
                && port.protocol == publisher.protocol
        });
        if already_listed {
            continue;
        }
        ports.push(PortMapping {
            host_ip: publisher.url.clone().filter(|url| !url.is_empty()),
            host_port: Some(publisher.published_port),
            container_port: publisher.target_port,
            protocol: publisher.protocol.clone(),
        });
    }
    ports
}

pub fn build_services(
    config: &ComposeConfig,
    containers: &[ContainerEntry],
    app_service: &str,
) -> Vec<Service> {
    config
        .services
        .iter()
        .map(|(name, service_config)| {
            let service_containers: Vec<&ContainerEntry> = containers
                .iter()
                .filter(|container| &container.service == name)
                .collect();
            let container = service_containers
                .iter()
                .find(|container| container.state.eq_ignore_ascii_case("running"))
                .or_else(|| service_containers.first())
                .copied();

            let image = container
                .and_then(|container| container.image.clone().filter(|image| !image.is_empty()))
                .or_else(|| service_config.image.clone());
            let kind = classify_service(name, image.as_deref(), app_service);

            let Some(container) = container else {
                return Service {
                    name: name.clone(),
                    image,
                    kind,
                    state: ServiceState::NotCreated,
                    status_text: None,
                    container_name: service_config.container_name.clone(),
                    ports: ports_from_config(&service_config.ports),
                };
            };

            let state = ServiceState::from_container(container);
            let publisher_ports = if state.is_running() {
                ports_from_publishers(&container.publishers)
            } else {
                Vec::new()
            };
            let ports = if publisher_ports.is_empty() {
                ports_from_config(&service_config.ports)
            } else {
                publisher_ports
            };
            Service {
                name: name.clone(),
                image,
                kind,
                state,
                status_text: Some(container.status.clone()).filter(|status| !status.is_empty()),
                container_name: Some(container.name.clone()).filter(|name| !name.is_empty()),
                ports,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentState {
    Running,
    Partial,
    Stopped,
    NoServices,
}

pub fn environment_state(services: &[Service]) -> EnvironmentState {
    if services.is_empty() {
        return EnvironmentState::NoServices;
    }
    let running = services
        .iter()
        .filter(|service| service.state.is_running())
        .count();
    if running == services.len() {
        EnvironmentState::Running
    } else if running == 0 {
        EnvironmentState::Stopped
    } else {
        EnvironmentState::Partial
    }
}

fn http_url(scheme: &str, host: &str, port: u16, default_port: u16) -> String {
    if port == default_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    }
}

pub fn web_urls(service: &Service, host: &str, manual_url: Option<&str>) -> Vec<String> {
    if let Some(manual_url) = manual_url {
        return vec![manual_url.to_string()];
    }
    let mut urls: Vec<String> = Vec::new();
    for port in &service.ports {
        let Some(host_port) = port.host_port else {
            continue;
        };
        let url = match (service.kind, port.container_port) {
            (ServiceKind::App, 80) | (ServiceKind::PhpMyAdmin, 80) => {
                http_url("http", host, host_port, 80)
            }
            (ServiceKind::App, 443) => http_url("https", host, host_port, 443),
            (ServiceKind::App, 5173) => format!("http://{host}:{host_port}"),
            (ServiceKind::Mailpit, 8025) => http_url("http", host, host_port, 80),
            _ => continue,
        };
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

pub fn validate_manual_url(input: &str) -> Result<Option<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .ok_or_else(|| anyhow!("The URL must start with http:// or https://"))?;
    ensure!(
        !rest.is_empty() && !rest.starts_with('/') && !rest.chars().any(char::is_whitespace),
        "The URL is not valid"
    );
    Ok(Some(trimmed.to_string()))
}

pub fn validate_service_name(name: &str) -> Result<()> {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        bail!("name is empty");
    };
    ensure!(
        first.is_ascii_alphanumeric(),
        "name {name:?} must start with a letter or digit"
    );
    ensure!(
        characters
            .all(|character| character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-')),
        "name {name:?} contains invalid characters"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceOperation {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeLauncher {
    DockerCompose { docker_binary: PathBuf },
    Sail { sail_script: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeInvocation {
    pub launcher: ComposeLauncher,
    pub project_directory: PathBuf,
    pub files: Vec<String>,
    pub generated_override: Option<PathBuf>,
}

impl ComposeInvocation {
    pub fn effective_files(&self) -> Vec<String> {
        self.files
            .iter()
            .cloned()
            .chain(
                self.generated_override
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned()),
            )
            .collect()
    }

    pub fn without_generated_override(&self) -> Self {
        Self {
            generated_override: None,
            ..self.clone()
        }
    }

    fn command(&self, extra: &[&str]) -> ComposeCommand {
        let extra = extra.iter().map(|argument| argument.to_string());
        match &self.launcher {
            ComposeLauncher::DockerCompose { docker_binary } => {
                let mut args = vec![
                    "compose".to_string(),
                    "--project-directory".to_string(),
                    self.project_directory.to_string_lossy().into_owned(),
                ];
                for file in self.effective_files() {
                    args.push("-f".to_string());
                    args.push(file);
                }
                args.extend(extra);
                ComposeCommand {
                    program: docker_binary.clone(),
                    args,
                    environment: Vec::new(),
                }
            }
            ComposeLauncher::Sail { sail_script } => {
                let mut environment = vec![("SAIL_SKIP_CHECKS".to_string(), "1".to_string())];
                let files = self.effective_files();
                if !files.is_empty() {
                    environment.push(("SAIL_FILES".to_string(), files.join(":")));
                }
                ComposeCommand {
                    program: sail_script.clone(),
                    args: extra.collect(),
                    environment,
                }
            }
        }
    }

    pub fn config_command(&self) -> ComposeCommand {
        self.command(&["config", "--format", "json"])
    }

    pub fn ps_command(&self) -> ComposeCommand {
        self.command(&["ps", "-a", "--format", "json"])
    }

    pub fn up_command(&self) -> ComposeCommand {
        self.command(&["up", "-d"])
    }

    pub fn stop_command(&self) -> ComposeCommand {
        self.command(&["stop"])
    }

    pub fn restart_command(&self) -> ComposeCommand {
        self.command(&["restart"])
    }

    pub fn service_command(
        &self,
        operation: ServiceOperation,
        service: &str,
    ) -> Result<ComposeCommand> {
        validate_service_name(service)?;
        let mut command = match operation {
            ServiceOperation::Start => self.command(&["up", "-d"]),
            ServiceOperation::Stop => self.command(&["stop"]),
            ServiceOperation::Restart => self.command(&["restart"]),
        };
        command.args.push(service.to_string());
        Ok(command)
    }

    pub fn logs_command(&self, service: &str, follow: bool, tail: u32) -> Result<ComposeCommand> {
        validate_service_name(service)?;
        let mut command = self.command(&["logs", "--tail"]);
        command.args.push(tail.to_string());
        if follow {
            command.args.push("-f".to_string());
        }
        command.args.push(service.to_string());
        Ok(command)
    }

    pub fn events_command(&self) -> ComposeCommand {
        self.command(&["events", "--json"])
    }

    pub fn exec_command(
        &self,
        service: &str,
        user: Option<&str>,
        workdir: Option<&str>,
        no_tty: bool,
        command: &[String],
    ) -> Result<ComposeCommand> {
        validate_service_name(service)?;
        let mut compose_command = self.command(&["exec"]);
        if no_tty {
            compose_command.args.push("-T".to_string());
        }
        if let Some(user) = user {
            validate_service_name(user)?;
            compose_command.args.push("-u".to_string());
            compose_command.args.push(user.to_string());
        }
        if let Some(workdir) = workdir {
            compose_command.args.push("-w".to_string());
            compose_command.args.push(workdir.to_string());
        }
        compose_command.args.push(service.to_string());
        compose_command.args.extend(command.iter().cloned());
        Ok(compose_command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalTarget {
    Shell { shell: String, root: bool },
    Client,
}

fn sail_client_shortcut(service: &Service) -> Option<&'static str> {
    match (service.name.as_str(), service.kind) {
        ("mysql", ServiceKind::Mysql) => Some("mysql"),
        ("mariadb", ServiceKind::Mariadb) => Some("mariadb"),
        ("pgsql", ServiceKind::Postgres) => Some("psql"),
        ("redis", ServiceKind::Redis) => Some("redis"),
        ("valkey", ServiceKind::Redis) => Some("valkey"),
        _ => None,
    }
}

pub fn terminal_command(
    invocation: &ComposeInvocation,
    service: &Service,
    is_sail: bool,
    app_service: &str,
    target: &TerminalTarget,
) -> Result<ComposeCommand> {
    validate_service_name(&service.name)?;
    match target {
        TerminalTarget::Shell { shell, root } => {
            validate_service_name(shell)?;
            if is_sail && service.name == app_service {
                let user = if *root { "root" } else { "sail" };
                return invocation.exec_command(
                    &service.name,
                    Some(user),
                    None,
                    false,
                    std::slice::from_ref(shell),
                );
            }
            invocation.exec_command(
                &service.name,
                None,
                None,
                false,
                std::slice::from_ref(shell),
            )
        }
        TerminalTarget::Client => {
            if is_sail && let Some(shortcut) = sail_client_shortcut(service) {
                return Ok(invocation.command(&[shortcut]));
            }
            let client = service
                .kind
                .default_client_command()
                .ok_or_else(|| anyhow!("{} has no command line client", service.name))?;
            let client = client.into_iter().map(String::from).collect::<Vec<_>>();
            invocation.exec_command(&service.name, None, None, false, &client)
        }
    }
}

pub fn parse_available_shells(output: &str) -> Vec<&'static str> {
    let found: Vec<&str> = output
        .lines()
        .filter_map(|line| line.trim().rsplit('/').next())
        .collect();
    ["zsh", "bash", "sh"]
        .into_iter()
        .filter(|shell| found.contains(shell))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPreference {
    Auto,
    Zsh,
    Bash,
    Sh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    EventsAndPoll,
    Events,
    Manual,
}

impl RefreshMode {
    pub fn listens_to_events(self) -> bool {
        !matches!(self, RefreshMode::Manual)
    }

    pub fn polls(self) -> bool {
        matches!(self, RefreshMode::EventsAndPoll)
    }
}

pub fn choose_shell(preference: ShellPreference, available: &[&str]) -> Option<&'static str> {
    let candidates: &[&'static str] = match preference {
        ShellPreference::Zsh => &["zsh", "bash", "sh"],
        ShellPreference::Bash | ShellPreference::Auto => &["bash", "sh"],
        ShellPreference::Sh => &["sh"],
    };
    candidates
        .iter()
        .find(|candidate| available.contains(candidate))
        .copied()
}

pub fn app_exec_user(kind: &ServiceKind, is_sail: bool) -> Option<&'static str> {
    (matches!(kind, ServiceKind::App) && is_sail).then_some("sail")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAction {
    Tinker,
    Migrate,
    MigrateSeed,
    MigrateStatus,
    RouteList,
    OptimizeClear,
    Test,
    ComposerInstall,
    ComposerUpdate,
    NodeInstall,
    NodeDev,
    NodeBuild,
}

impl QuickAction {
    pub fn label(&self, package_manager: NodePackageManager) -> String {
        let program = package_manager.program();
        match self {
            QuickAction::Tinker => "Tinker".to_string(),
            QuickAction::Migrate => "Migrate".to_string(),
            QuickAction::MigrateSeed => "Migrate and Seed".to_string(),
            QuickAction::MigrateStatus => "Migration Status".to_string(),
            QuickAction::RouteList => "Route List".to_string(),
            QuickAction::OptimizeClear => "Clear Caches".to_string(),
            QuickAction::Test => "Run Tests".to_string(),
            QuickAction::ComposerInstall => "composer install".to_string(),
            QuickAction::ComposerUpdate => "composer update".to_string(),
            QuickAction::NodeInstall => format!("{program} install"),
            QuickAction::NodeDev => format!("{program} run dev"),
            QuickAction::NodeBuild => format!("{program} run build"),
        }
    }

    fn program_args(&self, package_manager: NodePackageManager) -> Vec<String> {
        let artisan = |arguments: &[&str]| {
            std::iter::once("artisan")
                .chain(arguments.iter().copied())
                .map(String::from)
                .collect::<Vec<_>>()
        };
        let composer = |argument: &str| vec!["composer".to_string(), argument.to_string()];
        let node = |arguments: Vec<String>| {
            std::iter::once(package_manager.program().to_string())
                .chain(arguments)
                .collect::<Vec<_>>()
        };
        match self {
            QuickAction::Tinker => artisan(&["tinker"]),
            QuickAction::Migrate => artisan(&["migrate"]),
            QuickAction::MigrateSeed => artisan(&["migrate", "--seed"]),
            QuickAction::MigrateStatus => artisan(&["migrate:status"]),
            QuickAction::RouteList => artisan(&["route:list"]),
            QuickAction::OptimizeClear => artisan(&["optimize:clear"]),
            QuickAction::Test => artisan(&["test"]),
            QuickAction::ComposerInstall => composer("install"),
            QuickAction::ComposerUpdate => composer("update"),
            QuickAction::NodeInstall => node(package_manager.install_args()),
            QuickAction::NodeDev => node(package_manager.run_script_args("dev")),
            QuickAction::NodeBuild => node(package_manager.run_script_args("build")),
        }
    }

    pub fn display_command(&self, package_manager: NodePackageManager) -> String {
        let arguments = self.program_args(package_manager);
        match arguments.first().map(String::as_str) {
            Some("artisan") => format!("php {}", arguments.join(" ")),
            _ => arguments.join(" "),
        }
    }

    pub fn requires_confirmation(&self) -> bool {
        matches!(self, QuickAction::MigrateSeed | QuickAction::ComposerUpdate)
    }
}

pub fn quick_action_command(
    invocation: &ComposeInvocation,
    is_sail: bool,
    app_service: &str,
    action: QuickAction,
    package_manager: NodePackageManager,
) -> Result<ComposeCommand> {
    let arguments = action.program_args(package_manager);
    program_command(invocation, is_sail, app_service, arguments)
}

pub fn composer_custom_command(
    invocation: &ComposeInvocation,
    is_sail: bool,
    app_service: &str,
    composer_arguments: &[String],
) -> Result<ComposeCommand> {
    ensure!(!composer_arguments.is_empty(), "the command is empty");
    let arguments = std::iter::once("composer".to_string())
        .chain(composer_arguments.iter().cloned())
        .collect::<Vec<_>>();
    program_command(invocation, is_sail, app_service, arguments)
}

fn program_command(
    invocation: &ComposeInvocation,
    is_sail: bool,
    app_service: &str,
    arguments: Vec<String>,
) -> Result<ComposeCommand> {
    if is_sail {
        let mut command = invocation.command(&[]);
        command.args = arguments;
        return Ok(command);
    }
    let mut container_command = Vec::with_capacity(arguments.len() + 1);
    if arguments.first().map(String::as_str) == Some("artisan") {
        container_command.push("php".to_string());
    }
    container_command.extend(arguments);
    invocation.exec_command(app_service, None, None, false, &container_command)
}

pub fn split_command_line(input: &str) -> Result<Vec<String>> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        match character {
            '\'' => {
                in_word = true;
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => bail!("unclosed quote in the command"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match characters.next() {
                        Some('"') => break,
                        Some('\\') => match characters.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => bail!("unclosed quote in the command"),
                        },
                        Some(inner) => current.push(inner),
                        None => bail!("unclosed quote in the command"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match characters.next() {
                    Some(escaped) => current.push(escaped),
                    None => bail!("the command ends with a backslash"),
                }
            }
            character if character.is_whitespace() => {
                if in_word {
                    arguments.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            character => {
                in_word = true;
                current.push(character);
            }
        }
    }
    if in_word {
        arguments.push(current);
    }
    ensure!(!arguments.is_empty(), "the command is empty");
    Ok(arguments)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomActionMode {
    Terminal,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAction {
    pub name: String,
    pub service: Option<String>,
    pub command: Vec<String>,
    pub working_directory: Option<String>,
    pub mode: CustomActionMode,
    pub confirm: bool,
}

impl CustomAction {
    pub fn validated(
        name: &str,
        service: Option<&str>,
        command: Vec<String>,
        working_directory: Option<&str>,
        mode: CustomActionMode,
        confirm: bool,
    ) -> Result<Self> {
        let name = name.trim();
        ensure!(!name.is_empty(), "the name is empty");
        ensure!(
            command.first().is_some_and(|program| !program.is_empty()),
            "the command is empty"
        );
        let service = match service.map(str::trim).filter(|service| !service.is_empty()) {
            Some(service) => {
                validate_service_name(service)?;
                Some(service.to_string())
            }
            None => None,
        };
        let working_directory = working_directory
            .map(str::trim)
            .filter(|directory| !directory.is_empty())
            .map(str::to_string);
        Ok(Self {
            name: name.to_string(),
            service,
            command,
            working_directory,
            mode,
            confirm,
        })
    }
}

pub fn custom_action_command(
    invocation: &ComposeInvocation,
    is_sail: bool,
    app_service: &str,
    action: &CustomAction,
) -> Result<ComposeCommand> {
    let service = action.service.as_deref().unwrap_or(app_service);
    validate_service_name(service)?;
    let user = (is_sail && service == app_service).then_some("sail");
    invocation.exec_command(
        service,
        user,
        action.working_directory.as_deref(),
        false,
        &action.command,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAIL_CONFIG: &str = r#"{
        "name": "blog",
        "services": {
            "laravel.test": {
                "image": "sail-8.4/app",
                "environment": {"DB_PASSWORD": "super-secret-password", "APP_KEY": "base64:topsecret"},
                "env_file": [{"path": "/home/x/.env"}],
                "labels": {"secret.label": "label-secret"},
                "secrets": {"token": {"file": "/secret"}},
                "ports": [
                    {"mode": "ingress", "host_ip": "0.0.0.0", "target": 80, "published": "80", "protocol": "tcp"},
                    {"mode": "ingress", "target": 5173, "published": 5173, "protocol": "tcp"}
                ]
            },
            "mysql": {
                "image": "mysql/mysql-server:8.0",
                "container_name": "blog-mysql",
                "ports": [{"mode": "ingress", "target": 3306, "published": "3306", "protocol": "tcp"}]
            },
            "redis": {
                "image": "redis:alpine",
                "ports": [{"mode": "ingress", "target": 6379, "published": "6379", "protocol": "tcp"}]
            },
            "mailpit": {
                "image": "axllent/mailpit:latest",
                "ports": [
                    {"mode": "ingress", "target": 1025, "published": "1025", "protocol": "tcp"},
                    {"mode": "ingress", "target": 8025, "published": "8025", "protocol": "tcp"}
                ]
            },
            "meilisearch": {
                "image": "getmeili/meilisearch:latest",
                "ports": [{"mode": "ingress", "target": 7700, "published": "7700", "protocol": "tcp"}]
            }
        }
    }"#;

    fn invocation(files: &[&str]) -> ComposeInvocation {
        ComposeInvocation {
            launcher: ComposeLauncher::DockerCompose {
                docker_binary: PathBuf::from("/usr/bin/docker"),
            },
            project_directory: PathBuf::from("/work/blog"),
            files: files.iter().map(|file| file.to_string()).collect(),
            generated_override: None,
        }
    }

    fn sail_invocation(files: &[&str]) -> ComposeInvocation {
        ComposeInvocation {
            launcher: ComposeLauncher::Sail {
                sail_script: PathBuf::from("/work/blog/vendor/bin/sail"),
            },
            project_directory: PathBuf::from("/work/blog"),
            files: files.iter().map(|file| file.to_string()).collect(),
            generated_override: None,
        }
    }

    fn running_container(
        service: &str,
        image: &str,
        publishers: &str,
        health: &str,
    ) -> ContainerEntry {
        serde_json::from_str(&format!(
            r#"{{"Name":"blog-{service}-1","Service":"{service}","State":"running","Status":"Up 2 hours","Health":"{health}","ExitCode":0,"Image":"{image}","Publishers":{publishers}}}"#
        ))
        .unwrap()
    }

    fn sail_services(containers: &[ContainerEntry]) -> Vec<Service> {
        let config = parse_compose_config(SAIL_CONFIG).unwrap();
        build_services(&config, containers, "laravel.test")
    }

    fn find<'a>(services: &'a [Service], name: &str) -> &'a Service {
        services
            .iter()
            .find(|service| service.name == name)
            .unwrap()
    }

    #[test]
    fn test_detect_compose_files() {
        assert_eq!(detect_compose_files(&["README.md", "composer.json"]), None);
        assert_eq!(
            detect_compose_files(&["docker-compose.yml"]),
            Some(ComposeFiles {
                primary: "docker-compose.yml".into(),
                overrides: vec![],
            })
        );
        assert_eq!(
            detect_compose_files(&[
                "docker-compose.yml",
                "docker-compose.override.yml",
                "docker-compose.override.yaml",
                "compose.override.yaml",
            ]),
            Some(ComposeFiles {
                primary: "docker-compose.yml".into(),
                overrides: vec![
                    "docker-compose.override.yaml".into(),
                    "docker-compose.override.yml".into()
                ],
            })
        );
        assert_eq!(
            detect_compose_files(&[
                "docker-compose.yaml",
                "compose.yml",
                "compose.override.yml",
                "docker-compose.override.yml",
            ]),
            Some(ComposeFiles {
                primary: "compose.yml".into(),
                overrides: vec!["compose.override.yml".into()],
            })
        );
        assert_eq!(
            detect_compose_files(&["compose.yaml", "compose.yml"])
                .unwrap()
                .primary,
            "compose.yaml"
        );
    }

    #[test]
    fn test_is_sail_project() {
        let require_dev = r#"{"require": {"laravel/framework": "^11"}, "require-dev": {"laravel/sail": "^1.26"}}"#;
        let require = r#"{"require": {"laravel/sail": "^1.26"}}"#;
        let without_sail = r#"{"require": {"laravel/framework": "^11"}}"#;
        assert!(is_sail_project(Some(require_dev), false));
        assert!(is_sail_project(Some(require), false));
        assert!(!is_sail_project(Some(without_sail), false));
        assert!(is_sail_project(None, true));
        assert!(is_sail_project(Some("not json"), true));
        assert!(!is_sail_project(Some("not json"), false));
        assert!(!is_sail_project(Some(r#"{"require": []}"#), false));
        assert!(!is_sail_project(None, false));
    }

    #[test]
    fn test_detect_node_package_manager() {
        assert_eq!(
            detect_node_package_manager(&["package.json", "pnpm-lock.yaml"]),
            Some(NodePackageManager::Pnpm)
        );
        assert_eq!(
            detect_node_package_manager(&["package.json", "yarn.lock"]),
            Some(NodePackageManager::Yarn)
        );
        assert_eq!(
            detect_node_package_manager(&["package.json", "bun.lock"]),
            Some(NodePackageManager::Bun)
        );
        assert_eq!(
            detect_node_package_manager(&["package.json", "bun.lockb"]),
            Some(NodePackageManager::Bun)
        );
        assert_eq!(
            detect_node_package_manager(&["package.json", "package-lock.json"]),
            Some(NodePackageManager::Npm)
        );
        assert_eq!(
            detect_node_package_manager(&["package.json"]),
            Some(NodePackageManager::Npm)
        );
        assert_eq!(detect_node_package_manager(&["composer.json"]), None);
    }

    #[test]
    fn test_node_package_manager_commands() {
        assert_eq!(NodePackageManager::Pnpm.program(), "pnpm");
        assert_eq!(
            NodePackageManager::Npm.run_script_args("dev"),
            ["run", "dev"]
        );
        assert_eq!(
            NodePackageManager::Bun.run_script_args("dev"),
            ["run", "dev"]
        );
        assert_eq!(NodePackageManager::Yarn.run_script_args("dev"), ["dev"]);
        assert_eq!(NodePackageManager::Yarn.install_args(), ["install"]);
    }

    #[test]
    fn test_parse_compose_config() {
        let config = parse_compose_config(SAIL_CONFIG).unwrap();
        assert_eq!(config.name, "blog");
        assert_eq!(config.services.len(), 5);
        let app = &config.services["laravel.test"];
        assert_eq!(app.ports.len(), 2);
        assert_eq!(app.ports[0].published.as_deref(), Some("80"));
        assert_eq!(app.ports[0].host_ip.as_deref(), Some("0.0.0.0"));
        assert_eq!(app.ports[0].published_port(), Some(80));
        assert_eq!(app.ports[1].published.as_deref(), Some("5173"));
        assert_eq!(app.ports[1].published_port(), Some(5173));
        assert_eq!(app.ports[1].protocol, "tcp");
        assert_eq!(
            config.services["mysql"].container_name.as_deref(),
            Some("blog-mysql")
        );
    }

    #[test]
    fn test_parse_compose_config_port_variants() {
        let config = parse_compose_config(
            r#"{"name":"x","services":{"web":{"ports":[
                {"target": 80, "published": "8000-8010"},
                {"target": 81},
                {"target": 82, "published": null, "protocol": "udp"}
            ]}}}"#,
        )
        .unwrap();
        let ports = &config.services["web"].ports;
        assert_eq!(ports[0].published.as_deref(), Some("8000-8010"));
        assert_eq!(ports[0].published_port(), None);
        assert_eq!(ports[1].published, None);
        assert_eq!(ports[1].published_port(), None);
        assert_eq!(ports[1].protocol, "tcp");
        assert_eq!(ports[2].published_port(), None);
        assert_eq!(ports[2].protocol, "udp");
    }

    #[test]
    fn test_parse_compose_config_does_not_keep_secrets() {
        let config = parse_compose_config(SAIL_CONFIG).unwrap();
        let dumped = format!("{config:?}");
        for secret in [
            "super-secret-password",
            "topsecret",
            "label-secret",
            "DB_PASSWORD",
            "/secret",
            "/home/x/.env",
        ] {
            assert!(!dumped.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn test_parse_compose_config_invalid() {
        assert!(parse_compose_config("not json").is_err());
    }

    const NDJSON: &str = concat!(
        r#"{"Name":"blog-laravel.test-1","Service":"laravel.test","State":"running","Status":"Up 2 hours","Health":"","ExitCode":0,"Image":"sail-8.4/app","Publishers":[{"URL":"0.0.0.0","TargetPort":80,"PublishedPort":80,"Protocol":"tcp"},{"URL":"::","TargetPort":80,"PublishedPort":80,"Protocol":"tcp"},{"URL":"","TargetPort":9000,"PublishedPort":0,"Protocol":"tcp"}]}"#,
        "\n\n",
        r#"{"Name":"blog-mysql-1","Service":"mysql","State":"running","Status":"Up 2 hours (healthy)","Health":"healthy","ExitCode":0,"Publishers":null}"#,
        "\n",
    );

    #[test]
    fn test_parse_compose_ps_ndjson() {
        let entries = parse_compose_ps(NDJSON).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].service, "laravel.test");
        assert_eq!(entries[0].publishers.len(), 3);
        assert_eq!(entries[0].health.as_deref(), Some(""));
        assert_eq!(entries[1].health.as_deref(), Some("healthy"));
        assert!(entries[1].publishers.is_empty());
        assert_eq!(entries[1].image, None);
    }

    #[test]
    fn test_parse_compose_ps_array_and_empty() {
        let array = r#"[{"Name":"a-1","Service":"a","State":"exited","Status":"Exited (137) 1 minute ago","ExitCode":137}]"#;
        let entries = parse_compose_ps(array).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].exit_code, 137);
        assert_eq!(entries[0].health, None);
        assert!(parse_compose_ps("").unwrap().is_empty());
        assert!(parse_compose_ps("  \n\n").unwrap().is_empty());
        assert!(parse_compose_ps("[]").unwrap().is_empty());
        assert!(parse_compose_ps("garbage").is_err());
    }

    #[test]
    fn test_build_services_running() {
        let containers = parse_compose_ps(NDJSON).unwrap();
        let services = sail_services(&containers);
        let names: Vec<&str> = services
            .iter()
            .map(|service| service.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["laravel.test", "mailpit", "meilisearch", "mysql", "redis"]
        );

        let app = find(&services, "laravel.test");
        assert_eq!(app.kind, ServiceKind::App);
        assert_eq!(app.state, ServiceState::Running { health: None });
        assert_eq!(app.status_text.as_deref(), Some("Up 2 hours"));
        assert_eq!(app.container_name.as_deref(), Some("blog-laravel.test-1"));
        assert_eq!(
            app.ports,
            vec![PortMapping {
                host_ip: Some("0.0.0.0".into()),
                host_port: Some(80),
                container_port: 80,
                protocol: "tcp".into(),
            }]
        );

        let mysql = find(&services, "mysql");
        assert_eq!(
            mysql.state,
            ServiceState::Running {
                health: Some(Health::Healthy)
            }
        );
        assert_eq!(mysql.kind, ServiceKind::Mysql);
        assert_eq!(mysql.image.as_deref(), Some("mysql/mysql-server:8.0"));
        assert_eq!(mysql.ports.len(), 1);
        assert_eq!(mysql.ports[0].host_port, Some(3306));
    }

    #[test]
    fn test_build_services_stopped_uses_config_ports() {
        let stopped: ContainerEntry = serde_json::from_str(
            r#"{"Name":"blog-redis-1","Service":"redis","State":"exited","Status":"Exited (1) 3 minutes ago","ExitCode":1,"Image":"redis:alpine","Publishers":[]}"#,
        )
        .unwrap();
        let services = sail_services(&[stopped]);
        let redis = find(&services, "redis");
        assert_eq!(redis.state, ServiceState::Exited { exit_code: 1 });
        assert_eq!(redis.ports.len(), 1);
        assert_eq!(redis.ports[0].host_port, Some(6379));
        assert_eq!(redis.ports[0].container_port, 6379);
    }

    #[test]
    fn test_build_services_without_container() {
        let services = sail_services(&[]);
        let meilisearch = find(&services, "meilisearch");
        assert_eq!(meilisearch.state, ServiceState::NotCreated);
        assert_eq!(meilisearch.status_text, None);
        assert_eq!(meilisearch.kind, ServiceKind::Other);
        assert_eq!(meilisearch.ports[0].host_port, Some(7700));
        assert_eq!(
            find(&services, "mysql").container_name.as_deref(),
            Some("blog-mysql")
        );
        assert_eq!(find(&services, "laravel.test").kind, ServiceKind::App);
    }

    #[test]
    fn test_build_services_dedupes_ipv6_publishers() {
        let container = running_container(
            "redis",
            "redis:alpine",
            r#"[{"URL":"0.0.0.0","TargetPort":6379,"PublishedPort":6379,"Protocol":"tcp"},{"URL":"::","TargetPort":6379,"PublishedPort":6379,"Protocol":"tcp"}]"#,
            "",
        );
        let services = sail_services(&[container]);
        assert_eq!(find(&services, "redis").ports.len(), 1);
    }

    #[test]
    fn test_build_services_ignores_foreign_containers() {
        let foreign = running_container("ghost", "nginx", "[]", "");
        let services = sail_services(&[foreign]);
        assert_eq!(services.len(), 5);
        assert!(services.iter().all(|service| service.name != "ghost"));
        assert!(
            services
                .iter()
                .all(|service| service.state == ServiceState::NotCreated)
        );
    }

    #[test]
    fn test_service_state_variants() {
        let state_of = |state: &str, health: &str| {
            let container: ContainerEntry = serde_json::from_str(&format!(
                r#"{{"Name":"c","Service":"redis","State":"{state}","Health":"{health}","ExitCode":2}}"#
            ))
            .unwrap();
            sail_services(&[container])
                .into_iter()
                .find(|service| service.name == "redis")
                .unwrap()
                .state
        };
        assert_eq!(
            state_of("running", "starting"),
            ServiceState::Running {
                health: Some(Health::Starting)
            }
        );
        assert_eq!(
            state_of("running", "unhealthy"),
            ServiceState::Running {
                health: Some(Health::Unhealthy)
            }
        );
        assert_eq!(state_of("restarting", ""), ServiceState::Restarting);
        assert_eq!(state_of("paused", ""), ServiceState::Paused);
        assert_eq!(state_of("created", ""), ServiceState::Created);
        assert_eq!(
            state_of("exited", ""),
            ServiceState::Exited { exit_code: 2 }
        );
        assert_eq!(state_of("dead", ""), ServiceState::Unknown("dead".into()));
        assert_eq!(
            state_of("removing", ""),
            ServiceState::Unknown("removing".into())
        );
        assert!(state_of("running", "").is_running());
        assert!(!state_of("paused", "").is_running());
    }

    #[test]
    fn test_environment_state() {
        assert_eq!(environment_state(&[]), EnvironmentState::NoServices);

        let stopped = sail_services(&[]);
        assert_eq!(environment_state(&stopped), EnvironmentState::Stopped);

        let partial = sail_services(&parse_compose_ps(NDJSON).unwrap());
        assert_eq!(environment_state(&partial), EnvironmentState::Partial);

        let all_running: Vec<ContainerEntry> =
            ["laravel.test", "mysql", "redis", "mailpit", "meilisearch"]
                .iter()
                .map(|service| running_container(service, "img", "[]", ""))
                .collect();
        let running = sail_services(&all_running);
        assert_eq!(environment_state(&running), EnvironmentState::Running);
    }

    fn service_with_ports(kind: ServiceKind, ports: &[(u16, u16)]) -> Service {
        Service {
            name: "svc".into(),
            image: None,
            kind,
            state: ServiceState::Running { health: None },
            status_text: None,
            container_name: None,
            ports: ports
                .iter()
                .map(|(host_port, container_port)| PortMapping {
                    host_ip: None,
                    host_port: Some(*host_port),
                    container_port: *container_port,
                    protocol: "tcp".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_web_urls() {
        let app = service_with_ports(ServiceKind::App, &[(80, 80), (5173, 5173)]);
        assert_eq!(
            web_urls(&app, "localhost", None),
            ["http://localhost", "http://localhost:5173"]
        );

        let app_custom = service_with_ports(ServiceKind::App, &[(8080, 80), (8443, 443)]);
        assert_eq!(
            web_urls(&app_custom, "blog.test", None),
            ["http://blog.test:8080", "https://blog.test:8443"]
        );

        let app_https = service_with_ports(ServiceKind::App, &[(443, 443)]);
        assert_eq!(
            web_urls(&app_https, "localhost", None),
            ["https://localhost"]
        );

        let mailpit = service_with_ports(ServiceKind::Mailpit, &[(1025, 1025), (8025, 8025)]);
        assert_eq!(
            web_urls(&mailpit, "localhost", None),
            ["http://localhost:8025"]
        );

        let phpmyadmin = service_with_ports(ServiceKind::PhpMyAdmin, &[(8081, 80)]);
        assert_eq!(
            web_urls(&phpmyadmin, "localhost", None),
            ["http://localhost:8081"]
        );

        let meilisearch = service_with_ports(ServiceKind::Other, &[(7700, 7700), (8080, 80)]);
        assert!(web_urls(&meilisearch, "localhost", None).is_empty());

        let mysql = service_with_ports(ServiceKind::Mysql, &[(3306, 3306)]);
        assert!(web_urls(&mysql, "localhost", None).is_empty());

        assert_eq!(
            web_urls(&mysql, "localhost", Some("https://custom.test")),
            ["https://custom.test"]
        );

        let mut unpublished = service_with_ports(ServiceKind::App, &[(80, 80)]);
        unpublished.ports[0].host_port = None;
        assert!(web_urls(&unpublished, "localhost", None).is_empty());
    }

    #[test]
    fn test_validate_service_name() {
        for valid in ["laravel.test", "mysql", "db_1", "my-service", "A1"] {
            assert!(validate_service_name(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "my service",
            "-rm",
            "a;b",
            "a$(x)",
            ".hidden",
            "_a",
            "a/b",
            "ñ",
        ] {
            assert!(validate_service_name(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn test_compose_commands() {
        let plain = invocation(&[]);
        let config = plain.config_command();
        assert_eq!(config.program, PathBuf::from("/usr/bin/docker"));
        assert!(config.environment.is_empty());
        assert_eq!(
            config.args,
            [
                "compose",
                "--project-directory",
                "/work/blog",
                "config",
                "--format",
                "json"
            ]
        );
        assert_eq!(
            plain.ps_command().args,
            [
                "compose",
                "--project-directory",
                "/work/blog",
                "ps",
                "-a",
                "--format",
                "json"
            ]
        );
        assert_eq!(
            plain.up_command().args,
            ["compose", "--project-directory", "/work/blog", "up", "-d"]
        );
        assert_eq!(
            plain.restart_command().args,
            ["compose", "--project-directory", "/work/blog", "restart"]
        );
        assert_eq!(
            plain.events_command().args,
            [
                "compose",
                "--project-directory",
                "/work/blog",
                "events",
                "--json"
            ]
        );

        let with_files = invocation(&["compose.yaml", "compose.override.yaml"]);
        assert_eq!(
            with_files.config_command().args,
            [
                "compose",
                "--project-directory",
                "/work/blog",
                "-f",
                "compose.yaml",
                "-f",
                "compose.override.yaml",
                "config",
                "--format",
                "json"
            ]
        );
    }

    #[test]
    fn test_sail_commands() {
        let plain = sail_invocation(&[]);
        let config = plain.config_command();
        assert_eq!(config.program, PathBuf::from("/work/blog/vendor/bin/sail"));
        assert_eq!(config.args, ["config", "--format", "json"]);
        assert!(
            !config
                .environment
                .iter()
                .any(|(name, _)| name == "SAIL_FILES")
        );
        assert_eq!(
            config.environment,
            [("SAIL_SKIP_CHECKS".to_string(), "1".to_string())]
        );
        assert_eq!(plain.ps_command().args, ["ps", "-a", "--format", "json"]);
        assert_eq!(plain.up_command().args, ["up", "-d"]);
        assert_eq!(plain.stop_command().args, ["stop"]);
        assert_eq!(plain.restart_command().args, ["restart"]);
        assert_eq!(plain.events_command().args, ["events", "--json"]);

        let with_files = sail_invocation(&["compose.yaml", "compose.override.yaml"]);
        let command = with_files.up_command();
        assert_eq!(command.args, ["up", "-d"]);
        assert!(command.environment.contains(&(
            "SAIL_FILES".to_string(),
            "compose.yaml:compose.override.yaml".to_string()
        )));
        for command in [
            with_files.config_command(),
            with_files.logs_command("redis", true, 10).unwrap(),
            with_files
                .exec_command("mysql", None, None, false, &["ls".to_string()])
                .unwrap(),
            with_files
                .service_command(ServiceOperation::Stop, "mysql")
                .unwrap(),
        ] {
            assert_ne!(command.args.first().map(String::as_str), Some("compose"));
            assert!(
                !command
                    .args
                    .iter()
                    .any(|argument| argument == "--project-directory")
            );
        }
    }

    #[test]
    fn test_stop_command_never_destroys() {
        for invocation in [invocation(&["compose.yaml"]), sail_invocation(&[])] {
            let args = invocation.stop_command().args;
            assert_eq!(args.last().map(String::as_str), Some("stop"));
            assert!(
                !args
                    .iter()
                    .any(|argument| argument == "down" || argument == "-v")
            );
            let service_stop = invocation
                .service_command(ServiceOperation::Stop, "mysql")
                .unwrap();
            assert!(
                !service_stop
                    .args
                    .iter()
                    .any(|argument| argument == "down" || argument == "-v")
            );
        }
    }

    #[test]
    fn test_service_command() {
        let invocation = invocation(&[]);
        let base = ["compose", "--project-directory", "/work/blog"];
        assert_eq!(
            invocation
                .service_command(ServiceOperation::Start, "mysql")
                .unwrap()
                .args,
            [base.as_slice(), &["up", "-d", "mysql"]].concat()
        );
        assert_eq!(
            invocation
                .service_command(ServiceOperation::Stop, "mysql")
                .unwrap()
                .args,
            [base.as_slice(), &["stop", "mysql"]].concat()
        );
        assert_eq!(
            invocation
                .service_command(ServiceOperation::Restart, "mysql")
                .unwrap()
                .args,
            [base.as_slice(), &["restart", "mysql"]].concat()
        );
        assert!(
            invocation
                .service_command(ServiceOperation::Start, "-x")
                .is_err()
        );
        assert!(
            invocation
                .service_command(ServiceOperation::Stop, "a b")
                .is_err()
        );
    }

    #[test]
    fn test_logs_command() {
        let invocation = invocation(&[]);
        let base = ["compose", "--project-directory", "/work/blog"];
        assert_eq!(
            invocation.logs_command("redis", true, 200).unwrap().args,
            [base.as_slice(), &["logs", "--tail", "200", "-f", "redis"]].concat()
        );
        assert_eq!(
            invocation.logs_command("redis", false, 50).unwrap().args,
            [base.as_slice(), &["logs", "--tail", "50", "redis"]].concat()
        );
        assert!(invocation.logs_command("--help", false, 50).is_err());
    }

    #[test]
    fn test_exec_command() {
        let invocation = invocation(&[]);
        let base = ["compose", "--project-directory", "/work/blog"];
        let command = vec!["mysql".to_string(), "-uroot".to_string()];
        assert_eq!(
            invocation
                .exec_command(
                    "laravel.test",
                    Some("sail"),
                    Some("/var/www/html"),
                    false,
                    &command
                )
                .unwrap()
                .args,
            [
                base.as_slice(),
                &[
                    "exec",
                    "-u",
                    "sail",
                    "-w",
                    "/var/www/html",
                    "laravel.test",
                    "mysql",
                    "-uroot"
                ]
            ]
            .concat()
        );
        assert_eq!(
            invocation
                .exec_command("mysql", None, None, false, &command)
                .unwrap()
                .args,
            [base.as_slice(), &["exec", "mysql", "mysql", "-uroot"]].concat()
        );
        assert!(
            invocation
                .exec_command("mysql;x", None, None, false, &command)
                .is_err()
        );
        assert!(
            invocation
                .exec_command("mysql", Some("-u"), None, false, &command)
                .is_err()
        );
    }

    #[test]
    fn test_choose_shell() {
        assert_eq!(
            choose_shell(ShellPreference::Zsh, &["sh", "bash", "zsh"]),
            Some("zsh")
        );
        assert_eq!(
            choose_shell(ShellPreference::Zsh, &["sh", "bash"]),
            Some("bash")
        );
        assert_eq!(choose_shell(ShellPreference::Zsh, &["sh"]), Some("sh"));
        assert_eq!(
            choose_shell(ShellPreference::Bash, &["zsh", "sh"]),
            Some("sh")
        );
        assert_eq!(
            choose_shell(ShellPreference::Sh, &["bash", "sh"]),
            Some("sh")
        );
        assert_eq!(
            choose_shell(ShellPreference::Auto, &["zsh", "bash", "sh"]),
            Some("bash")
        );
        assert_eq!(choose_shell(ShellPreference::Auto, &["zsh"]), None);
        assert_eq!(choose_shell(ShellPreference::Sh, &[]), None);
    }

    #[test]
    fn test_app_exec_user() {
        assert_eq!(app_exec_user(&ServiceKind::App, true), Some("sail"));
        assert_eq!(app_exec_user(&ServiceKind::App, false), None);
        assert_eq!(app_exec_user(&ServiceKind::Mysql, true), None);
    }

    #[test]
    fn test_classify_service() {
        let classify =
            |name: &str, image: Option<&str>| classify_service(name, image, "laravel.test");
        assert_eq!(classify("laravel.test", None), ServiceKind::App);
        assert_eq!(classify("web", Some("sail-8.4/app")), ServiceKind::App);
        assert_eq!(
            classify("db", Some("docker.io/library/mysql:8.0")),
            ServiceKind::Mysql
        );
        assert_eq!(
            classify("db", Some("mysql/mysql-server:8.0")),
            ServiceKind::Mysql
        );
        assert_eq!(classify("db", Some("MariaDB:11")), ServiceKind::Mariadb);
        assert_eq!(classify("db", Some("postgres:17")), ServiceKind::Postgres);
        assert_eq!(
            classify("db", Some("postgis/postgis:16-3.4")),
            ServiceKind::Postgres
        );
        assert_eq!(classify("cache", Some("redis:alpine")), ServiceKind::Redis);
        assert_eq!(
            classify("cache", Some("valkey/valkey:8")),
            ServiceKind::Redis
        );
        assert_eq!(
            classify("mail", Some("registry.local:5000/axllent/mailpit:latest")),
            ServiceKind::Mailpit
        );
        assert_eq!(
            classify("pma", Some("phpmyadmin:latest")),
            ServiceKind::PhpMyAdmin
        );
        assert_eq!(
            classify("search", Some("getmeili/meilisearch:latest")),
            ServiceKind::Other
        );
        assert_eq!(classify("x", None), ServiceKind::Other);
        assert_eq!(
            classify("db", Some("mysql@sha256:abcdef")),
            ServiceKind::Mysql
        );
    }

    #[test]
    fn test_default_client_command() {
        assert_eq!(
            ServiceKind::Mysql.default_client_command(),
            Some(vec!["mysql"])
        );
        assert_eq!(
            ServiceKind::Mariadb.default_client_command(),
            Some(vec!["mariadb"])
        );
        assert_eq!(
            ServiceKind::Postgres.default_client_command(),
            Some(vec!["psql"])
        );
        assert_eq!(
            ServiceKind::Redis.default_client_command(),
            Some(vec!["redis-cli"])
        );
        assert_eq!(ServiceKind::App.default_client_command(), None);
        assert_eq!(ServiceKind::Other.default_client_command(), None);
    }

    fn named_service(name: &str, kind: ServiceKind) -> Service {
        Service {
            name: name.into(),
            image: None,
            kind,
            state: ServiceState::Running { health: None },
            status_text: None,
            container_name: None,
            ports: Vec::new(),
        }
    }

    fn shell_target(shell: &str, root: bool) -> TerminalTarget {
        TerminalTarget::Shell {
            shell: shell.to_string(),
            root,
        }
    }

    #[test]
    fn test_terminal_command_sail_app() {
        let sail = sail_invocation(&["compose.yaml"]);
        let app = named_service("laravel.test", ServiceKind::App);
        let run = |target: &TerminalTarget| {
            terminal_command(&sail, &app, true, "laravel.test", target)
                .unwrap()
                .args
        };
        assert_eq!(
            run(&shell_target("bash", false)),
            ["exec", "-u", "sail", "laravel.test", "bash"]
        );
        assert_eq!(
            run(&shell_target("bash", true)),
            ["exec", "-u", "root", "laravel.test", "bash"]
        );
        assert_eq!(
            run(&shell_target("zsh", false)),
            ["exec", "-u", "sail", "laravel.test", "zsh"]
        );
        assert_eq!(
            run(&shell_target("sh", true)),
            ["exec", "-u", "root", "laravel.test", "sh"]
        );
        let other_app = named_service("web", ServiceKind::App);
        let args = terminal_command(&sail, &other_app, true, "web", &shell_target("bash", false))
            .unwrap()
            .args;
        assert_eq!(args, ["exec", "-u", "sail", "web", "bash"]);
        let command = terminal_command(
            &sail,
            &app,
            true,
            "laravel.test",
            &shell_target("bash", false),
        )
        .unwrap();
        assert!(
            command
                .environment
                .contains(&("SAIL_SKIP_CHECKS".to_string(), "1".to_string()))
        );
    }

    #[test]
    fn test_terminal_command_sail_clients() {
        let sail = sail_invocation(&[]);
        let client = |name: &str, kind: ServiceKind| {
            terminal_command(
                &sail,
                &named_service(name, kind),
                true,
                "laravel.test",
                &TerminalTarget::Client,
            )
        };
        assert_eq!(client("mysql", ServiceKind::Mysql).unwrap().args, ["mysql"]);
        assert_eq!(
            client("mariadb", ServiceKind::Mariadb).unwrap().args,
            ["mariadb"]
        );
        assert_eq!(
            client("pgsql", ServiceKind::Postgres).unwrap().args,
            ["psql"]
        );
        assert_eq!(client("redis", ServiceKind::Redis).unwrap().args, ["redis"]);
        assert_eq!(
            client("valkey", ServiceKind::Redis).unwrap().args,
            ["valkey"]
        );
        assert_eq!(
            client("db", ServiceKind::Mysql).unwrap().args,
            ["exec", "db", "mysql"]
        );
        assert_eq!(
            client("postgres", ServiceKind::Postgres).unwrap().args,
            ["exec", "postgres", "psql"]
        );
        assert_eq!(
            client("cache", ServiceKind::Redis).unwrap().args,
            ["exec", "cache", "redis-cli"]
        );
        assert!(client("mailpit", ServiceKind::Mailpit).is_err());
        assert!(client("laravel.test", ServiceKind::App).is_err());
    }

    #[test]
    fn test_terminal_command_without_sail() {
        let plain = invocation(&["compose.yaml"]);
        let base = [
            "compose",
            "--project-directory",
            "/work/blog",
            "-f",
            "compose.yaml",
        ];
        let app = named_service("laravel.test", ServiceKind::App);
        assert_eq!(
            terminal_command(
                &plain,
                &app,
                false,
                "laravel.test",
                &shell_target("bash", false)
            )
            .unwrap()
            .args,
            [base.as_slice(), &["exec", "laravel.test", "bash"]].concat()
        );
        let mysql = named_service("mysql", ServiceKind::Mysql);
        assert_eq!(
            terminal_command(
                &plain,
                &mysql,
                false,
                "laravel.test",
                &TerminalTarget::Client
            )
            .unwrap()
            .args,
            [base.as_slice(), &["exec", "mysql", "mysql"]].concat()
        );
        assert!(
            terminal_command(&plain, &app, false, "laravel.test", &TerminalTarget::Client).is_err()
        );
    }

    #[test]
    fn test_terminal_command_other_sail_service_uses_exec() {
        let sail = sail_invocation(&[]);
        let mysql = named_service("mysql", ServiceKind::Mysql);
        assert_eq!(
            terminal_command(
                &sail,
                &mysql,
                true,
                "laravel.test",
                &shell_target("sh", false)
            )
            .unwrap()
            .args,
            ["exec", "mysql", "sh"]
        );
        assert!(
            terminal_command(
                &sail,
                &mysql,
                true,
                "laravel.test",
                &shell_target("a b", false)
            )
            .is_err()
        );
    }

    #[test]
    fn test_exec_command_no_tty() {
        assert_eq!(
            sail_invocation(&[])
                .exec_command("mysql", Some("sail"), None, true, &["sh".to_string()])
                .unwrap()
                .args,
            ["exec", "-T", "-u", "sail", "mysql", "sh"]
        );
    }

    #[test]
    fn test_parse_available_shells() {
        assert_eq!(
            parse_available_shells("/usr/bin/zsh\n/bin/bash\n/bin/sh\n"),
            ["zsh", "bash", "sh"]
        );
        assert_eq!(parse_available_shells("/bin/sh\n/usr/bin/sh\n"), ["sh"]);
        assert_eq!(parse_available_shells("bash\nsh"), ["bash", "sh"]);
        assert_eq!(
            parse_available_shells("/bin/ash\n/bin/fish\n"),
            Vec::<&str>::new()
        );
        assert!(parse_available_shells("").is_empty());
    }

    #[test]
    fn test_validate_manual_url() {
        assert_eq!(validate_manual_url("  ").unwrap(), None);
        assert_eq!(validate_manual_url("").unwrap(), None);
        assert_eq!(
            validate_manual_url(" https://blog.test:8443/app ").unwrap(),
            Some("https://blog.test:8443/app".to_string())
        );
        assert_eq!(
            validate_manual_url("http://localhost:8080").unwrap(),
            Some("http://localhost:8080".to_string())
        );
        for invalid in [
            "localhost:8080",
            "ftp://x",
            "http://",
            "http:///x",
            "http://a b",
        ] {
            assert!(validate_manual_url(invalid).is_err(), "{invalid}");
        }
    }

    const ALL_MANAGERS: [NodePackageManager; 4] = [
        NodePackageManager::Npm,
        NodePackageManager::Pnpm,
        NodePackageManager::Yarn,
        NodePackageManager::Bun,
    ];

    #[test]
    fn test_quick_action_labels_and_display_commands() {
        let npm = NodePackageManager::Npm;
        let expected = [
            (QuickAction::Tinker, "Tinker", "php artisan tinker"),
            (QuickAction::Migrate, "Migrate", "php artisan migrate"),
            (
                QuickAction::MigrateSeed,
                "Migrate and Seed",
                "php artisan migrate --seed",
            ),
            (
                QuickAction::MigrateStatus,
                "Migration Status",
                "php artisan migrate:status",
            ),
            (
                QuickAction::RouteList,
                "Route List",
                "php artisan route:list",
            ),
            (
                QuickAction::OptimizeClear,
                "Clear Caches",
                "php artisan optimize:clear",
            ),
            (QuickAction::Test, "Run Tests", "php artisan test"),
            (
                QuickAction::ComposerInstall,
                "composer install",
                "composer install",
            ),
            (
                QuickAction::ComposerUpdate,
                "composer update",
                "composer update",
            ),
            (QuickAction::NodeInstall, "npm install", "npm install"),
            (QuickAction::NodeDev, "npm run dev", "npm run dev"),
            (QuickAction::NodeBuild, "npm run build", "npm run build"),
        ];
        for (action, label, display) in expected {
            assert_eq!(action.label(npm), label);
            assert_eq!(action.display_command(npm), display);
        }
        assert_eq!(
            QuickAction::NodeDev.display_command(NodePackageManager::Yarn),
            "yarn dev"
        );
        assert_eq!(
            QuickAction::NodeBuild.label(NodePackageManager::Bun),
            "bun run build"
        );
        assert_eq!(
            QuickAction::NodeInstall.label(NodePackageManager::Pnpm),
            "pnpm install"
        );
    }

    #[test]
    fn test_quick_actions_requiring_confirmation() {
        let all = [
            QuickAction::Tinker,
            QuickAction::Migrate,
            QuickAction::MigrateSeed,
            QuickAction::MigrateStatus,
            QuickAction::RouteList,
            QuickAction::OptimizeClear,
            QuickAction::Test,
            QuickAction::ComposerInstall,
            QuickAction::ComposerUpdate,
            QuickAction::NodeInstall,
            QuickAction::NodeDev,
            QuickAction::NodeBuild,
        ];
        let confirmed: Vec<QuickAction> = all
            .into_iter()
            .filter(|action| action.requires_confirmation())
            .collect();
        assert_eq!(
            confirmed,
            [QuickAction::MigrateSeed, QuickAction::ComposerUpdate]
        );
    }

    #[test]
    fn test_quick_action_command_with_sail() {
        let sail = sail_invocation(&["compose.yaml"]);
        let run = |action, manager| {
            quick_action_command(&sail, true, "laravel.test", action, manager).unwrap()
        };
        let npm = NodePackageManager::Npm;
        assert_eq!(run(QuickAction::Tinker, npm).args, ["artisan", "tinker"]);
        assert_eq!(
            run(QuickAction::MigrateSeed, npm).args,
            ["artisan", "migrate", "--seed"]
        );
        assert_eq!(run(QuickAction::Test, npm).args, ["artisan", "test"]);
        assert_eq!(
            run(QuickAction::ComposerUpdate, npm).args,
            ["composer", "update"]
        );
        let command = run(QuickAction::RouteList, npm);
        assert_eq!(command.program, PathBuf::from("/work/blog/vendor/bin/sail"));
        assert!(
            command
                .environment
                .contains(&("SAIL_SKIP_CHECKS".to_string(), "1".to_string()))
        );
        let expected = [
            (NodePackageManager::Npm, ["npm", "run", "dev"].as_slice()),
            (NodePackageManager::Pnpm, ["pnpm", "run", "dev"].as_slice()),
            (NodePackageManager::Yarn, ["yarn", "dev"].as_slice()),
            (NodePackageManager::Bun, ["bun", "run", "dev"].as_slice()),
        ];
        for (manager, args) in expected {
            assert_eq!(run(QuickAction::NodeDev, manager).args, args);
        }
        for manager in ALL_MANAGERS {
            assert_eq!(
                run(QuickAction::NodeInstall, manager).args,
                [manager.program(), "install"]
            );
            assert_eq!(
                run(QuickAction::NodeBuild, manager)
                    .args
                    .first()
                    .map(String::as_str),
                Some(manager.program())
            );
        }
    }

    #[test]
    fn test_quick_action_command_without_sail() {
        let compose = invocation(&["compose.yaml"]);
        let run = |action, manager| {
            quick_action_command(&compose, false, "app", action, manager).unwrap()
        };
        let npm = NodePackageManager::Npm;
        assert_eq!(
            run(QuickAction::Tinker, npm).args[run(QuickAction::Tinker, npm).args.len() - 4..],
            ["exec", "app", "php", "artisan", "tinker"][1..]
        );
        let command = run(QuickAction::MigrateSeed, npm);
        assert_eq!(command.program, PathBuf::from("/usr/bin/docker"));
        assert!(command.args.ends_with(&[
            "exec".to_string(),
            "app".to_string(),
            "php".to_string(),
            "artisan".to_string(),
            "migrate".to_string(),
            "--seed".to_string(),
        ]));
        assert!(run(QuickAction::ComposerInstall, npm).args.ends_with(&[
            "exec".into(),
            "app".into(),
            "composer".into(),
            "install".into()
        ]));
        for manager in ALL_MANAGERS {
            let mut expected = vec![
                "exec".to_string(),
                "app".to_string(),
                manager.program().to_string(),
            ];
            expected.extend(manager.run_script_args("build"));
            assert!(
                run(QuickAction::NodeBuild, manager)
                    .args
                    .ends_with(&expected)
            );
        }
        assert!(quick_action_command(&compose, false, "bad name", QuickAction::Test, npm).is_err());
    }

    #[test]
    fn test_composer_custom_command() {
        let arguments = ["require".to_string(), "laravel/horizon".to_string()];
        let sail = composer_custom_command(&sail_invocation(&[]), true, "laravel.test", &arguments)
            .unwrap();
        assert_eq!(sail.args, ["composer", "require", "laravel/horizon"]);
        let compose = composer_custom_command(&invocation(&[]), false, "app", &arguments).unwrap();
        assert!(compose.args.ends_with(&[
            "exec".into(),
            "app".into(),
            "composer".into(),
            "require".into(),
            "laravel/horizon".into()
        ]));
        assert!(composer_custom_command(&invocation(&[]), false, "app", &[]).is_err());
    }

    #[test]
    fn test_split_command_line() {
        assert_eq!(
            split_command_line("require laravel/horizon").unwrap(),
            ["require", "laravel/horizon"]
        );
        assert_eq!(
            split_command_line(r#"echo "hello world" 'it is' a\ b"#).unwrap(),
            ["echo", "hello world", "it is", "a b"]
        );
        assert_eq!(
            split_command_line(r#"say "she said \"hi\"""#).unwrap(),
            ["say", r#"she said "hi""#]
        );
        assert_eq!(
            split_command_line("a ; b && c | d $(id) `x`").unwrap(),
            ["a", ";", "b", "&&", "c", "|", "d", "$(id)", "`x`"]
        );
        assert_eq!(split_command_line("cmd a;b&&c").unwrap(), ["cmd", "a;b&&c"]);
        assert_eq!(split_command_line("a '' b").unwrap(), ["a", "", "b"]);
        assert!(split_command_line("").is_err());
        assert!(split_command_line("   \t ").is_err());
        assert!(split_command_line("echo \"open").is_err());
        assert!(split_command_line("echo 'open").is_err());
        assert!(split_command_line("echo open\\").is_err());
    }

    #[test]
    fn test_split_command_line_keeps_hash_arguments() {
        assert_eq!(
            split_command_line("echo a #b").unwrap(),
            ["echo", "a", "#b"]
        );
        assert_eq!(split_command_line("#tag").unwrap(), ["#tag"]);
    }

    fn action(service: Option<&str>, working_directory: Option<&str>) -> CustomAction {
        CustomAction::validated(
            "Seed",
            service,
            vec![
                "php".to_string(),
                "artisan".to_string(),
                "db:seed".to_string(),
            ],
            working_directory,
            CustomActionMode::Terminal,
            false,
        )
        .unwrap()
    }

    #[test]
    fn test_custom_action_command() {
        let sail = sail_invocation(&["compose.yaml"]);
        let app = custom_action_command(&sail, true, "laravel.test", &action(None, None)).unwrap();
        assert_eq!(
            app.args,
            [
                "exec",
                "-u",
                "sail",
                "laravel.test",
                "php",
                "artisan",
                "db:seed"
            ]
        );
        let with_directory = custom_action_command(
            &sail,
            true,
            "laravel.test",
            &action(Some("laravel.test"), Some("/var/www/html")),
        )
        .unwrap();
        assert_eq!(
            with_directory.args,
            [
                "exec",
                "-u",
                "sail",
                "-w",
                "/var/www/html",
                "laravel.test",
                "php",
                "artisan",
                "db:seed"
            ]
        );
        let other =
            custom_action_command(&sail, true, "laravel.test", &action(Some("mysql"), None))
                .unwrap();
        assert_eq!(other.args, ["exec", "mysql", "php", "artisan", "db:seed"]);
        let compose =
            custom_action_command(&invocation(&[]), false, "app", &action(None, None)).unwrap();
        assert!(!compose.args.contains(&"-u".to_string()));
        assert!(compose.args.ends_with(&[
            "exec".into(),
            "app".into(),
            "php".into(),
            "artisan".into(),
            "db:seed".into()
        ]));
        assert_eq!(compose.program, PathBuf::from("/usr/bin/docker"));
    }

    #[test]
    fn test_custom_action_validation() {
        let valid = |name: &str, service: Option<&str>, command: &[&str]| {
            CustomAction::validated(
                name,
                service,
                command
                    .iter()
                    .map(|argument| argument.to_string())
                    .collect(),
                None,
                CustomActionMode::Terminal,
                false,
            )
        };
        assert!(valid("Seed", None, &["ls"]).is_ok());
        assert!(valid("  ", None, &["ls"]).is_err());
        assert!(valid("Seed", None, &[]).is_err());
        assert!(valid("Seed", None, &[""]).is_err());
        assert!(valid("Seed", Some("bad name"), &["ls"]).is_err());
        assert!(valid("Seed", Some("--rm"), &["ls"]).is_err());
        assert_eq!(valid("Seed", Some(" "), &["ls"]).unwrap().service, None);
        let bad = CustomAction {
            service: Some("bad name".to_string()),
            ..action(None, None)
        };
        assert!(custom_action_command(&sail_invocation(&[]), true, "laravel.test", &bad).is_err());
    }

    #[test]
    fn test_generated_override_is_last_in_sail_files() {
        let mut invocation = sail_invocation(&["compose.yaml", "compose.override.yaml"]);
        invocation.generated_override =
            Some(PathBuf::from("/data/sail_manager/abc/ports.override.yaml"));
        let expected =
            "compose.yaml:compose.override.yaml:/data/sail_manager/abc/ports.override.yaml";
        for command in [
            invocation.config_command(),
            invocation.ps_command(),
            invocation.up_command(),
            invocation.stop_command(),
            invocation.restart_command(),
            invocation.events_command(),
            invocation
                .service_command(ServiceOperation::Start, "mysql")
                .unwrap(),
            invocation.logs_command("redis", true, 10).unwrap(),
            invocation
                .exec_command("mysql", None, None, false, &["ls".to_string()])
                .unwrap(),
        ] {
            assert!(
                command
                    .environment
                    .contains(&("SAIL_FILES".to_string(), expected.to_string())),
                "{command:?}"
            );
        }
    }

    #[test]
    fn test_generated_override_alone_sets_sail_files() {
        let mut invocation = sail_invocation(&[]);
        invocation.generated_override = Some(PathBuf::from("/data/ports.override.yaml"));
        assert!(invocation.up_command().environment.contains(&(
            "SAIL_FILES".to_string(),
            "/data/ports.override.yaml".to_string()
        )));
        assert!(
            !sail_invocation(&[])
                .up_command()
                .environment
                .iter()
                .any(|(name, _)| name == "SAIL_FILES")
        );
    }

    #[test]
    fn test_generated_override_is_a_file_flag_for_docker_compose() {
        let mut invocation = invocation(&["compose.yaml", "compose.override.yaml"]);
        invocation.generated_override = Some(PathBuf::from("/data/ports.override.yaml"));
        let expected = [
            "compose",
            "--project-directory",
            "/work/blog",
            "-f",
            "compose.yaml",
            "-f",
            "compose.override.yaml",
            "-f",
            "/data/ports.override.yaml",
        ];
        for command in [
            invocation.up_command(),
            invocation.ps_command(),
            invocation.events_command(),
            invocation.stop_command(),
        ] {
            assert_eq!(command.args[..expected.len()], expected);
        }
        assert!(invocation.up_command().environment.is_empty());
        assert_eq!(
            invocation.without_generated_override().generated_override,
            None
        );
    }

    #[test]
    fn test_compose_files_all_lists_primary_first() {
        let files = ComposeFiles {
            primary: "compose.yaml".to_string(),
            overrides: vec!["compose.override.yaml".to_string()],
        };
        assert_eq!(files.all(), ["compose.yaml", "compose.override.yaml"]);
    }
}
