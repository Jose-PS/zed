use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::Result;
use fs::Fs;
use futures::future::BoxFuture;
use futures::io::{AsyncBufReadExt as _, BufReader};
use futures::{Stream, StreamExt as _};
use util::command::{Child, Stdio, new_command};

use crate::compose::{
    self, ComposeCommand, ComposeFiles, ComposeInvocation, EnvironmentState, NodePackageManager,
    Service, ServiceOperation,
};
use crate::ports::{
    self, AssignmentRegistry, AssignmentStore, PortAssignment, PortConflict, PortProbe,
};
use collections::HashSet;
use util::ResultExt as _;

const MAXIMUM_DETAIL_LINES: usize = 20;
const MAXIMUM_DETAIL_CHARACTERS: usize = 2000;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status_success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub struct LineStream {
    _child: Option<Child>,
    lines: Pin<Box<dyn Stream<Item = String> + Send>>,
}

#[cfg(test)]
impl LineStream {
    pub(crate) fn pending() -> Self {
        Self {
            _child: None,
            lines: Box::pin(futures::stream::pending()),
        }
    }
}

impl Stream for LineStream {
    type Item = String;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<String>> {
        self.lines.as_mut().poll_next(cx)
    }
}

pub trait CommandRunner: Send + Sync + 'static {
    fn run(
        &self,
        program: &Path,
        args: Vec<String>,
        environment: HashMap<String, String>,
        working_directory: PathBuf,
    ) -> BoxFuture<'static, Result<CommandOutput>>;

    fn stream_lines(
        &self,
        program: &Path,
        args: Vec<String>,
        environment: HashMap<String, String>,
        working_directory: PathBuf,
    ) -> Result<LineStream>;
}

pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(
        &self,
        program: &Path,
        args: Vec<String>,
        environment: HashMap<String, String>,
        working_directory: PathBuf,
    ) -> BoxFuture<'static, Result<CommandOutput>> {
        let program = program.to_path_buf();
        Box::pin(async move {
            let output = new_command(&program)
                .args(&args)
                .envs(&environment)
                .current_dir(&working_directory)
                .stdin(Stdio::null())
                .output()
                .await?;
            Ok(CommandOutput {
                status_success: output.status.success(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }

    fn stream_lines(
        &self,
        program: &Path,
        args: Vec<String>,
        environment: HashMap<String, String>,
        working_directory: PathBuf,
    ) -> Result<LineStream> {
        let mut child = new_command(program)
            .args(&args)
            .envs(&environment)
            .current_dir(&working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("child process has no stdout"))?;
        let lines = BufReader::new(stdout)
            .lines()
            .filter_map(|line| async move { line.ok() });
        Ok(LineStream {
            _child: Some(child),
            lines: Box::pin(lines),
        })
    }
}

pub fn find_docker_binary(
    path_variable: Option<&str>,
    working_directory: &Path,
) -> Option<PathBuf> {
    which::which_in("docker", path_variable, working_directory).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerError {
    DockerNotFound,
    ComposeNotAvailable,
    DaemonUnavailable {
        detail: String,
    },
    CommandFailed {
        command: String,
        exit_code: Option<i32>,
        detail: String,
    },
}

impl fmt::Display for DockerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DockerError::DockerNotFound => {
                write!(formatter, "Docker is not installed or not in PATH")
            }
            DockerError::ComposeNotAvailable => {
                write!(formatter, "Docker Compose v2 is not available")
            }
            DockerError::DaemonUnavailable { .. } => {
                write!(formatter, "Docker daemon is not reachable")
            }
            DockerError::CommandFailed { command, .. } => write!(formatter, "`{command}` failed"),
        }
    }
}

impl std::error::Error for DockerError {}

fn error_detail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(MAXIMUM_DETAIL_LINES);
    let joined = lines.get(start..).unwrap_or_default().join("\n");
    let total_characters = joined.chars().count();
    if total_characters <= MAXIMUM_DETAIL_CHARACTERS {
        joined
    } else {
        joined
            .chars()
            .skip(total_characters - MAXIMUM_DETAIL_CHARACTERS)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ProjectSnapshot {
    pub project_name: String,
    pub services: Vec<Service>,
    pub environment_state: EnvironmentState,
}

#[derive(Clone)]
pub struct ComposeClient {
    runner: Arc<dyn CommandRunner>,
    docker_binary: PathBuf,
    environment: HashMap<String, String>,
    invocation: ComposeInvocation,
}

impl ComposeClient {
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        docker_binary: PathBuf,
        environment: HashMap<String, String>,
        invocation: ComposeInvocation,
    ) -> Self {
        Self {
            runner,
            docker_binary,
            environment,
            invocation,
        }
    }

    pub fn invocation(&self) -> &ComposeInvocation {
        &self.invocation
    }

    pub fn environment(&self) -> &HashMap<String, String> {
        &self.environment
    }

    fn merged_environment(&self, command: &ComposeCommand) -> HashMap<String, String> {
        let mut environment = self.environment.clone();
        environment.extend(command.environment.iter().cloned());
        environment
    }

    async fn execute_docker(
        &self,
        args: Vec<String>,
    ) -> std::result::Result<CommandOutput, DockerError> {
        self.runner
            .run(
                &self.docker_binary,
                args,
                self.environment.clone(),
                self.invocation.project_directory.clone(),
            )
            .await
            .map_err(|_| DockerError::DockerNotFound)
    }

    async fn execute(
        &self,
        command: ComposeCommand,
    ) -> std::result::Result<CommandOutput, DockerError> {
        let environment = self.merged_environment(&command);
        self.runner
            .run(
                &command.program,
                command.args,
                environment,
                self.invocation.project_directory.clone(),
            )
            .await
            .map_err(|_| DockerError::DockerNotFound)
    }

    async fn execute_checked(&self, command: ComposeCommand, label: &str) -> Result<CommandOutput> {
        let output = self
            .execute(command)
            .await
            .map_err(|_| DockerError::CommandFailed {
                command: label.to_string(),
                exit_code: None,
                detail: "the docker process could not be started".to_string(),
            })?;
        if output.status_success {
            Ok(output)
        } else {
            Err(DockerError::CommandFailed {
                command: label.to_string(),
                exit_code: output.exit_code,
                detail: error_detail(&output.stderr),
            }
            .into())
        }
    }

    pub async fn check_availability(&self) -> Result<String, DockerError> {
        let version = self
            .execute_docker(vec![
                "compose".to_string(),
                "version".to_string(),
                "--short".to_string(),
            ])
            .await?;
        if !version.status_success {
            return Err(DockerError::ComposeNotAvailable);
        }
        let info = self
            .execute_docker(vec![
                "info".to_string(),
                "--format".to_string(),
                "{{.ServerVersion}}".to_string(),
            ])
            .await?;
        if !info.status_success {
            return Err(DockerError::DaemonUnavailable {
                detail: error_detail(&info.stderr),
            });
        }
        Ok(version.stdout.trim().to_string())
    }

    pub async fn config(&self) -> Result<compose::ComposeConfig> {
        let output = self
            .execute_checked(self.invocation.config_command(), "docker compose config")
            .await?;
        compose::parse_compose_config(&output.stdout)
    }

    pub async fn containers(&self) -> Result<Vec<compose::ContainerEntry>> {
        let output = self
            .execute_checked(self.invocation.ps_command(), "docker compose ps")
            .await?;
        compose::parse_compose_ps(&output.stdout)
    }

    pub async fn snapshot(&self, app_service: &str) -> Result<ProjectSnapshot> {
        let config = self.config().await?;
        let containers = self.containers().await?;
        let services = compose::build_services(&config, &containers, app_service);
        let environment_state = compose::environment_state(&services);
        Ok(ProjectSnapshot {
            project_name: config.name,
            services,
            environment_state,
        })
    }

    pub async fn up(&self) -> Result<()> {
        self.execute_checked(self.invocation.up_command(), "docker compose up")
            .await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.execute_checked(self.invocation.stop_command(), "docker compose stop")
            .await?;
        Ok(())
    }

    pub async fn restart(&self) -> Result<()> {
        self.execute_checked(self.invocation.restart_command(), "docker compose restart")
            .await?;
        Ok(())
    }

    pub async fn service_operation(
        &self,
        operation: ServiceOperation,
        service: &str,
    ) -> Result<()> {
        let command = self.invocation.service_command(operation, service)?;
        let label = match operation {
            ServiceOperation::Start => "docker compose up",
            ServiceOperation::Stop => "docker compose stop",
            ServiceOperation::Restart => "docker compose restart",
        };
        self.execute_checked(command, label).await?;
        Ok(())
    }

    pub fn reconfigured(
        &self,
        files: Vec<String>,
        generated_override: Option<PathBuf>,
    ) -> ComposeClient {
        let mut client = self.clone();
        client.invocation.files = files;
        client.invocation.generated_override = generated_override;
        client
    }

    pub async fn system_published_ports(&self) -> Result<Vec<(Option<String>, u16)>> {
        let output = self
            .execute_docker(vec![
                "ps".to_string(),
                "--format".to_string(),
                "{{json .}}".to_string(),
            ])
            .await?;
        if !output.status_success {
            return Err(DockerError::CommandFailed {
                command: "docker ps".to_string(),
                exit_code: output.exit_code,
                detail: error_detail(&output.stderr),
            }
            .into());
        }
        ports::parse_docker_ps_ports(&output.stdout)
    }

    pub async fn available_shells(&self, service: &Service) -> Result<Vec<&'static str>> {
        let is_sail = matches!(
            self.invocation.launcher,
            compose::ComposeLauncher::Sail { .. }
        );
        let command = self.invocation.exec_command(
            &service.name,
            compose::app_exec_user(&service.kind, is_sail),
            None,
            true,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "command -v zsh bash sh".to_string(),
            ],
        )?;
        match self.execute(command).await {
            Ok(output) if output.status_success => {
                Ok(compose::parse_available_shells(&output.stdout))
            }
            _ => Ok(Vec::new()),
        }
    }

    pub fn events(&self) -> Result<LineStream> {
        let command = self.invocation.events_command();
        let environment = self.merged_environment(&command);
        self.runner.stream_lines(
            &command.program,
            command.args,
            environment,
            self.invocation.project_directory.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartTarget {
    Environment,
    Service(String),
}

#[derive(Clone)]
pub struct PortContext {
    pub fs: Arc<dyn Fs>,
    pub store: Arc<dyn AssignmentStore>,
    pub probe: Arc<dyn PortProbe>,
    pub root_key: String,
    pub override_path: PathBuf,
    pub configured_files: Vec<String>,
    pub base_files: Vec<String>,
    pub pins: collections::HashMap<String, u16>,
    pub auto_assign: bool,
    pub compose_supported: bool,
}

pub struct StartReport {
    pub moved: Option<String>,
    pub result: Result<()>,
}

const MAXIMUM_PORT_RETRIES: usize = 2;

pub struct PortSettings {
    pub configured_files: Vec<String>,
    pub base_files: Vec<String>,
    pub pins: collections::HashMap<String, u16>,
    pub auto_assign: bool,
    pub compose_supported: bool,
}

pub async fn canonical_root(fs: &dyn Fs, root: &Path) -> PathBuf {
    fs.canonicalize(root)
        .await
        .unwrap_or_else(|_| root.to_path_buf())
}

impl PortContext {
    pub async fn new(
        fs: Arc<dyn Fs>,
        store: Arc<dyn AssignmentStore>,
        probe: Arc<dyn PortProbe>,
        root: &Path,
        settings: PortSettings,
    ) -> Self {
        let canonical = canonical_root(fs.as_ref(), root).await;
        Self {
            fs,
            store,
            probe,
            root_key: canonical.to_string_lossy().into_owned(),
            override_path: ports::generated_override_path(&canonical),
            configured_files: settings.configured_files,
            base_files: settings.base_files,
            pins: settings.pins,
            auto_assign: settings.auto_assign,
            compose_supported: settings.compose_supported,
        }
    }

    pub async fn reset(&self) -> Result<()> {
        let mut registry: AssignmentRegistry = self.store.load().await?;
        if registry.remove(&self.root_key).is_some() {
            self.store.save(registry).await?;
        }
        self.remove_override().await
    }

    async fn remove_override(&self) -> Result<()> {
        if !self.fs.is_file(&self.override_path).await {
            return Ok(());
        }
        self.fs
            .remove_file(
                &self.override_path,
                fs::RemoveOptions {
                    recursive: false,
                    ignore_if_not_exists: true,
                },
            )
            .await
    }

    async fn write_override(&self, yaml: Option<&str>) -> Result<()> {
        match yaml {
            Some(yaml) => {
                if let Some(parent) = self.override_path.parent() {
                    self.fs.create_dir(parent).await?;
                }
                self.fs.write(&self.override_path, yaml.as_bytes()).await
            }
            None => self.remove_override().await,
        }
    }

    async fn save_assignments(&self, assignments: Vec<PortAssignment>) -> Result<()> {
        let mut registry: AssignmentRegistry =
            self.store.load().await.log_err().unwrap_or_default();
        registry.insert(self.root_key.clone(), assignments);
        self.store.save(registry).await
    }
}

async fn start_client(client: &ComposeClient, target: &StartTarget) -> Result<()> {
    match target {
        StartTarget::Environment => client.up().await,
        StartTarget::Service(service) => {
            client
                .service_operation(ServiceOperation::Start, service)
                .await
        }
    }
}

fn running_host_ports(containers: &[compose::ContainerEntry]) -> HashSet<u16> {
    containers
        .iter()
        .filter(|container| container.state.eq_ignore_ascii_case("running"))
        .flat_map(|container| container.publishers.iter())
        .map(|publisher| publisher.published_port)
        .filter(|port| *port != 0)
        .collect()
}

pub async fn start_with_ports(
    client: &ComposeClient,
    target: &StartTarget,
    context: &PortContext,
) -> StartReport {
    if !context.compose_supported || !context.auto_assign {
        let keep_override = context.compose_supported
            && !context.pins.is_empty()
            && client.invocation().generated_override.is_some();
        if keep_override {
            return StartReport {
                moved: None,
                result: start_client(client, target).await,
            };
        }
        let removal = context.remove_override().await;
        let plain = client.reconfigured(context.configured_files.clone(), None);
        let result = match removal {
            Ok(()) => start_client(&plain, target).await,
            Err(error) => Err(error),
        };
        return StartReport {
            moved: None,
            result,
        };
    }
    match start_with_assignments(client, target, context).await {
        Ok(report) => report,
        Err(error) => StartReport {
            moved: None,
            result: Err(error),
        },
    }
}

async fn start_with_assignments(
    client: &ComposeClient,
    target: &StartTarget,
    context: &PortContext,
) -> Result<StartReport> {
    let plain = client.reconfigured(context.configured_files.clone(), None);
    let config = plain.config().await?;
    let requests = ports::requests_from_config(&config);
    let ports_by_service: std::collections::BTreeMap<String, Vec<compose::PortConfig>> = config
        .services
        .iter()
        .map(|(name, service)| (name.clone(), service.ports.clone()))
        .collect();

    let registry = context.store.load().await.log_err().unwrap_or_default();
    let mut previous = registry.get(&context.root_key).cloned().unwrap_or_default();
    let mut reserved = ports::reserved_by_other_roots(&registry, &context.root_key);
    let system_ports = plain
        .system_published_ports()
        .await
        .log_err()
        .unwrap_or_default();
    reserved.extend(
        system_ports
            .iter()
            .filter(|(label, _)| label.as_deref() != Some(config.name.as_str()))
            .map(|(_, port)| *port),
    );
    let mut own_ports = plain
        .containers()
        .await
        .log_err()
        .map(|containers| running_host_ports(&containers))
        .unwrap_or_default();

    let mut assignments =
        ports::assign_ports(&requests, &previous, &context.pins, &reserved, &|port| {
            own_ports.contains(&port) || context.probe.is_free(port)
        })
        .map_err(anyhow::Error::new)?;
    let mut attempt = 0;
    loop {
        let yaml = ports::override_yaml(&assignments, &ports_by_service)?;
        context.write_override(yaml.as_deref()).await?;
        context.save_assignments(assignments.clone()).await?;
        let applied = if yaml.is_some() {
            client.reconfigured(
                context.base_files.clone(),
                Some(context.override_path.clone()),
            )
        } else {
            plain.clone()
        };
        let moved = ports::moved_ports_message(&assignments);
        let result = start_client(&applied, target).await;
        if result.is_ok() || attempt >= MAXIMUM_PORT_RETRIES {
            return Ok(StartReport { moved, result });
        }
        if let Some(containers) = plain.containers().await.log_err() {
            own_ports = running_host_ports(&containers);
        }
        let is_free = |port: u16| own_ports.contains(&port) || context.probe.is_free(port);
        let culprits: Vec<&PortAssignment> = assignments
            .iter()
            .filter(|assignment| !is_free(assignment.host_port))
            .collect();
        if culprits.is_empty() {
            return Ok(StartReport { moved, result });
        }
        if let Some(pinned) = culprits.iter().find(|assignment| assignment.pinned) {
            return Err(PortConflict {
                key: pinned.key.clone(),
                port: pinned.host_port,
            }
            .into());
        }
        reserved.extend(culprits.iter().map(|assignment| assignment.host_port));
        previous = assignments.clone();
        assignments = ports::assign_ports(&requests, &previous, &context.pins, &reserved, &is_free)
            .map_err(anyhow::Error::new)?;
        attempt += 1;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFiles {
    pub root_file_names: Vec<String>,
    pub composer_json: Option<String>,
    pub has_vendor_sail_binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDetection {
    pub compose_files: Option<ComposeFiles>,
    pub is_sail: bool,
    pub has_vendor_sail_binary: bool,
    pub node_package_manager: Option<NodePackageManager>,
}

pub async fn read_project_files(fs: &dyn Fs, root: &Path) -> Result<ProjectFiles> {
    let mut root_file_names = Vec::new();
    let mut entries = fs.read_dir(root).await?;
    while let Some(entry) = entries.next().await {
        let path = entry?;
        if let Some(name) = path.file_name() {
            root_file_names.push(name.to_string_lossy().into_owned());
        }
    }
    let composer_path = root.join("composer.json");
    let composer_json = if fs.is_file(&composer_path).await {
        Some(fs.load(&composer_path).await?)
    } else {
        None
    };
    let has_vendor_sail_binary = fs.is_file(&root.join("vendor/bin/sail")).await;
    Ok(ProjectFiles {
        root_file_names,
        composer_json,
        has_vendor_sail_binary,
    })
}

impl ProjectFiles {
    pub fn detect(&self) -> ProjectDetection {
        let names: Vec<&str> = self.root_file_names.iter().map(String::as_str).collect();
        ProjectDetection {
            compose_files: compose::detect_compose_files(&names),
            is_sail: compose::is_sail_project(
                self.composer_json.as_deref(),
                self.has_vendor_sail_binary,
            ),
            has_vendor_sail_binary: self.has_vendor_sail_binary,
            node_package_manager: compose::detect_node_package_manager(&names),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::ComposeLauncher;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use std::sync::Mutex;

    const CONFIG: &str = r#"{
        "name": "blog",
        "services": {
            "laravel.test": {"image": "sail-8.4/app", "environment": {"DB_PASSWORD": "super-secret-password"}, "ports": [{"mode": "ingress", "target": 80, "published": "80", "protocol": "tcp"}]},
            "mysql": {"image": "mysql/mysql-server:8.0", "ports": [{"mode": "ingress", "target": 3306, "published": "3306", "protocol": "tcp"}]}
        }
    }"#;

    const PS: &str = concat!(
        r#"{"Name":"blog-laravel.test-1","Service":"laravel.test","State":"running","Status":"Up 2 hours","Health":"","ExitCode":0,"Image":"sail-8.4/app","Publishers":[{"URL":"0.0.0.0","TargetPort":80,"PublishedPort":80,"Protocol":"tcp"}]}"#,
        "\n",
        r#"{"Name":"blog-mysql-1","Service":"mysql","State":"running","Status":"Up 2 hours (healthy)","Health":"healthy","ExitCode":0,"Publishers":null}"#,
        "\n",
    );

    type RecordedCalls = Arc<Mutex<Vec<(PathBuf, Vec<String>, HashMap<String, String>)>>>;

    type Responder = Box<dyn Fn(&[String]) -> CommandOutput + Send + Sync>;

    struct FakeCommandRunner {
        responder: Responder,
        calls: RecordedCalls,
    }

    impl FakeCommandRunner {
        fn new(
            responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
        ) -> (Arc<Self>, RecordedCalls) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let runner = Arc::new(Self {
                responder: Box::new(responder),
                calls: calls.clone(),
            });
            (runner, calls)
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(
            &self,
            program: &Path,
            args: Vec<String>,
            environment: HashMap<String, String>,
            _working_directory: PathBuf,
        ) -> BoxFuture<'static, Result<CommandOutput>> {
            let output = (self.responder)(&args);
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((program.to_path_buf(), args, environment));
            }
            Box::pin(async move { Ok(output) })
        }

        fn stream_lines(
            &self,
            _program: &Path,
            _args: Vec<String>,
            _environment: HashMap<String, String>,
            _working_directory: PathBuf,
        ) -> Result<LineStream> {
            anyhow::bail!("not supported by the fake runner")
        }
    }

    fn success(stdout: &str) -> CommandOutput {
        CommandOutput {
            status_success: true,
            exit_code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn failure(exit_code: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            status_success: false,
            exit_code: Some(exit_code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    fn subcommand(args: &[String]) -> &str {
        match args.first().map(String::as_str) {
            Some("info") => "info",
            _ => args
                .iter()
                .map(String::as_str)
                .find(|argument| {
                    ["version", "config", "ps", "up", "stop", "restart", "events"]
                        .contains(argument)
                })
                .unwrap_or(""),
        }
    }

    fn client_with(
        responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
    ) -> (ComposeClient, RecordedCalls) {
        let (runner, calls) = FakeCommandRunner::new(responder);
        let environment = HashMap::from([("DB_PASSWORD".to_string(), "env-secret".to_string())]);
        let client = ComposeClient::new(
            runner,
            PathBuf::from("/usr/bin/docker"),
            environment,
            ComposeInvocation {
                launcher: ComposeLauncher::DockerCompose {
                    docker_binary: PathBuf::from("/usr/bin/docker"),
                },
                project_directory: PathBuf::from("/work/blog"),
                files: vec!["compose.yaml".to_string()],
                generated_override: None,
            },
        );
        (client, calls)
    }

    fn sail_client_with(
        responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
    ) -> (ComposeClient, RecordedCalls) {
        let (runner, calls) = FakeCommandRunner::new(responder);
        let environment = HashMap::from([("DB_PASSWORD".to_string(), "env-secret".to_string())]);
        let client = ComposeClient::new(
            runner,
            PathBuf::from("/usr/bin/docker"),
            environment,
            ComposeInvocation {
                launcher: ComposeLauncher::Sail {
                    sail_script: PathBuf::from("/work/blog/vendor/bin/sail"),
                },
                project_directory: PathBuf::from("/work/blog"),
                files: vec!["compose.yaml".to_string()],
                generated_override: None,
            },
        );
        (client, calls)
    }

    fn healthy_responder(args: &[String]) -> CommandOutput {
        match subcommand(args) {
            "version" => success("2.29.1\n"),
            "info" => success("27.0.3\n"),
            "config" => success(CONFIG),
            "ps" => success(PS),
            _ => success(""),
        }
    }

    #[test]
    fn test_sail_snapshot_runs_the_sail_script() {
        let (client, calls) = sail_client_with(healthy_responder);
        smol::block_on(client.snapshot("laravel.test")).unwrap();
        smol::block_on(client.check_availability()).unwrap();
        let calls = calls.lock().unwrap();
        let sail = PathBuf::from("/work/blog/vendor/bin/sail");
        assert_eq!(calls[0].0, sail);
        assert_eq!(calls[0].1, ["config", "--format", "json"]);
        assert_eq!(calls[1].0, sail);
        assert_eq!(calls[1].1, ["ps", "-a", "--format", "json"]);
        assert_eq!(
            calls[0].2.get("SAIL_FILES").map(String::as_str),
            Some("compose.yaml")
        );
        assert_eq!(
            calls[0].2.get("DB_PASSWORD").map(String::as_str),
            Some("env-secret")
        );
        assert_eq!(calls[2].0, PathBuf::from("/usr/bin/docker"));
        assert_eq!(calls[2].1, ["compose", "version", "--short"]);
        assert_eq!(calls[3].0, PathBuf::from("/usr/bin/docker"));
    }

    #[test]
    fn test_docker_snapshot_runs_the_docker_binary() {
        let (client, calls) = client_with(healthy_responder);
        smol::block_on(client.snapshot("laravel.test")).unwrap();
        let calls = calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .all(|(program, _, _)| program == &PathBuf::from("/usr/bin/docker"))
        );
    }

    #[test]
    fn test_check_availability_ok() {
        let (client, calls) = client_with(healthy_responder);
        let version = smol::block_on(client.check_availability());
        assert_eq!(version, Ok("2.29.1".to_string()));
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].1, ["compose", "version", "--short"]);
        assert_eq!(calls[1].1, ["info", "--format", "{{.ServerVersion}}"]);
    }

    #[test]
    fn test_check_availability_without_compose() {
        let (client, calls) = client_with(|args| match subcommand(args) {
            "version" => failure(125, "", "docker: 'compose' is not a docker command."),
            _ => success(""),
        });
        assert_eq!(
            smol::block_on(client.check_availability()),
            Err(DockerError::ComposeNotAvailable)
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_check_availability_daemon_down_does_not_depend_on_language() {
        let (client, _) = client_with(|args| match subcommand(args) {
            "version" => success("2.29.1"),
            _ => failure(1, "", "No se puede conectar al daemon de Docker"),
        });
        assert_eq!(
            smol::block_on(client.check_availability()),
            Err(DockerError::DaemonUnavailable {
                detail: "No se puede conectar al daemon de Docker".to_string()
            })
        );
    }

    #[test]
    fn test_snapshot_combines_config_and_ps() {
        let (client, _) = client_with(healthy_responder);
        let snapshot = smol::block_on(client.snapshot("laravel.test")).unwrap();
        assert_eq!(snapshot.project_name, "blog");
        assert_eq!(snapshot.services.len(), 2);
        assert_eq!(snapshot.environment_state, EnvironmentState::Running);
        assert!(
            snapshot
                .services
                .iter()
                .all(|service| service.state.is_running())
        );
    }

    #[test]
    fn test_ps_failure_is_command_failed_with_trimmed_detail() {
        let long_line = "x".repeat(3000);
        let mut stderr = String::new();
        for index in 0..30 {
            stderr.push_str(&format!("line {index}\n\n"));
        }
        stderr.push_str(&long_line);
        let (client, _) = client_with(move |args| match subcommand(args) {
            "config" => success(CONFIG),
            _ => failure(17, "", &stderr),
        });
        let error = smol::block_on(client.snapshot("laravel.test")).unwrap_err();
        match error.downcast_ref::<DockerError>() {
            Some(DockerError::CommandFailed {
                command,
                exit_code,
                detail,
            }) => {
                assert_eq!(command, "docker compose ps");
                assert_eq!(*exit_code, Some(17));
                assert_eq!(detail.chars().count(), MAXIMUM_DETAIL_CHARACTERS);
                assert!(detail.ends_with("xxx"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_error_detail_keeps_last_non_empty_lines() {
        let stderr: String = (0..30).map(|index| format!("line {index}\n\n")).collect();
        let detail = error_detail(&stderr);
        let lines: Vec<&str> = detail.lines().collect();
        assert_eq!(lines.len(), MAXIMUM_DETAIL_LINES);
        assert_eq!(lines.first(), Some(&"line 10"));
        assert_eq!(lines.last(), Some(&"line 29"));
    }

    #[test]
    fn test_stop_does_not_destroy() {
        let (client, calls) = client_with(healthy_responder);
        smol::block_on(client.stop()).unwrap();
        let calls = calls.lock().unwrap();
        let args = &calls[0].1;
        assert!(args.contains(&"stop".to_string()));
        assert!(!args.contains(&"down".to_string()));
        assert!(!args.contains(&"-v".to_string()));
    }

    #[test]
    fn test_service_operation_rejects_invalid_name() {
        let (client, calls) = client_with(healthy_responder);
        assert!(
            smol::block_on(client.service_operation(ServiceOperation::Restart, "a; rm -rf"))
                .is_err()
        );
        assert!(calls.lock().unwrap().is_empty());
        smol::block_on(client.service_operation(ServiceOperation::Restart, "mysql")).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_arguments_carry_project_directory_and_no_secrets() {
        let (client, calls) = client_with(healthy_responder);
        smol::block_on(client.snapshot("laravel.test")).unwrap();
        smol::block_on(client.up()).unwrap();
        let calls = calls.lock().unwrap();
        for (_, args, environment) in calls.iter() {
            assert!(args.contains(&"--project-directory".to_string()));
            assert_eq!(
                args.iter()
                    .filter(|argument| argument.contains("secret"))
                    .count(),
                0
            );
            assert_eq!(
                environment.get("DB_PASSWORD").map(String::as_str),
                Some("env-secret")
            );
        }
    }

    #[test]
    fn test_config_error_does_not_include_stdout() {
        let (client, _) = client_with(|_| failure(1, CONFIG, "invalid compose file"));
        let error = smol::block_on(client.config()).unwrap_err();
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("super-secret-password"));
        match error.downcast_ref::<DockerError>() {
            Some(DockerError::CommandFailed { detail, .. }) => {
                assert_eq!(detail, "invalid compose file")
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_invalid_config_json_error_has_no_output() {
        let (client, _) = client_with(|_| success("DB_PASSWORD=super-secret-password not json"));
        let error = smol::block_on(client.config()).unwrap_err();
        assert!(!format!("{error:?}").contains("super-secret-password"));
    }

    #[cfg(unix)]
    #[test]
    fn test_real_runner_run_and_stream_lines() {
        let runner = RealCommandRunner;
        let printf = PathBuf::from("/usr/bin/printf");
        if !printf.exists() {
            return;
        }
        let output = smol::block_on(runner.run(
            &printf,
            vec!["a\\nb\\n".to_string()],
            HashMap::new(),
            PathBuf::from("/"),
        ))
        .unwrap();
        assert!(output.status_success);
        assert_eq!(output.stdout, "a\nb\n");

        let stream = runner
            .stream_lines(
                &printf,
                vec!["a\\nb\\n".to_string()],
                HashMap::new(),
                PathBuf::from("/"),
            )
            .unwrap();
        let lines: Vec<String> = smol::block_on(stream.collect());
        assert_eq!(lines, ["a", "b"]);
    }

    #[cfg(unix)]
    #[test]
    fn test_real_runner_failure_reports_exit_code() {
        let runner = RealCommandRunner;
        let command = PathBuf::from("/usr/bin/false");
        if !command.exists() {
            return;
        }
        let output =
            smol::block_on(runner.run(&command, Vec::new(), HashMap::new(), PathBuf::from("/")))
                .unwrap();
        assert!(!output.status_success);
        assert_eq!(output.exit_code, Some(1));
    }

    #[gpui::test]
    async fn test_detect_sail_project(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/work/blog",
            json!({
                "compose.yaml": "services: {}",
                "composer.json": r#"{"require-dev": {"laravel/sail": "^1.0"}}"#,
                "package-lock.json": "{}",
                "vendor": {"bin": {"sail": "#!/bin/sh"}},
            }),
        )
        .await;
        let files = read_project_files(fs.as_ref(), Path::new("/work/blog"))
            .await
            .unwrap();
        assert!(files.has_vendor_sail_binary);
        assert!(files.composer_json.is_some());
        let detection = files.detect();
        assert!(detection.is_sail);
        assert_eq!(
            detection.compose_files.map(|files| files.primary),
            Some("compose.yaml".to_string())
        );
        assert_eq!(
            detection.node_package_manager,
            Some(NodePackageManager::Npm)
        );
    }

    #[gpui::test]
    async fn test_detect_compose_without_sail(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/work/api",
            json!({
                "docker-compose.yml": "services: {}",
                "composer.json": r#"{"require": {}}"#,
            }),
        )
        .await;
        let files = read_project_files(fs.as_ref(), Path::new("/work/api"))
            .await
            .unwrap();
        let detection = files.detect();
        assert!(!detection.is_sail);
        assert_eq!(
            detection.compose_files.map(|files| files.primary),
            Some("docker-compose.yml".to_string())
        );
        assert_eq!(detection.node_package_manager, None);
    }

    #[gpui::test]
    async fn test_detect_folder_without_compose(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/work/empty", json!({"README.md": "hello"}))
            .await;
        let files = read_project_files(fs.as_ref(), Path::new("/work/empty"))
            .await
            .unwrap();
        assert_eq!(files.composer_json, None);
        assert!(!files.has_vendor_sail_binary);
        let detection = files.detect();
        assert_eq!(detection.compose_files, None);
        assert!(!detection.is_sail);
    }

    fn service_named(name: &str, kind: compose::ServiceKind) -> Service {
        Service {
            name: name.to_string(),
            image: None,
            kind,
            state: compose::ServiceState::Running { health: None },
            status_text: None,
            container_name: None,
            ports: Vec::new(),
        }
    }

    #[test]
    fn test_available_shells_parses_the_output() {
        let (client, calls) = client_with(|_| success("/usr/bin/zsh\n/bin/bash\n/bin/sh\n"));
        let shells = smol::block_on(
            client.available_shells(&service_named("mysql", compose::ServiceKind::Mysql)),
        )
        .unwrap();
        assert_eq!(shells, ["zsh", "bash", "sh"]);
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            [
                "compose",
                "--project-directory",
                "/work/blog",
                "-f",
                "compose.yaml",
                "exec",
                "-T",
                "mysql",
                "sh",
                "-c",
                "command -v zsh bash sh"
            ]
        );
    }

    #[test]
    fn test_available_shells_failure_is_empty() {
        let (client, _) = client_with(|_| failure(126, "", "exec: \"sh\": not found"));
        let shells = smol::block_on(
            client.available_shells(&service_named("redis", compose::ServiceKind::Redis)),
        )
        .unwrap();
        assert!(shells.is_empty());
    }

    #[test]
    fn test_available_shells_for_the_sail_app_uses_the_sail_user() {
        let (client, calls) = sail_client_with(|_| success("/bin/bash\n/bin/sh\n"));
        let shells = smol::block_on(
            client.available_shells(&service_named("laravel.test", compose::ServiceKind::App)),
        )
        .unwrap();
        assert_eq!(shells, ["bash", "sh"]);
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].0, PathBuf::from("/work/blog/vendor/bin/sail"));
        assert_eq!(
            calls[0].1,
            [
                "exec",
                "-T",
                "-u",
                "sail",
                "laravel.test",
                "sh",
                "-c",
                "command -v zsh bash sh"
            ]
        );
    }

    const FLOW_CONFIG: &str = r#"{
        "name": "blog",
        "services": {
            "laravel.test": {"image": "sail-8.4/app", "ports": [{"target": 80, "published": "80", "protocol": "tcp"}]},
            "mysql": {"image": "mysql/mysql-server:8.0", "ports": [{"target": 3306, "published": "3306", "protocol": "tcp"}]}
        }
    }"#;

    const RUNNING_PS: &str = concat!(
        r#"{"Name":"blog-laravel.test-1","Service":"laravel.test","State":"running","Status":"Up","Health":"","ExitCode":0,"Publishers":[{"URL":"0.0.0.0","TargetPort":80,"PublishedPort":80,"Protocol":"tcp"}]}"#,
        "\n",
    );

    fn is_system_ps(args: &[String]) -> bool {
        args.iter().any(|argument| argument == "{{json .}}")
    }

    fn flow_context(
        fs: Arc<dyn Fs>,
        store: Arc<dyn AssignmentStore>,
        probe: Arc<dyn PortProbe>,
    ) -> PortContext {
        PortContext {
            fs,
            store,
            probe,
            root_key: "/work/blog".to_string(),
            override_path: ports::override_file_path(Path::new("/data"), Path::new("/work/blog")),
            configured_files: Vec::new(),
            base_files: vec!["compose.yaml".to_string()],
            pins: collections::HashMap::default(),
            auto_assign: true,
            compose_supported: true,
        }
    }

    fn up_calls(calls: &RecordedCalls) -> Vec<(Vec<String>, HashMap<String, String>)> {
        calls
            .lock()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|(_, args, _)| args.iter().any(|argument| argument == "up"))
                    .map(|(_, args, environment)| (args.clone(), environment.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn subcommand_count(calls: &RecordedCalls, name: &str) -> usize {
        calls
            .lock()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|(_, args, _)| subcommand(args) == name)
                    .count()
            })
            .unwrap_or_default()
    }

    fn override_files(path: &Path) -> String {
        format!("compose.yaml:{}", path.to_string_lossy())
    }

    #[gpui::test]
    async fn test_start_remaps_ports_writes_the_override_and_runs_sail_with_it(
        cx: &mut TestAppContext,
    ) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let system_ps = concat!(
            r#"{"Labels":"com.docker.compose.project=shop","Ports":"0.0.0.0:3306->3306/tcp"}"#,
            "\n",
            r#"{"Labels":"com.docker.compose.project=blog","Ports":"0.0.0.0:6379->6379/tcp"}"#,
            "\n",
        );
        let (client, calls) = sail_client_with(move |args| {
            if is_system_ps(args) {
                success(system_ps)
            } else {
                match subcommand(args) {
                    "config" => success(FLOW_CONFIG),
                    "ps" => success(""),
                    _ => success(""),
                }
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs.clone(), store.clone(), probe);

        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert_eq!(
            report.moved.as_deref(),
            Some("laravel.test: 80 \u{2192} 8080, mysql: 3306 \u{2192} 3307")
        );

        let written = fs.load(&context.override_path).await.unwrap();
        assert_eq!(
            written,
            "services:\n  laravel.test:\n    ports: !override\n      - \"8080:80\"\n  mysql:\n    ports: !override\n      - \"3307:3306\"\n"
        );
        let up_calls = up_calls(&calls);
        assert_eq!(up_calls.len(), 1);
        assert_eq!(up_calls[0].0, ["up", "-d"]);
        assert_eq!(
            up_calls[0].1.get("SAIL_FILES"),
            Some(&override_files(&context.override_path))
        );
        let registry = store.snapshot();
        let stored = registry.get("/work/blog").unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|assignment| assignment.host_port)
                .collect::<Vec<_>>(),
            [8080, 3307]
        );
        assert_eq!(stored[0].original_host_port, 80);
    }

    #[gpui::test]
    async fn test_start_without_conflicts_does_not_write_an_override(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[]);
        let (client, calls) = sail_client_with(|args| {
            if is_system_ps(args) {
                success("")
            } else if subcommand(args) == "config" {
                success(FLOW_CONFIG)
            } else {
                success("")
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs.clone(), store, probe);
        fs.insert_tree("/data/sail_manager", json!({})).await;
        fs.write(&context.override_path, b"stale").await.unwrap();

        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert_eq!(report.moved, None);
        assert!(!fs.is_file(&context.override_path).await);
        let up_calls = up_calls(&calls);
        assert_eq!(up_calls.len(), 1);
        assert!(!up_calls[0].1.contains_key("SAIL_FILES"));
    }

    #[gpui::test]
    async fn test_service_start_uses_the_same_override(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let (client, calls) = sail_client_with(|args| {
            if is_system_ps(args) {
                success("")
            } else if subcommand(args) == "config" {
                success(FLOW_CONFIG)
            } else {
                success("")
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs, store, probe);
        let report = start_with_ports(
            &client,
            &StartTarget::Service("laravel.test".to_string()),
            &context,
        )
        .await;
        assert!(report.result.is_ok());
        let up_calls = up_calls(&calls);
        assert_eq!(up_calls[0].0, ["up", "-d", "laravel.test"]);
        assert_eq!(
            up_calls[0].1.get("SAIL_FILES"),
            Some(&override_files(&context.override_path))
        );
    }

    #[gpui::test]
    async fn test_start_retries_with_another_port_when_one_gets_taken(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let busy = probe.busy.clone();
        let attempts = Arc::new(Mutex::new(0usize));
        let attempts_for_responder = attempts.clone();
        let (client, calls) = sail_client_with(move |args| {
            if is_system_ps(args) {
                return success("");
            }
            match subcommand(args) {
                "config" => success(FLOW_CONFIG),
                "up" => {
                    let mut attempts = attempts_for_responder.lock().unwrap();
                    *attempts += 1;
                    if *attempts == 1 {
                        busy.lock().unwrap().insert(8080);
                        failure(1, "", "port is already allocated")
                    } else {
                        success("")
                    }
                }
                _ => success(""),
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs.clone(), store.clone(), probe);

        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert_eq!(up_calls(&calls).len(), 2);
        let written = fs.load(&context.override_path).await.unwrap();
        assert!(written.contains("\"8081:80\""), "{written}");
        assert!(!written.contains("8080"), "{written}");
        assert_eq!(
            report.moved.as_deref(),
            Some("laravel.test: 80 \u{2192} 8081")
        );
    }

    #[gpui::test]
    async fn test_start_failure_without_a_busy_port_is_a_normal_error(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[]);
        let (client, calls) = sail_client_with(|args| {
            if is_system_ps(args) {
                success("")
            } else {
                match subcommand(args) {
                    "config" => success(FLOW_CONFIG),
                    "up" => failure(1, "", "no space left on device"),
                    _ => success(""),
                }
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs, store, probe);
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_err());
        assert_eq!(up_calls(&calls).len(), 1);
    }

    #[gpui::test]
    async fn test_start_retries_at_most_twice(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let busy = probe.busy.clone();
        let (client, calls) = sail_client_with(move |args| {
            if is_system_ps(args) {
                return success("");
            }
            match subcommand(args) {
                "config" => success(FLOW_CONFIG),
                "up" => {
                    let mut busy = busy.lock().unwrap();
                    let next = (8080..8200).find(|port| !busy.contains(port)).unwrap();
                    busy.insert(next);
                    failure(1, "", "port is already allocated")
                }
                _ => success(""),
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs, store, probe);
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_err());
        assert_eq!(up_calls(&calls).len(), 3);
    }

    #[gpui::test]
    async fn test_start_does_not_move_ports_of_running_containers(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let (client, calls) = sail_client_with(|args| {
            if is_system_ps(args) {
                success(concat!(
                    r#"{"Labels":"com.docker.compose.project=blog","Ports":"0.0.0.0:80->80/tcp"}"#,
                    "\n"
                ))
            } else {
                match subcommand(args) {
                    "config" => success(FLOW_CONFIG),
                    "ps" => success(RUNNING_PS),
                    _ => success(""),
                }
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let context = flow_context(fs.clone(), store, probe);
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert_eq!(report.moved, None);
        assert!(!fs.is_file(&context.override_path).await);
        assert_eq!(up_calls(&calls).len(), 1);
    }

    #[gpui::test]
    async fn test_start_with_a_pin_in_conflict_fails_without_running_up(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[9000]);
        let (client, calls) = sail_client_with(|args| {
            if is_system_ps(args) {
                success("")
            } else if subcommand(args) == "config" {
                success(FLOW_CONFIG)
            } else {
                success("")
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let mut context = flow_context(fs, store.clone(), probe);
        context.pins.insert("laravel.test:80/tcp".to_string(), 9000);
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        let error = report.result.unwrap_err();
        let conflict = error.downcast_ref::<PortConflict>().unwrap();
        assert_eq!(conflict.port, 9000);
        assert_eq!(conflict.to_string(), "Port 9000 for laravel.test is in use");
        assert!(up_calls(&calls).is_empty());
        assert!(store.snapshot().is_empty());
    }

    #[gpui::test]
    async fn test_start_respects_a_pin_and_reuses_the_previous_assignment(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        store.seed(
            "/work/blog",
            vec![ports::PortAssignment {
                key: ports::PortKey {
                    service: "mysql".to_string(),
                    container_port: 3306,
                    protocol: "tcp".to_string(),
                },
                host_ip: None,
                host_port: 3310,
                original_host_port: 3306,
                pinned: false,
            }],
        );
        let probe = ports::FakePortProbe::new(&[]);
        let (client, _) = sail_client_with(|args| {
            if is_system_ps(args) {
                success("")
            } else if subcommand(args) == "config" {
                success(FLOW_CONFIG)
            } else {
                success("")
            }
        });
        let client = client.reconfigured(Vec::new(), None);
        let mut context = flow_context(fs.clone(), store, probe);
        context.pins.insert("laravel.test:80/tcp".to_string(), 8090);
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        let written = fs.load(&context.override_path).await.unwrap();
        assert!(written.contains("\"8090:80\""));
        assert!(written.contains("\"3310:3306\""));
    }

    #[gpui::test]
    async fn test_start_with_an_old_compose_runs_without_the_override(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let (client, calls) = sail_client_with(|_| success(""));
        let mut context = flow_context(fs.clone(), store.clone(), probe);
        context.compose_supported = false;
        fs.write(&context.override_path, b"stale").await.unwrap();
        let client = client.reconfigured(
            vec!["compose.yaml".to_string()],
            Some(context.override_path.clone()),
        );

        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert!(!fs.is_file(&context.override_path).await);
        assert_eq!(subcommand_count(&calls, "config"), 0);
        let up_calls = up_calls(&calls);
        assert_eq!(up_calls.len(), 1);
        assert!(!up_calls[0].1.contains_key("SAIL_FILES"));
        assert!(store.snapshot().is_empty());
    }

    #[gpui::test]
    async fn test_start_without_auto_assign_keeps_the_override_only_with_pins(
        cx: &mut TestAppContext,
    ) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[]);
        let mut context = flow_context(fs.clone(), store, probe);
        context.auto_assign = false;
        fs.write(&context.override_path, b"existing").await.unwrap();
        let (client, calls) = sail_client_with(|_| success(""));
        let client = client.reconfigured(
            vec!["compose.yaml".to_string()],
            Some(context.override_path.clone()),
        );

        context.pins.insert("laravel.test:80/tcp".to_string(), 8090);
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert!(fs.is_file(&context.override_path).await);
        assert_eq!(
            up_calls(&calls)[0].1.get("SAIL_FILES"),
            Some(&override_files(&context.override_path))
        );

        context.pins.clear();
        let report = start_with_ports(&client, &StartTarget::Environment, &context).await;
        assert!(report.result.is_ok());
        assert!(!fs.is_file(&context.override_path).await);
        assert!(!up_calls(&calls)[1].1.contains_key("SAIL_FILES"));
    }

    #[gpui::test]
    async fn test_reset_removes_the_registry_entry_and_the_override(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let store = ports::InMemoryAssignmentStore::new();
        store.seed("/work/blog", Vec::new());
        store.seed("/work/shop", Vec::new());
        let context = flow_context(fs.clone(), store.clone(), ports::FakePortProbe::new(&[]));
        fs.write(&context.override_path, b"existing").await.unwrap();
        context.reset().await.unwrap();
        assert!(!fs.is_file(&context.override_path).await);
        let registry = store.snapshot();
        assert!(!registry.contains_key("/work/blog"));
        assert!(registry.contains_key("/work/shop"));
    }

    #[gpui::test]
    async fn test_system_published_ports_runs_docker_directly(_cx: &mut TestAppContext) {
        let (client, calls) = sail_client_with(|args| {
            if is_system_ps(args) {
                success(
                    "{\"Labels\":\"com.docker.compose.project=shop\",\"Ports\":\"0.0.0.0:8080->80/tcp\"}\n",
                )
            } else {
                failure(1, "", "unexpected")
            }
        });
        let published = client.system_published_ports().await.unwrap();
        assert_eq!(published, vec![(Some("shop".to_string()), 8080)]);
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].0, PathBuf::from("/usr/bin/docker"));
        assert_eq!(calls[0].1, ["ps", "--format", "{{json .}}"]);
    }
}
