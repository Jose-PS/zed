use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use collections::HashSet;
use fs::Fs;
use futures::StreamExt as _;
use gpui::{
    Action, App, AsyncWindowContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, Pixels, PromptLevel, Render, ScrollHandle, Task, WeakEntity, Window,
    actions,
};
use project::Project;
use settings::{Settings, SettingsLocation, SettingsStore};
use terminal_view::terminal_panel::TerminalPanel;
use ui::{
    ContextMenu, ContextMenuEntry, Indicator, ListItem, ListItemSpacing, PopoverMenu, Tooltip,
    WithScrollbar, prelude::*, right_click_menu,
};
use util::ResultExt as _;
use util::rel_path::RelPath;
use util::shell::ShellKind;
use workspace::{
    Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
};
use worktree::WorktreeId;

use crate::add_action_modal::AddActionModal;
use crate::compose::{
    self, ComposeInvocation, ComposeLauncher, CustomAction, CustomActionMode, EnvironmentState,
    Health, NodePackageManager, PortMapping, QuickAction, Service, ServiceKind, ServiceOperation,
    ServiceState, ShellPreference, TerminalTarget,
};
use crate::composer_command_modal::ComposerCommandModal;
use crate::configure_url_modal::ConfigureUrlModal;
use crate::docker::{
    self, CommandRunner, ComposeClient, DockerError, PortContext, PortSettings, ProjectDetection,
    ProjectSnapshot, RealCommandRunner, StartReport, StartTarget,
};
use crate::ports::{
    self, AssignmentStore, KvpAssignmentStore, PortAssignment, PortConflict, PortProbe,
    SystemPortProbe,
};
use crate::project_settings;
use crate::sail_manager_settings::{DockSide, SailManagerSettings};
use crate::settings_view::SettingsView;
use crate::terminal;

actions!(
    sail_manager,
    [
        ToggleFocus,
        Refresh,
        StartEnvironment,
        StopEnvironment,
        RestartEnvironment
    ]
);

struct SailManagerToast;

pub(crate) struct SettingsContext {
    pub services: Vec<String>,
    pub detected_package_manager: Option<NodePackageManager>,
    pub has_node: bool,
    pub port_rows: Vec<PortRow>,
    pub ports_supported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalRequest {
    Shell { root: bool },
    Client,
}

impl TerminalRequest {
    fn placeholder_target(&self) -> TerminalTarget {
        match self {
            Self::Shell { root } => TerminalTarget::Shell {
                shell: String::new(),
                root: *root,
            },
            Self::Client => TerminalTarget::Client,
        }
    }
}

struct TerminalLaunch {
    client: ComposeClient,
    service: Service,
    project_name: String,
    app_service: String,
    preference: ShellPreference,
    is_sail: bool,
    label: String,
}

fn shell_target(
    preference: ShellPreference,
    available: &[&str],
    root: bool,
) -> Option<TerminalTarget> {
    compose::choose_shell(preference, available).map(|shell| TerminalTarget::Shell {
        shell: shell.to_string(),
        root,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServiceMenuItem {
    OpenInBrowser { label: String, url: String },
    OpenTerminal,
    OpenRootTerminal,
    OpenClient { label: &'static str },
    ViewLogs,
    CopyPort(u16),
    Operation(ServiceOperation),
    ConfigureUrl,
    Separator,
}

impl ServiceMenuItem {
    fn label(&self) -> String {
        match self {
            Self::OpenInBrowser { label, .. } => label.clone(),
            Self::OpenTerminal => "Open Terminal".to_string(),
            Self::OpenRootTerminal => "Open Root Terminal".to_string(),
            Self::OpenClient { label } => label.to_string(),
            Self::ViewLogs => "View Logs".to_string(),
            Self::CopyPort(_) => "Copy Port".to_string(),
            Self::Operation(ServiceOperation::Start) => "Start Service".to_string(),
            Self::Operation(ServiceOperation::Stop) => "Stop Service".to_string(),
            Self::Operation(ServiceOperation::Restart) => "Restart Service".to_string(),
            Self::ConfigureUrl => "Configure URL…".to_string(),
            Self::Separator => String::new(),
        }
    }
}

fn service_menu_items(service: &Service, is_sail: bool, urls: &[String]) -> Vec<ServiceMenuItem> {
    use ServiceMenuItem::*;
    let operations = if service.state.is_running() {
        vec![
            Operation(ServiceOperation::Restart),
            Operation(ServiceOperation::Stop),
        ]
    } else {
        vec![Operation(ServiceOperation::Start)]
    };
    let browser = urls.iter().map(|url| OpenInBrowser {
        label: if urls.len() > 1 {
            url.clone()
        } else {
            "Open in Browser".to_string()
        },
        url: url.clone(),
    });
    let mut items = Vec::new();
    match service.kind {
        ServiceKind::App => {
            items.extend(browser);
            items.push(OpenTerminal);
            if is_sail {
                items.push(OpenRootTerminal);
            }
            items.push(ViewLogs);
            items.push(Separator);
            items.extend(operations);
            items.push(Separator);
            items.push(ConfigureUrl);
        }
        ServiceKind::Mysql | ServiceKind::Mariadb | ServiceKind::Postgres => {
            items.push(OpenClient {
                label: match service.kind {
                    ServiceKind::Mysql => "Open MySQL Client",
                    ServiceKind::Mariadb => "Open MariaDB Client",
                    _ => "Open PostgreSQL Client",
                },
            });
            items.push(OpenTerminal);
            items.push(ViewLogs);
            items.push(Separator);
            if let Some(port) = service.ports.iter().find_map(|port| port.host_port) {
                items.push(CopyPort(port));
            }
            items.extend(operations);
        }
        ServiceKind::Redis => {
            items.push(OpenClient {
                label: "Open redis-cli",
            });
            items.push(OpenTerminal);
            items.push(ViewLogs);
            items.push(Separator);
            items.extend(operations);
        }
        ServiceKind::Mailpit | ServiceKind::PhpMyAdmin | ServiceKind::Other => {
            items.extend(browser);
            items.push(OpenTerminal);
            items.push(ViewLogs);
            items.push(Separator);
            items.extend(operations);
            items.push(Separator);
            items.push(ConfigureUrl);
        }
    }
    items
}

const EVENT_REFRESH_DEBOUNCE: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub type DockerFinder = fn(Option<&str>, &Path) -> Option<PathBuf>;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<SailManagerPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &StartEnvironment, _, cx| {
            if let Some(panel) = workspace.panel::<SailManagerPanel>(cx) {
                panel.update(cx, |panel, cx| panel.start_environment(cx));
            }
        });
        workspace.register_action(|workspace, _: &StopEnvironment, _, cx| {
            if let Some(panel) = workspace.panel::<SailManagerPanel>(cx) {
                panel.update(cx, |panel, cx| panel.stop_environment(cx));
            }
        });
        workspace.register_action(|workspace, _: &RestartEnvironment, _, cx| {
            if let Some(panel) = workspace.panel::<SailManagerPanel>(cx) {
                panel.update(cx, |panel, cx| panel.restart_environment(cx));
            }
        });
    })
    .detach();
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelOperation {
    StartingEnvironment,
    StoppingEnvironment,
    RestartingEnvironment,
    Service {
        service: String,
        operation: ServiceOperation,
    },
}

impl PanelOperation {
    fn success_message(&self) -> String {
        match self {
            Self::StartingEnvironment => "Environment started".to_string(),
            Self::StoppingEnvironment => "Environment stopped (volumes kept)".to_string(),
            Self::RestartingEnvironment => "Environment restarted".to_string(),
            Self::Service { service, operation } => match operation {
                ServiceOperation::Start => format!("Service {service} started"),
                ServiceOperation::Stop => format!("Service {service} stopped"),
                ServiceOperation::Restart => format!("Service {service} restarted"),
            },
        }
    }

    fn failure_message(&self, detail: &str) -> String {
        let action = match self {
            Self::StartingEnvironment => "start the environment".to_string(),
            Self::StoppingEnvironment => "stop the environment".to_string(),
            Self::RestartingEnvironment => "restart the environment".to_string(),
            Self::Service { service, operation } => match operation {
                ServiceOperation::Start => format!("start service {service}"),
                ServiceOperation::Stop => format!("stop service {service}"),
                ServiceOperation::Restart => format!("restart service {service}"),
            },
        };
        format!("Could not {action}: {detail}")
    }

    fn status(&self) -> &'static str {
        match self {
            Self::StartingEnvironment => "Starting…",
            Self::StoppingEnvironment => "Stopping…",
            Self::RestartingEnvironment => "Restarting…",
            Self::Service { operation, .. } => match operation {
                ServiceOperation::Start => "Starting…",
                ServiceOperation::Stop => "Stopping…",
                ServiceOperation::Restart => "Restarting…",
            },
        }
    }
}

fn header_button_states(
    environment_state: EnvironmentState,
    operation: Option<&PanelOperation>,
) -> (bool, bool, bool) {
    if operation.is_some() {
        return (false, false, false);
    }
    match environment_state {
        EnvironmentState::Running => (false, true, true),
        EnvironmentState::Stopped => (true, false, false),
        EnvironmentState::Partial => (true, true, true),
        EnvironmentState::NoServices => (false, false, false),
    }
}

#[derive(Debug, Clone)]
enum PanelState {
    Loading,
    Remote,
    NoProject,
    NotCompose,
    DockerUnavailable(String),
    Ready {
        project_name: String,
        detection: ProjectDetection,
        snapshot: ProjectSnapshot,
    },
    Error(String),
}

enum RefreshOutcome {
    NotCompose,
    DockerUnavailable(String),
    Ready {
        detection: ProjectDetection,
        snapshot: ProjectSnapshot,
        client: ComposeClient,
        compose_version: Option<String>,
        assignments: Vec<PortAssignment>,
    },
    Error(String),
}

struct RefreshRequest {
    store: Arc<dyn AssignmentStore>,
    root: Arc<Path>,
    fs: Arc<dyn Fs>,
    runner: Arc<dyn CommandRunner>,
    find_docker: DockerFinder,
    environment: HashMap<String, String>,
    app_service: String,
    compose_files: Vec<String>,
    check_availability: bool,
    override_enabled: bool,
    known_compose_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortRow {
    pub key: String,
    pub service: String,
    pub container_port: u16,
    pub protocol: String,
    pub original_host_port: u16,
    pub host_port: u16,
    pub pinned: bool,
}

pub struct SailManagerPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    fs: Arc<dyn Fs>,
    runner: Arc<dyn CommandRunner>,
    find_docker: DockerFinder,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    root: Option<(WorktreeId, Arc<Path>)>,
    state: PanelState,
    client: Option<ComposeClient>,
    operation: Option<PanelOperation>,
    refreshing: bool,
    generation: u64,
    refresh_task: Task<()>,
    events_task: Option<Task<()>>,
    event_refresh_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    expanded_services: HashSet<String>,
    shell_cache: HashMap<String, Vec<&'static str>>,
    quick_actions_collapsed: bool,
    custom_actions_collapsed: bool,
    showing_settings: bool,
    settings_view: Option<Entity<SettingsView>>,
    active: bool,
    observed_settings: (String, String, Vec<String>),
    assignment_store: Arc<dyn AssignmentStore>,
    port_probe: Arc<dyn PortProbe>,
    compose_version: Option<String>,
    port_assignments: Vec<PortAssignment>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl SailManagerPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        let weak_workspace = workspace.clone();
        workspace.update_in(&mut cx, |workspace, _, cx| {
            let project = workspace.project().clone();
            cx.new(|cx| {
                Self::new(
                    weak_workspace.clone(),
                    project,
                    Arc::new(RealCommandRunner),
                    docker::find_docker_binary,
                    cx,
                )
            })
        })
    }

    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        runner: Arc<dyn CommandRunner>,
        find_docker: DockerFinder,
        cx: &mut Context<Self>,
    ) -> Self {
        let store: Arc<dyn AssignmentStore> =
            Arc::new(KvpAssignmentStore::new(db::kvp::KeyValueStore::global(cx)));
        Self::new_with_ports(
            workspace,
            project,
            runner,
            find_docker,
            store,
            Arc::new(SystemPortProbe),
            cx,
        )
    }

    pub fn new_with_ports(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        runner: Arc<dyn CommandRunner>,
        find_docker: DockerFinder,
        assignment_store: Arc<dyn AssignmentStore>,
        port_probe: Arc<dyn PortProbe>,
        cx: &mut Context<Self>,
    ) -> Self {
        let project_subscription = cx.subscribe(&project, |this, _, event, cx| match event {
            project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) => {
                this.update_root(cx);
            }
            _ => {}
        });
        let fs = project.read(cx).fs().clone();
        let root = Self::first_visible_root(&project, cx);
        let mut panel = Self {
            workspace,
            project,
            fs,
            runner,
            find_docker,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            root,
            state: PanelState::Loading,
            client: None,
            operation: None,
            refreshing: false,
            generation: 0,
            refresh_task: Task::ready(()),
            events_task: None,
            event_refresh_task: None,
            poll_task: None,
            expanded_services: HashSet::default(),
            shell_cache: HashMap::new(),
            quick_actions_collapsed: false,
            custom_actions_collapsed: false,
            showing_settings: false,
            settings_view: None,
            active: false,
            observed_settings: (String::new(), String::new(), Vec::new()),
            assignment_store,
            port_probe,
            compose_version: None,
            port_assignments: Vec::new(),
            _subscriptions: vec![project_subscription],
        };
        let settings_subscription = cx.observe_global::<SettingsStore>(|this, cx| {
            this.sync_background_tasks(cx);
            if this.observed_settings != this.settings_signature(cx) {
                this.refresh(cx);
            } else {
                cx.notify();
            }
        });
        panel._subscriptions.push(settings_subscription);
        panel.refresh(cx);
        panel
    }

    fn is_remote(&self, cx: &App) -> bool {
        let project = self.project.read(cx);
        project.is_via_remote_server() || project.is_via_collab()
    }

    fn first_visible_root(project: &Entity<Project>, cx: &App) -> Option<(WorktreeId, Arc<Path>)> {
        project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| {
                let worktree = worktree.read(cx);
                (worktree.id(), worktree.abs_path())
            })
    }

    fn update_root(&mut self, cx: &mut Context<Self>) {
        let root = Self::first_visible_root(&self.project, cx);
        if root == self.root {
            return;
        }
        self.root = root;
        self.generation += 1;
        self.refresh_task = Task::ready(());
        self.events_task = None;
        self.event_refresh_task = None;
        self.showing_settings = false;
        self.settings_view = None;
        self.refreshing = false;
        self.expanded_services.clear();
        self.shell_cache.clear();
        self.client = None;
        self.operation = None;
        self.compose_version = None;
        self.port_assignments.clear();
        self.state = PanelState::Loading;
        self.refresh(cx);
    }

    fn settings<'a>(&self, cx: &'a App) -> &'a SailManagerSettings {
        let location = self.root.as_ref().map(|(worktree_id, _)| SettingsLocation {
            worktree_id: *worktree_id,
            path: RelPath::empty(),
        });
        SailManagerSettings::get(location, cx)
    }

    fn settings_signature(&self, cx: &App) -> (String, String, Vec<String>) {
        let settings = self.settings(cx);
        (
            settings.app_service.clone(),
            settings.url_host.clone(),
            settings.compose_files.clone(),
        )
    }

    fn set_state(&mut self, state: PanelState, cx: &mut Context<Self>) {
        if !matches!(state, PanelState::Ready { .. }) {
            self.client = None;
            self.events_task = None;
            self.event_refresh_task = None;
        }
        self.state = state;
        cx.notify();
    }

    fn refresh_action(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.is_remote(cx) {
            self.refreshing = false;
            self.set_state(PanelState::Remote, cx);
            return;
        }
        let Some((_, root)) = self.root.clone() else {
            self.refreshing = false;
            self.set_state(PanelState::NoProject, cx);
            return;
        };

        let generation = self.generation;
        self.observed_settings = self.settings_signature(cx);
        let settings = self.settings(cx);
        let app_service = settings.app_service.clone();
        let compose_files = settings.compose_files.clone();
        let check_availability = !matches!(self.state, PanelState::Ready { .. });
        let override_enabled = settings.auto_assign_ports || !settings.port_pins.is_empty();
        let known_compose_version = self.compose_version.clone();
        let fs = self.fs.clone();
        let store = self.assignment_store.clone();
        let runner = self.runner.clone();
        let find_docker = self.find_docker;
        let project_environment = self.project.read(cx).environment().clone();
        let environment_task = project_environment.update(cx, |environment, cx| {
            environment.directory_environment(root.clone(), cx)
        });

        self.refreshing = true;
        cx.notify();
        self.refresh_task = cx.spawn(async move |this, cx| {
            let environment: HashMap<String, String> = environment_task
                .await
                .map(|environment| environment.into_iter().collect())
                .unwrap_or_default();
            let request = RefreshRequest {
                store,
                root,
                fs,
                runner,
                find_docker,
                environment,
                app_service,
                compose_files,
                check_availability,
                override_enabled,
                known_compose_version,
            };
            let outcome = cx
                .background_spawn(async move { load_compose_state(request).await })
                .await;
            this.update(cx, |this, cx| this.apply_refresh(generation, outcome, cx))
                .log_err();
        });
    }

    fn apply_refresh(&mut self, generation: u64, outcome: RefreshOutcome, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        self.refreshing = false;
        match outcome {
            RefreshOutcome::NotCompose => self.set_state(PanelState::NotCompose, cx),
            RefreshOutcome::DockerUnavailable(message) => {
                self.set_state(PanelState::DockerUnavailable(message), cx)
            }
            RefreshOutcome::Error(message) => self.set_state(PanelState::Error(message), cx),
            RefreshOutcome::Ready {
                detection,
                snapshot,
                client,
                compose_version,
                assignments,
            } => {
                if compose_version.is_some() {
                    self.compose_version = compose_version;
                }
                self.port_assignments = assignments;
                let project_name = if snapshot.project_name.is_empty() {
                    self.root_name().unwrap_or_default()
                } else {
                    snapshot.project_name.clone()
                };
                self.set_state(
                    PanelState::Ready {
                        project_name,
                        detection,
                        snapshot,
                    },
                    cx,
                );
                self.client = Some(client);
                self.sync_background_tasks(cx);
            }
        }
    }

    fn sync_background_tasks(&mut self, cx: &mut Context<Self>) {
        let refresh_mode = self.settings(cx).refresh_mode;
        if self.active && refresh_mode.polls() {
            if self.poll_task.is_none() {
                self.start_polling(cx);
            }
        } else {
            self.poll_task = None;
        }
        if refresh_mode.listens_to_events() {
            if self.events_task.is_none()
                && let Some(client) = self.client.clone()
            {
                self.start_events(client, cx);
            }
        } else {
            self.events_task = None;
            self.event_refresh_task = None;
        }
    }

    fn start_events(&mut self, client: ComposeClient, cx: &mut Context<Self>) {
        let generation = self.generation;
        let Some(mut stream) = client.events().log_err() else {
            return;
        };
        self.events_task = Some(cx.spawn(async move |this, cx| {
            while stream.next().await.is_some() {
                if this
                    .update(cx, |this, cx| this.schedule_event_refresh(generation, cx))
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, _| {
                if this.generation == generation {
                    this.events_task = None;
                }
            })
            .log_err();
        }));
    }

    fn schedule_event_refresh(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        self.event_refresh_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(EVENT_REFRESH_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.refresh(cx);
                }
            })
            .log_err();
        }));
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn start_environment_action(
        &mut self,
        _: &StartEnvironment,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_environment(cx);
    }

    fn stop_environment_action(
        &mut self,
        _: &StopEnvironment,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_environment(cx);
    }

    fn restart_environment_action(
        &mut self,
        _: &RestartEnvironment,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restart_environment(cx);
    }

    pub fn start_environment(&mut self, cx: &mut Context<Self>) {
        self.run_operation(PanelOperation::StartingEnvironment, cx);
    }

    pub fn stop_environment(&mut self, cx: &mut Context<Self>) {
        self.run_operation(PanelOperation::StoppingEnvironment, cx);
    }

    pub fn restart_environment(&mut self, cx: &mut Context<Self>) {
        self.run_operation(PanelOperation::RestartingEnvironment, cx);
    }

    pub fn run_service_operation(
        &mut self,
        service: String,
        operation: ServiceOperation,
        cx: &mut Context<Self>,
    ) {
        self.run_operation(PanelOperation::Service { service, operation }, cx);
    }

    fn run_operation(&mut self, operation: PanelOperation, cx: &mut Context<Self>) {
        if self.operation.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.generation;
        let ports = self.start_target(&operation).and_then(|target| {
            let (_, root) = self.root.clone()?;
            let settings = self.port_settings(cx)?;
            Some((target, root, settings))
        });
        let fs = self.fs.clone();
        let store = self.assignment_store.clone();
        let probe = self.port_probe.clone();
        self.operation = Some(operation.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let task_operation = operation.clone();
            let report = cx
                .background_spawn(async move {
                    if let Some((target, root, settings)) = ports {
                        let context = PortContext::new(fs, store, probe, &root, settings).await;
                        return docker::start_with_ports(&client, &target, &context).await;
                    }
                    let result = match task_operation {
                        PanelOperation::StartingEnvironment => client.up().await,
                        PanelOperation::StoppingEnvironment => client.stop().await,
                        PanelOperation::RestartingEnvironment => client.restart().await,
                        PanelOperation::Service { service, operation } => {
                            client.service_operation(operation, &service).await
                        }
                    };
                    StartReport {
                        moved: None,
                        result,
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.finish_operation(generation, operation, report, cx)
            })
            .log_err();
        })
        .detach();
    }

    fn start_target(&self, operation: &PanelOperation) -> Option<StartTarget> {
        match operation {
            PanelOperation::StartingEnvironment => Some(StartTarget::Environment),
            PanelOperation::RestartingEnvironment
                if matches!(
                    &self.state,
                    PanelState::Ready { snapshot, .. }
                        if snapshot.environment_state == EnvironmentState::Stopped
                ) =>
            {
                Some(StartTarget::Environment)
            }
            PanelOperation::Service {
                service,
                operation: ServiceOperation::Start,
            } => Some(StartTarget::Service(service.clone())),
            _ => None,
        }
    }

    fn port_settings(&self, cx: &App) -> Option<PortSettings> {
        let PanelState::Ready { detection, .. } = &self.state else {
            return None;
        };
        let settings = self.settings(cx);
        let configured_files = settings.compose_files.clone();
        let base_files = if configured_files.is_empty() {
            detection
                .compose_files
                .as_ref()
                .map(|files| files.all())
                .unwrap_or_default()
        } else {
            configured_files.clone()
        };
        Some(PortSettings {
            configured_files,
            base_files,
            pins: settings.port_pins.clone(),
            auto_assign: settings.auto_assign_ports,
            compose_supported: self
                .compose_version
                .as_deref()
                .is_some_and(ports::compose_supports_port_override),
        })
    }

    fn port_rows(&self, cx: &App) -> Vec<PortRow> {
        let PanelState::Ready { snapshot, .. } = &self.state else {
            return Vec::new();
        };
        let pins = &self.settings(cx).port_pins;
        let mut rows: Vec<PortRow> = Vec::new();
        for service in &snapshot.services {
            for port in &service.ports {
                let Some(host_port) = port.host_port else {
                    continue;
                };
                let key = ports::PortKey {
                    service: service.name.clone(),
                    container_port: port.container_port,
                    protocol: port.protocol.clone(),
                };
                if rows.iter().any(|row| row.key == key.pin_key()) {
                    continue;
                }
                let original_host_port = self
                    .port_assignments
                    .iter()
                    .find(|assignment| assignment.key == key)
                    .map_or(host_port, |assignment| assignment.original_host_port);
                rows.push(PortRow {
                    pinned: pins.contains_key(&key.pin_key()),
                    key: key.pin_key(),
                    service: key.service,
                    container_port: key.container_port,
                    protocol: key.protocol,
                    original_host_port,
                    host_port,
                });
            }
        }
        rows
    }

    pub(crate) fn reset_port_assignments(&mut self, cx: &mut Context<Self>) {
        let Some((_, root)) = self.root.clone() else {
            return;
        };
        let fs = self.fs.clone();
        let store = self.assignment_store.clone();
        let probe = self.port_probe.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let settings = PortSettings {
                        configured_files: Vec::new(),
                        base_files: Vec::new(),
                        pins: Default::default(),
                        auto_assign: false,
                        compose_supported: false,
                    };
                    PortContext::new(fs, store, probe, &root, settings)
                        .await
                        .reset()
                        .await
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.show_save_error("the port assignments", &error, cx);
                }
                this.refresh(cx);
            })
            .log_err();
        })
        .detach();
    }

    fn finish_operation(
        &mut self,
        generation: u64,
        operation: PanelOperation,
        report: StartReport,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.operation = None;
        self.shell_cache.clear();
        cx.notify();
        self.refresh(cx);
        if let Some(moved) = report.moved {
            self.show_toast(
                Toast::new(
                    NotificationId::unique::<SailManagerToast>(),
                    format!("Ports remapped: {moved}"),
                )
                .autohide(),
                cx,
            );
        }
        let toast = match report.result {
            Ok(()) => Toast::new(
                NotificationId::unique::<SailManagerToast>(),
                operation.success_message(),
            )
            .autohide(),
            Err(error) => match error.downcast_ref::<PortConflict>() {
                Some(conflict) => Toast::new(
                    NotificationId::unique::<SailManagerToast>(),
                    conflict.to_string(),
                ),
                None => Toast::new(
                    NotificationId::unique::<SailManagerToast>(),
                    operation.failure_message(&anyhow_error_message(&error)),
                ),
            },
        };
        self.show_toast(toast, cx);
    }

    fn show_toast(&self, toast: Toast, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.show_toast(toast, cx))
            .log_err();
    }

    pub fn view_logs(&mut self, service: &str, window: &mut Window, cx: &mut Context<Self>) {
        let PanelState::Ready { project_name, .. } = &self.state else {
            return;
        };
        let Some(client) = self.client.as_ref() else {
            return;
        };
        let spawn = match terminal::logs_spawn(
            client.invocation(),
            project_name,
            service,
            client.environment(),
            ShellKind::system(),
        ) {
            Ok(spawn) => spawn,
            Err(error) => {
                self.show_toast(
                    Toast::new(
                        NotificationId::unique::<SailManagerToast>(),
                        format!("Could not open the logs of {service}: {error:#}"),
                    ),
                    cx,
                );
                return;
            }
        };
        self.run_spawn(spawn, "logs", window, cx);
    }

    fn run_spawn(
        &mut self,
        spawn: task::SpawnInTerminal,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal_task = self
            .workspace
            .update(cx, |workspace, cx| {
                workspace.spawn_in_terminal(spawn, window, cx)
            })
            .log_err();
        if let Some(terminal_task) = terminal_task {
            cx.spawn(async move |this, cx| {
                if let Some(Err(error)) = terminal_task.await {
                    this.update(cx, |this, cx| {
                        this.show_toast(
                            Toast::new(
                                NotificationId::unique::<SailManagerToast>(),
                                format!("Could not open the {what}: {error:#}"),
                            ),
                            cx,
                        )
                    })
                    .log_err();
                }
            })
            .detach();
        }
    }

    fn show_message(&self, message: String, cx: &mut Context<Self>) {
        self.show_toast(
            Toast::new(NotificationId::unique::<SailManagerToast>(), message).autohide(),
            cx,
        );
    }

    fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        cx.open_url(&url);
        self.show_message(format!("Opening {url}"), cx);
    }

    fn copy_port(&mut self, port: u16, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(port.to_string()));
        self.show_message(format!("Copied port {port}"), cx);
    }

    pub(crate) fn configure_url(
        &mut self,
        service_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((worktree_id, _)) = self.root.clone() else {
            return;
        };
        let settings = self.settings(cx);
        let url_host = settings.url_host.clone();
        let current_url = settings.manual_urls.get(service_name).cloned();
        let project = self.project.clone();
        let service = service_name.to_string();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    ConfigureUrlModal::new(
                        project,
                        worktree_id,
                        service,
                        &url_host,
                        current_url.as_deref(),
                        window,
                        cx,
                    )
                })
            })
            .log_err();
    }

    fn focus_existing_terminal(
        &self,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(terminal_panel) = self
            .workspace
            .read_with(cx, |workspace, cx| workspace.panel::<TerminalPanel>(cx))
            .log_err()
            .flatten()
        else {
            return false;
        };
        terminal_panel.update(cx, |terminal_panel, cx| {
            terminal_panel.focus_running_task_terminal(label, window, cx)
        })
    }

    fn open_terminal(
        &mut self,
        service_name: &str,
        request: TerminalRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PanelState::Ready {
            project_name,
            snapshot,
            ..
        } = &self.state
        else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(service) = snapshot
            .services
            .iter()
            .find(|service| service.name == service_name)
            .cloned()
        else {
            return;
        };
        let settings = self.settings(cx);
        let launch = TerminalLaunch {
            label: terminal::service_terminal_label(
                project_name,
                &service.name,
                &request.placeholder_target(),
            ),
            project_name: project_name.clone(),
            app_service: settings.app_service.clone(),
            preference: settings.shell,
            is_sail: matches!(client.invocation().launcher, ComposeLauncher::Sail { .. }),
            client,
            service,
        };
        if self.focus_existing_terminal(&launch.label, window, cx) {
            return;
        }
        match request {
            TerminalRequest::Client => {
                self.launch_terminal(launch, TerminalTarget::Client, window, cx)
            }
            TerminalRequest::Shell { root } => {
                if let Some(shells) = self.shell_cache.get(&launch.service.name).cloned() {
                    self.launch_shell(launch, root, &shells, window, cx);
                    return;
                }
                let generation = self.generation;
                let client = launch.client.clone();
                let service = launch.service.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let shells = cx
                        .background_spawn(async move { client.available_shells(&service).await })
                        .await;
                    this.update_in(cx, |this, window, cx| {
                        if this.generation != generation {
                            return;
                        }
                        match shells {
                            Ok(shells) => {
                                this.shell_cache
                                    .insert(launch.service.name.clone(), shells.clone());
                                this.launch_shell(launch, root, &shells, window, cx);
                            }
                            Err(error) => this.show_toast(
                                Toast::new(
                                    NotificationId::unique::<SailManagerToast>(),
                                    format!(
                                        "Could not detect the shell of {}: {error:#}",
                                        launch.service.name
                                    ),
                                ),
                                cx,
                            ),
                        }
                    })
                    .log_err();
                })
                .detach();
            }
        }
    }

    fn launch_shell(
        &mut self,
        launch: TerminalLaunch,
        root: bool,
        shells: &[&'static str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match shell_target(launch.preference, shells, root) {
            Some(target) => self.launch_terminal(launch, target, window, cx),
            None => self.show_toast(
                Toast::new(
                    NotificationId::unique::<SailManagerToast>(),
                    format!("No shell found in {}", launch.service.name),
                ),
                cx,
            ),
        }
    }

    fn launch_terminal(
        &mut self,
        launch: TerminalLaunch,
        target: TerminalTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.focus_existing_terminal(&launch.label, window, cx) {
            return;
        }
        match terminal::service_terminal_spawn(
            launch.client.invocation(),
            &launch.project_name,
            &launch.service,
            launch.is_sail,
            &launch.app_service,
            &target,
            launch.client.environment(),
            ShellKind::system(),
        ) {
            Ok(spawn) => self.run_spawn(spawn, "terminal", window, cx),
            Err(error) => self.show_toast(
                Toast::new(
                    NotificationId::unique::<SailManagerToast>(),
                    format!(
                        "Could not open a terminal in {}: {error:#}",
                        launch.service.name
                    ),
                ),
                cx,
            ),
        }
    }

    fn toggle_service(&mut self, service_name: &str, cx: &mut Context<Self>) {
        if !self.expanded_services.remove(service_name) {
            self.expanded_services.insert(service_name.to_string());
        }
        cx.notify();
    }

    fn root_name(&self) -> Option<String> {
        self.root.as_ref().and_then(|(_, root)| {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
    }

    fn environment_status(&self) -> Option<(Color, &'static str)> {
        match &self.state {
            PanelState::Loading => Some((Color::Accent, "Checking…")),
            PanelState::Ready { snapshot, .. } => Some(match snapshot.environment_state {
                EnvironmentState::Running => (Color::Success, "Running"),
                EnvironmentState::Partial => (Color::Warning, "Partially running"),
                EnvironmentState::Stopped => (Color::Muted, "Stopped"),
                EnvironmentState::NoServices => (Color::Muted, "No services"),
            }),
            PanelState::DockerUnavailable(_) => Some((Color::Error, "Unavailable")),
            PanelState::Error(_) => Some((Color::Error, "Error")),
            PanelState::Remote | PanelState::NoProject | PanelState::NotCompose => None,
        }
    }

    fn compose_files_description(&self, detection: &ProjectDetection, cx: &App) -> (String, Color) {
        let configured_files = &self.settings(cx).compose_files;
        let files: Vec<String> = if !configured_files.is_empty() {
            configured_files.clone()
        } else if let Some(compose_files) = &detection.compose_files {
            std::iter::once(compose_files.primary.clone())
                .chain(compose_files.overrides.iter().cloned())
                .collect()
        } else {
            Vec::new()
        };
        let files = files.join(" + ");
        if detection.is_sail && !detection.has_vendor_sail_binary {
            (
                "vendor/bin/sail not found · run composer install".to_string(),
                Color::Warning,
            )
        } else if detection.is_sail {
            (format!("Sail · {files}"), Color::Muted)
        } else {
            (files, Color::Muted)
        }
    }

    fn service_is_running(&self, name: &str) -> bool {
        match &self.state {
            PanelState::Ready { snapshot, .. } => snapshot
                .services
                .iter()
                .any(|service| service.name == name && service.state.is_running()),
            _ => false,
        }
    }

    fn detected_package_manager(&self) -> Option<NodePackageManager> {
        match &self.state {
            PanelState::Ready { detection, .. } => detection.node_package_manager,
            _ => None,
        }
    }

    fn effective_package_manager(&self, cx: &App) -> Option<NodePackageManager> {
        self.settings(cx)
            .node_package_manager
            .or_else(|| self.detected_package_manager())
    }

    fn confirm_then(
        &mut self,
        confirmation: Option<Confirmation>,
        window: &mut Window,
        cx: &mut Context<Self>,
        run: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(confirmation) = confirmation else {
            run(self, window, cx);
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &confirmation.message,
            Some(&confirmation.detail),
            &[confirmation.button, "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update_in(cx, |this, window, cx| run(this, window, cx))
                    .log_err();
            }
        })
        .detach();
    }

    fn launch_context(&self, cx: &App) -> Option<(String, ComposeClient, bool, String)> {
        let PanelState::Ready { project_name, .. } = &self.state else {
            return None;
        };
        let client = self.client.clone()?;
        let is_sail = matches!(client.invocation().launcher, ComposeLauncher::Sail { .. });
        Some((
            project_name.clone(),
            client,
            is_sail,
            self.settings(cx).app_service.clone(),
        ))
    }

    pub(crate) fn show_save_error(
        &mut self,
        what: &str,
        error: &anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        self.show_toast(
            Toast::new(
                NotificationId::unique::<SailManagerToast>(),
                format!("Could not save {what}: {error:#}"),
            ),
            cx,
        );
    }

    pub(crate) fn settings_context(&self, cx: &App) -> SettingsContext {
        let services = match &self.state {
            PanelState::Ready { snapshot, .. } => snapshot
                .services
                .iter()
                .map(|service| service.name.clone())
                .collect(),
            _ => Vec::new(),
        };
        SettingsContext {
            services,
            detected_package_manager: self.detected_package_manager(),
            has_node: self.effective_package_manager(cx).is_some(),
            port_rows: self.port_rows(cx),
            ports_supported: self
                .compose_version
                .as_deref()
                .is_none_or(ports::compose_supports_port_override),
        }
    }

    fn can_configure(&self, cx: &App) -> bool {
        self.root.is_some() && !self.is_remote(cx)
    }

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.showing_settings {
            self.close_settings(cx);
            return;
        }
        let Some((worktree_id, _)) = self.root.clone() else {
            return;
        };
        if !self.can_configure(cx) {
            return;
        }
        let panel = cx.weak_entity();
        let project = self.project.clone();
        self.settings_view =
            Some(cx.new(|cx| SettingsView::new(panel, project, worktree_id, window, cx)));
        self.showing_settings = true;
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.showing_settings = false;
        self.settings_view = None;
        cx.notify();
    }

    fn show_launch_error(&mut self, what: &str, error: &anyhow::Error, cx: &mut Context<Self>) {
        self.show_toast(
            Toast::new(
                NotificationId::unique::<SailManagerToast>(),
                format!("Could not run {what}: {error:#}"),
            ),
            cx,
        );
    }

    pub fn run_quick_action(
        &mut self,
        action: QuickAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let package_manager = self
            .effective_package_manager(cx)
            .unwrap_or(NodePackageManager::Npm);
        let Some((project_name, ..)) = self.launch_context(cx) else {
            return;
        };
        let label = terminal::quick_action_label(&project_name, action, package_manager);
        if self.focus_existing_terminal(&label, window, cx) {
            return;
        }
        self.confirm_then(
            quick_action_confirmation(action),
            window,
            cx,
            move |this, window, cx| {
                let Some((project_name, client, is_sail, app_service)) = this.launch_context(cx)
                else {
                    return;
                };
                match terminal::quick_action_spawn(
                    client.invocation(),
                    &project_name,
                    action,
                    is_sail,
                    &app_service,
                    package_manager,
                    client.environment(),
                    ShellKind::system(),
                ) {
                    Ok(spawn) => this.run_spawn(spawn, "action", window, cx),
                    Err(error) => {
                        this.show_launch_error(&action.label(package_manager), &error, cx)
                    }
                }
            },
        );
    }

    pub fn run_composer_command(
        &mut self,
        arguments: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((project_name, client, is_sail, app_service)) = self.launch_context(cx) else {
            return;
        };
        let label = terminal::composer_custom_label(&project_name, &arguments);
        if self.focus_existing_terminal(&label, window, cx) {
            return;
        }
        match terminal::composer_custom_spawn(
            client.invocation(),
            &project_name,
            &arguments,
            is_sail,
            &app_service,
            client.environment(),
            ShellKind::system(),
        ) {
            Ok(spawn) => self.run_spawn(spawn, "composer command", window, cx),
            Err(error) => self.show_launch_error("the composer command", &error, cx),
        }
    }

    pub fn run_custom_action(
        &mut self,
        action: CustomAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((project_name, ..)) = self.launch_context(cx) else {
            return;
        };
        let label = terminal::custom_action_label(&project_name, &action);
        if self.focus_existing_terminal(&label, window, cx) {
            return;
        }
        let confirmation = custom_action_confirmation(&action);
        self.confirm_then(confirmation, window, cx, move |this, window, cx| {
            let Some((project_name, client, is_sail, app_service)) = this.launch_context(cx) else {
                return;
            };
            match terminal::custom_action_spawn(
                client.invocation(),
                &project_name,
                &action,
                is_sail,
                &app_service,
                client.environment(),
                ShellKind::system(),
            ) {
                Ok(spawn) => this.run_spawn(spawn, "action", window, cx),
                Err(error) => this.show_launch_error(&action.name, &error, cx),
            }
        });
    }

    fn remove_custom_action(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree_id, _)) = self.root.clone() else {
            return;
        };
        let confirmation = Confirmation {
            message: format!("Remove action {name}?"),
            detail: "This removes it from the project settings.".to_string(),
            button: "Remove",
        };
        let project = self.project.clone();
        self.confirm_then(Some(confirmation), window, cx, move |_, _, cx| {
            let removal = project_settings::remove_custom_action(project, worktree_id, name, cx);
            cx.spawn(async move |this, cx| {
                if let Err(error) = removal.await {
                    this.update(cx, |this, cx| {
                        this.show_launch_error("removing the action", &error, cx)
                    })
                    .log_err();
                }
            })
            .detach();
        });
    }

    fn set_node_package_manager(
        &mut self,
        package_manager: Option<NodePackageManager>,
        cx: &mut Context<Self>,
    ) {
        let Some((worktree_id, _)) = self.root.clone() else {
            return;
        };
        let save = project_settings::set_node_package_manager(
            self.project.clone(),
            worktree_id,
            package_manager,
            cx,
        );
        cx.spawn(async move |this, cx| {
            if let Err(error) = save.await {
                this.update(cx, |this, cx| {
                    this.show_launch_error("saving the package manager", &error, cx)
                })
                .log_err();
            }
        })
        .detach();
    }

    fn open_composer_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    ComposerCommandModal::new(panel, window, cx)
                })
            })
            .log_err();
    }

    fn open_add_action_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree_id, _)) = self.root.clone() else {
            return;
        };
        let PanelState::Ready { snapshot, .. } = &self.state else {
            return;
        };
        let services = snapshot
            .services
            .iter()
            .map(|service| service.name.clone())
            .collect::<Vec<_>>();
        let settings = self.settings(cx);
        let app_service = settings.app_service.clone();
        let existing_names = settings
            .actions
            .iter()
            .map(|action| action.name.clone())
            .collect::<Vec<_>>();
        let project = self.project.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    AddActionModal::new(
                        project,
                        worktree_id,
                        services,
                        app_service,
                        existing_names,
                        window,
                        cx,
                    )
                })
            })
            .log_err();
    }

    fn render_section_header(
        &self,
        id: &'static str,
        title: &'static str,
        collapsed: bool,
        on_toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .px_1()
            .py_1()
            .gap_1()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                on_toggle(this, cx);
                cx.notify();
            }))
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .child(Label::new(title).size(LabelSize::Small).color(Color::Muted))
    }

    fn render_group_header(&self, title: String, trailing: Option<AnyElement>) -> impl IntoElement {
        h_flex()
            .px_2()
            .pt_1()
            .justify_between()
            .child(
                Label::new(title)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .when_some(trailing, |this, trailing| this.child(trailing))
    }

    fn render_action_row(
        &self,
        id: impl Into<ElementId>,
        label: String,
        command: String,
        warning: bool,
        background: bool,
        disabled: bool,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> ListItem {
        ListItem::new(id)
            .spacing(ListItemSpacing::Dense)
            .disabled(disabled)
            .start_slot(
                Icon::new(IconName::PlayOutlined)
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(Label::new(label).truncate())
                    .when(warning, |this| {
                        this.child(
                            Icon::new(IconName::Warning)
                                .size(IconSize::XSmall)
                                .color(Color::Warning),
                        )
                    })
                    .when(background, |this| {
                        this.child(
                            Label::new("background")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                    }),
            )
            .end_slot(
                Label::new(command)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .buffer_font(cx)
                    .truncate(),
            )
            .when(disabled, |this| {
                this.tooltip(Tooltip::text("Start the environment first"))
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    }

    fn render_quick_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app_running = self.service_is_running(&self.settings(cx).app_service);
        let detected = self.detected_package_manager();
        let package_manager = self.effective_package_manager(cx);
        let collapsed = self.quick_actions_collapsed;
        let mut section = v_flex().child(self.render_section_header(
            "sail-manager-quick-actions-header",
            "Quick Actions",
            collapsed,
            |this, _| this.quick_actions_collapsed = !this.quick_actions_collapsed,
            cx,
        ));
        if collapsed {
            return section;
        }
        let artisan = [
            QuickAction::Tinker,
            QuickAction::Migrate,
            QuickAction::MigrateSeed,
            QuickAction::MigrateStatus,
            QuickAction::RouteList,
            QuickAction::OptimizeClear,
            QuickAction::Test,
        ];
        let composer = [QuickAction::ComposerInstall, QuickAction::ComposerUpdate];
        let node = [
            QuickAction::NodeInstall,
            QuickAction::NodeDev,
            QuickAction::NodeBuild,
        ];
        let display_manager = package_manager.unwrap_or(NodePackageManager::Npm);

        section = section.child(self.render_group_header("Artisan".to_string(), None));
        for (index, action) in artisan.into_iter().enumerate() {
            section = section.child(self.render_quick_action(
                ("sail-manager-artisan", index),
                action,
                display_manager,
                app_running,
                cx,
            ));
        }
        section = section.child(self.render_group_header("Composer".to_string(), None));
        for (index, action) in composer.into_iter().enumerate() {
            section = section.child(self.render_quick_action(
                ("sail-manager-composer", index),
                action,
                display_manager,
                app_running,
                cx,
            ));
        }
        section = section.child(
            ListItem::new("sail-manager-composer-custom")
                .spacing(ListItemSpacing::Dense)
                .disabled(!app_running)
                .start_slot(
                    Icon::new(IconName::Plus)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(Label::new("Custom Composer Command…"))
                .when(!app_running, |this| {
                    this.tooltip(Tooltip::text("Start the environment first"))
                })
                .on_click(cx.listener(|this, _, window, cx| this.open_composer_modal(window, cx))),
        );
        if package_manager.is_some() {
            let panel = cx.weak_entity();
            let selector = PopoverMenu::new("sail-manager-node-manager-menu")
                .trigger(
                    Button::new("sail-manager-node-manager", display_manager.program())
                        .label_size(LabelSize::XSmall)
                        .size(ButtonSize::Compact)
                        .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall))
                        .tooltip(Tooltip::text("Package Manager")),
                )
                .menu(move |window, cx| {
                    let panel = panel.clone();
                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                        let choices: [(String, Option<NodePackageManager>); 5] = [
                            (
                                format!(
                                    "Auto ({})",
                                    detected.map_or("none", |manager| manager.program())
                                ),
                                None,
                            ),
                            ("npm".to_string(), Some(NodePackageManager::Npm)),
                            ("pnpm".to_string(), Some(NodePackageManager::Pnpm)),
                            ("yarn".to_string(), Some(NodePackageManager::Yarn)),
                            ("bun".to_string(), Some(NodePackageManager::Bun)),
                        ];
                        for (label, choice) in choices {
                            let panel = panel.clone();
                            menu = menu.entry(label, None, move |_, cx| {
                                panel
                                    .update(cx, |panel, cx| {
                                        panel.set_node_package_manager(choice, cx)
                                    })
                                    .log_err();
                            });
                        }
                        menu
                    }))
                })
                .anchor(gpui::Anchor::TopRight);
            section = section.child(self.render_group_header(
                format!("Node · {}", display_manager.program()),
                Some(selector.into_any_element()),
            ));
            for (index, action) in node.into_iter().enumerate() {
                section = section.child(self.render_quick_action(
                    ("sail-manager-node", index),
                    action,
                    display_manager,
                    app_running,
                    cx,
                ));
            }
        }
        section
    }

    fn render_quick_action(
        &self,
        id: (&'static str, usize),
        action: QuickAction,
        package_manager: NodePackageManager,
        app_running: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.render_action_row(
            id,
            action.label(package_manager),
            action.display_command(package_manager),
            action == QuickAction::ComposerUpdate,
            false,
            !app_running,
            move |this, window, cx| this.run_quick_action(action, window, cx),
            cx,
        )
    }

    fn render_custom_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.custom_actions_collapsed;
        let settings = self.settings(cx);
        let app_service = settings.app_service.clone();
        let actions = settings.actions.clone();
        let mut section = v_flex().child(self.render_section_header(
            "sail-manager-custom-actions-header",
            "Custom Actions",
            collapsed,
            |this, _| this.custom_actions_collapsed = !this.custom_actions_collapsed,
            cx,
        ));
        if collapsed {
            return section;
        }
        for (index, action) in actions.into_iter().enumerate() {
            let service = action
                .service
                .clone()
                .unwrap_or_else(|| app_service.clone());
            let disabled = !self.service_is_running(&service);
            let background = action.mode == CustomActionMode::Background;
            let command = action.command.join(" ");
            let name = action.name.clone();
            let run_action = action.clone();
            let row = self.render_action_row(
                ("sail-manager-custom-action", index),
                action.name.clone(),
                command,
                false,
                background,
                disabled,
                move |this, window, cx| this.run_custom_action(run_action.clone(), window, cx),
                cx,
            );
            let panel = cx.weak_entity();
            let focus_handle = self.focus_handle.clone();
            let menu_action = action.clone();
            section = section.child(
                right_click_menu(("sail-manager-custom-action-context", index))
                    .trigger(move |_, _, _| row)
                    .menu(move |window, cx| {
                        let panel = panel.clone();
                        let run_target = menu_action.clone();
                        let remove_name = name.clone();
                        ContextMenu::build(window, cx, {
                            let focus_handle = focus_handle.clone();
                            move |menu, _, _| {
                                let run_panel = panel.clone();
                                let remove_panel = panel.clone();
                                let run_target = run_target.clone();
                                let remove_name = remove_name.clone();
                                menu.context(focus_handle)
                                    .item(ContextMenuEntry::new("Run").disabled(disabled).handler(
                                        move |window, cx| {
                                            let run_target = run_target.clone();
                                            run_panel
                                                .update(cx, |panel, cx| {
                                                    panel.run_custom_action(run_target, window, cx)
                                                })
                                                .log_err();
                                        },
                                    ))
                                    .item(ContextMenuEntry::new("Remove").handler(
                                        move |window, cx| {
                                            let remove_name = remove_name.clone();
                                            remove_panel
                                                .update(cx, |panel, cx| {
                                                    panel.remove_custom_action(
                                                        remove_name,
                                                        window,
                                                        cx,
                                                    )
                                                })
                                                .log_err();
                                        },
                                    ))
                            }
                        })
                    }),
            );
        }
        section.child(
            ListItem::new("sail-manager-add-action")
                .spacing(ListItemSpacing::Dense)
                .start_slot(
                    Icon::new(IconName::Plus)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(Label::new("Add Action…"))
                .on_click(
                    cx.listener(|this, _, window, cx| this.open_add_action_modal(window, cx)),
                ),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let project_name = match &self.state {
            PanelState::Ready { project_name, .. } => Some(project_name.clone()),
            _ => self.root_name(),
        };
        let status = if let Some(operation) = &self.operation
            && !matches!(operation, PanelOperation::Service { .. })
        {
            Some((Color::Accent, operation.status()))
        } else if self.refreshing && matches!(self.state, PanelState::Loading) {
            Some((Color::Accent, "Checking…"))
        } else {
            self.environment_status()
        };
        let files_description = match &self.state {
            PanelState::Ready { detection, .. } => {
                Some(self.compose_files_description(detection, cx))
            }
            _ => None,
        };
        let ports_remapped = self
            .client
            .as_ref()
            .is_some_and(|client| client.invocation().generated_override.is_some());
        let can_refresh = !matches!(self.state, PanelState::Remote | PanelState::NoProject);
        let can_configure = self.can_configure(cx);
        let showing_settings = self.showing_settings;
        let button_states = match &self.state {
            PanelState::Ready { snapshot, .. } => Some(header_button_states(
                snapshot.environment_state,
                self.operation.as_ref(),
            )),
            _ => None,
        };

        v_flex()
            .px_2()
            .py_1p5()
            .gap_0p5()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                h_flex()
                    .justify_between()
                    .gap_1()
                    .child(Label::new("Sail Manager").weight(FontWeight::MEDIUM))
                    .child(
                        h_flex()
                            .gap_0p5()
                            .when(can_configure, |this| {
                                this.child(
                                    IconButton::new("sail-manager-settings", IconName::Settings)
                                        .icon_size(IconSize::Small)
                                        .toggle_state(showing_settings)
                                        .tooltip(Tooltip::text("Project Settings"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_settings(window, cx)
                                        })),
                                )
                            })
                            .when(can_refresh, |this| {
                                this.child(
                                    IconButton::new("sail-manager-refresh", IconName::RotateCw)
                                        .icon_size(IconSize::Small)
                                        .disabled(self.refreshing)
                                        .tooltip(Tooltip::text("Refresh"))
                                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                                )
                            }),
                    ),
            )
            .when(project_name.is_some() || status.is_some(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .when_some(project_name, |this, project_name| {
                            this.child(
                                Label::new(project_name)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                        })
                        .when_some(status, |this, (color, text)| {
                            this.child(
                                h_flex().gap_1().child(Indicator::dot().color(color)).child(
                                    Label::new(text).size(LabelSize::Small).color(Color::Muted),
                                ),
                            )
                        }),
                )
            })
            .when_some(button_states, |this, (can_start, can_stop, can_restart)| {
                this.child(
                    h_flex()
                        .gap_1()
                        .pt_0p5()
                        .child(
                            Button::new("sail-manager-start", "Start")
                                .start_icon(Icon::new(IconName::PlayFilled).size(IconSize::Small))
                                .label_size(LabelSize::Small)
                                .size(ButtonSize::Compact)
                                .disabled(!can_start)
                                .on_click(cx.listener(|this, _, _, cx| this.start_environment(cx))),
                        )
                        .child(
                            Button::new("sail-manager-stop", "Stop")
                                .start_icon(Icon::new(IconName::Stop).size(IconSize::Small))
                                .label_size(LabelSize::Small)
                                .size(ButtonSize::Compact)
                                .disabled(!can_stop)
                                .on_click(cx.listener(|this, _, _, cx| this.stop_environment(cx))),
                        )
                        .child(
                            Button::new("sail-manager-restart", "Restart")
                                .start_icon(Icon::new(IconName::RotateCcw).size(IconSize::Small))
                                .label_size(LabelSize::Small)
                                .size(ButtonSize::Compact)
                                .disabled(!can_restart)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.restart_environment(cx)),
                                ),
                        ),
                )
            })
            .when_some(files_description, |this, (description, color)| {
                this.child(
                    h_flex()
                        .min_w_0()
                        .child(
                            Label::new(description)
                                .size(LabelSize::XSmall)
                                .color(color)
                                .truncate(),
                        )
                        .when(ports_remapped, |this| {
                            this.child(
                                div()
                                    .id("sail-manager-ports-remapped")
                                    .flex_none()
                                    .child(
                                        Label::new(" \u{b7} ports remapped")
                                            .size(LabelSize::XSmall)
                                            .color(Color::Warning),
                                    )
                                    .tooltip(Tooltip::text(
                                        "Sail Manager publishes some ports on different host ports. A `sail up` run outside Zed will use the original ports.",
                                    )),
                            )
                        }),
                )
            })
    }

    fn render_services(
        &self,
        snapshot: &ProjectSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let url_host = self.settings(cx).url_host.clone();
        let rows = snapshot
            .services
            .iter()
            .enumerate()
            .map(|(index, service)| self.render_service(index, service, &url_host, cx))
            .collect::<Vec<_>>();

        v_flex()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(
                div()
                    .id("sail-manager-services")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(
                        v_flex()
                            .p_1()
                            .child(
                                h_flex()
                                    .px_1()
                                    .py_1()
                                    .gap_1()
                                    .child(
                                        Label::new("Services")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(
                                        Label::new(snapshot.services.len().to_string())
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    ),
                            )
                            .children(rows)
                            .child(self.render_quick_actions(cx))
                            .child(self.render_custom_actions(cx)),
                    ),
            )
            .vertical_scrollbar_for(&self.scroll_handle, window, cx)
    }

    fn render_service(
        &self,
        index: usize,
        service: &Service,
        url_host: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_services.contains(&service.name);
        let published_ports = service
            .ports
            .iter()
            .filter_map(|port| port.host_port)
            .map(|port| port.to_string())
            .collect::<Vec<_>>()
            .join(" · ");
        let toggle_name = service.name.clone();
        let click_name = service.name.clone();
        let service_operation = match &self.operation {
            Some(PanelOperation::Service {
                service: operating_service,
                operation,
            }) if *operating_service == service.name => Some(*operation),
            _ => None,
        };
        let state_color = if service_operation.is_some() {
            Color::Accent
        } else {
            service_state_color(&service.state)
        };
        let busy = self.operation.is_some();
        let running = service.state.is_running();
        let is_sail = self.client.as_ref().is_some_and(|client| {
            matches!(client.invocation().launcher, ComposeLauncher::Sail { .. })
        });
        let manual_url = self.settings(cx).manual_urls.get(&service.name).cloned();
        let urls = compose::web_urls(service, url_host, manual_url.as_deref());
        let menu_items = service_menu_items(service, is_sail, &urls);
        let panel = cx.weak_entity();
        let focus_handle = self.focus_handle.clone();
        let menu_builder = {
            let service_name = service.name.clone();
            move |window: &mut Window, cx: &mut App| {
                build_service_menu(
                    panel.clone(),
                    service_name.clone(),
                    menu_items.clone(),
                    running,
                    busy,
                    focus_handle.clone(),
                    window,
                    cx,
                )
            }
        };
        let browser_url = urls.first().cloned();
        let terminal_name = service.name.clone();
        let logs_name = service.name.clone();
        let popover_menu_builder = menu_builder.clone();

        let row = v_flex()
            .child(
                ListItem::new(("sail-manager-service", index))
                    .spacing(ListItemSpacing::Dense)
                    .toggle(expanded)
                    .on_toggle(
                        cx.listener(move |this, _, _, cx| this.toggle_service(&toggle_name, cx)),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_service(&click_name, cx)),
                    )
                    .start_slot(Indicator::dot().color(state_color))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Icon::new(service_kind_icon(service.kind))
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(Label::new(service.name.clone()).truncate()),
                    )
                    .when_some(service_operation, |this, operation| {
                        this.end_slot(
                            Label::new(
                                PanelOperation::Service {
                                    service: String::new(),
                                    operation,
                                }
                                .status(),
                            )
                            .size(LabelSize::XSmall)
                            .color(Color::Accent),
                        )
                    })
                    .when(
                        service_operation.is_none() && !published_ports.is_empty(),
                        |this| {
                            this.end_slot(
                                Label::new(published_ports)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted)
                                    .buffer_font(cx),
                            )
                        },
                    )
                    .end_slot_on_hover(
                        h_flex()
                            .gap_0p5()
                            .when_some(browser_url, |this, url| {
                                this.child(
                                    IconButton::new(
                                        ("sail-manager-service-browser", index),
                                        IconName::Globe,
                                    )
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Open in Browser"))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.open_url(url.clone(), cx),
                                    )),
                                )
                            })
                            .when(running, |this| {
                                this.child(
                                    IconButton::new(
                                        ("sail-manager-service-terminal", index),
                                        IconName::TerminalAlt,
                                    )
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Open Terminal"))
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.open_terminal(
                                                &terminal_name,
                                                TerminalRequest::Shell { root: false },
                                                window,
                                                cx,
                                            )
                                        },
                                    )),
                                )
                            })
                            .child(
                                IconButton::new(
                                    ("sail-manager-service-logs", index),
                                    IconName::ScrollText,
                                )
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text("View Logs"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.view_logs(&logs_name, window, cx)
                                    },
                                )),
                            )
                            .child(
                                PopoverMenu::new(("sail-manager-service-menu", index))
                                    .trigger(
                                        IconButton::new(
                                            ("sail-manager-service-more", index),
                                            IconName::Ellipsis,
                                        )
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text("Service Actions")),
                                    )
                                    .menu(move |window, cx| Some(popover_menu_builder(window, cx)))
                                    .anchor(gpui::Anchor::TopRight),
                            ),
                    ),
            )
            .when(expanded, |this| {
                this.child(self.render_service_details(service, url_host, &urls, cx))
            });

        right_click_menu(("sail-manager-service-context", index))
            .trigger(move |_, _, _| row)
            .menu(menu_builder)
            .into_any_element()
    }

    fn render_service_details(
        &self,
        service: &Service,
        url_host: &str,
        urls: &[String],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status = service
            .status_text
            .clone()
            .unwrap_or_else(|| service_state_label(&service.state));
        let service_name = service.name.clone();
        let supports_url = matches!(
            service.kind,
            ServiceKind::App | ServiceKind::Mailpit | ServiceKind::PhpMyAdmin | ServiceKind::Other
        );
        let url_buttons = urls
            .iter()
            .enumerate()
            .map(|(url_index, url)| {
                let url_to_open = url.clone();
                Button::new(
                    SharedString::from(format!("sail-manager-url-{}-{url_index}", service.name)),
                    url.clone(),
                )
                .label_size(LabelSize::XSmall)
                .color(Color::Accent)
                .tooltip(Tooltip::text("Open in Browser"))
                .on_click(cx.listener(move |this, _, _, cx| this.open_url(url_to_open.clone(), cx)))
            })
            .collect::<Vec<_>>();

        v_flex()
            .pl_8()
            .pr_2()
            .pb_1()
            .gap_0p5()
            .when_some(service.container_name.clone(), |this, container_name| {
                this.child(detail_line("Container", container_name))
            })
            .when_some(service.image.clone(), |this, image| {
                this.child(detail_line("Image", image))
            })
            .child(detail_line("Status", status))
            .children(
                service
                    .ports
                    .iter()
                    .map(|port| detail_line("Port", port_description(port, url_host))),
            )
            .when(!url_buttons.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Label::new("URL")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .children(url_buttons),
                )
            })
            .when(urls.is_empty() && supports_url, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .child(
                            Label::new("No web interface detected ·")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(
                            Button::new(
                                SharedString::from(format!(
                                    "sail-manager-configure-url-{}",
                                    service.name
                                )),
                                "Configure URL…",
                            )
                            .label_size(LabelSize::XSmall)
                            .color(Color::Accent)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.configure_url(&service_name, window, cx)
                                },
                            )),
                        ),
                )
            })
    }

    fn render_message(
        &self,
        icon: Option<IconName>,
        title: &'static str,
        description: Option<String>,
        button_label: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px_4()
            .gap_1p5()
            .when_some(icon, |this, icon| {
                this.child(Icon::new(icon).size(IconSize::Medium).color(Color::Error))
            })
            .child(Label::new(title).color(Color::Muted))
            .when_some(description, |this, description| {
                this.child(
                    Label::new(description)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
            })
            .when_some(button_label, |this, label| {
                this.child(
                    Button::new("sail-manager-retry", label)
                        .label_size(LabelSize::Small)
                        .style(ButtonStyle::Outlined)
                        .disabled(self.refreshing)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            })
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.showing_settings
            && let Some(settings_view) = self.settings_view.clone()
        {
            return settings_view.into_any_element();
        }
        match &self.state {
            PanelState::Ready { snapshot, .. } => self
                .render_services(snapshot, window, cx)
                .into_any_element(),
            PanelState::Loading => self
                .render_message(None, "Checking Docker Compose…", None, None, cx)
                .into_any_element(),
            PanelState::Remote => self
                .render_message(
                    None,
                    "Sail Manager only works with local projects",
                    None,
                    None,
                    cx,
                )
                .into_any_element(),
            PanelState::NoProject => self
                .render_message(
                    None,
                    "Open a project to manage its containers",
                    None,
                    None,
                    cx,
                )
                .into_any_element(),
            PanelState::NotCompose => self
                .render_message(
                    None,
                    "No Docker Compose configuration found",
                    Some(
                        "Sail Manager looks for compose.yaml, compose.yml, docker-compose.yaml or docker-compose.yml in the project root. No files will be changed."
                            .to_string(),
                    ),
                    Some("Check Again"),
                    cx,
                )
                .into_any_element(),
            PanelState::DockerUnavailable(message) => self
                .render_message(
                    Some(IconName::Warning),
                    "Docker is not available",
                    Some(message.clone()),
                    Some("Retry"),
                    cx,
                )
                .into_any_element(),
            PanelState::Error(message) => self
                .render_message(
                    Some(IconName::Warning),
                    "Could not read the Docker Compose state",
                    Some(message.clone()),
                    Some("Retry"),
                    cx,
                )
                .into_any_element(),
        }
    }
}

struct Confirmation {
    message: String,
    detail: String,
    button: &'static str,
}

fn quick_action_confirmation(action: QuickAction) -> Option<Confirmation> {
    let (message, detail) = match action {
        QuickAction::ComposerUpdate => (
            "Run composer update?",
            "This updates dependencies and modifies composer.lock.",
        ),
        QuickAction::MigrateSeed => (
            "Run migrations with seeders?",
            "Seeders will write data to the local database.",
        ),
        _ => return None,
    };
    Some(Confirmation {
        message: message.to_string(),
        detail: detail.to_string(),
        button: "Run",
    })
}

fn custom_action_confirmation(action: &CustomAction) -> Option<Confirmation> {
    action.confirm.then(|| Confirmation {
        message: format!("Run {}?", action.name),
        detail: "This action is configured to ask for confirmation.".to_string(),
        button: "Run",
    })
}

fn build_service_menu(
    panel: WeakEntity<SailManagerPanel>,
    service_name: String,
    items: Vec<ServiceMenuItem>,
    running: bool,
    busy: bool,
    focus_handle: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    ContextMenu::build(window, cx, move |menu, _, _| {
        let mut menu = menu.context(focus_handle);
        for item in items {
            if item == ServiceMenuItem::Separator {
                menu = menu.separator();
                continue;
            }
            let disabled = match &item {
                ServiceMenuItem::OpenTerminal
                | ServiceMenuItem::OpenRootTerminal
                | ServiceMenuItem::OpenClient { .. } => !running,
                ServiceMenuItem::Operation(_) => busy,
                _ => false,
            };
            let panel = panel.clone();
            let service_name = service_name.clone();
            let label = item.label();
            menu = menu.item(ContextMenuEntry::new(label).disabled(disabled).handler(
                move |window, cx| {
                    let service_name = service_name.clone();
                    let item = item.clone();
                    panel
                        .update(cx, |panel, cx| match item {
                            ServiceMenuItem::OpenInBrowser { url, .. } => panel.open_url(url, cx),
                            ServiceMenuItem::OpenTerminal => panel.open_terminal(
                                &service_name,
                                TerminalRequest::Shell { root: false },
                                window,
                                cx,
                            ),
                            ServiceMenuItem::OpenRootTerminal => panel.open_terminal(
                                &service_name,
                                TerminalRequest::Shell { root: true },
                                window,
                                cx,
                            ),
                            ServiceMenuItem::OpenClient { .. } => panel.open_terminal(
                                &service_name,
                                TerminalRequest::Client,
                                window,
                                cx,
                            ),
                            ServiceMenuItem::ViewLogs => panel.view_logs(&service_name, window, cx),
                            ServiceMenuItem::CopyPort(port) => panel.copy_port(port, cx),
                            ServiceMenuItem::Operation(operation) => {
                                panel.run_service_operation(service_name, operation, cx)
                            }
                            ServiceMenuItem::ConfigureUrl => {
                                panel.configure_url(&service_name, window, cx)
                            }
                            ServiceMenuItem::Separator => {}
                        })
                        .log_err();
                },
            ));
        }
        menu
    })
}

fn detail_line(label: &'static str, value: String) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(
            Label::new(label)
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(Label::new(value).size(LabelSize::XSmall).truncate())
}

fn port_description(port: &PortMapping, url_host: &str) -> String {
    match port.host_port {
        Some(host_port) => {
            let host = port
                .host_ip
                .as_deref()
                .filter(|host_ip| !host_ip.is_empty())
                .unwrap_or(url_host);
            format!(
                "{host}:{host_port} → {}/{}",
                port.container_port, port.protocol
            )
        }
        None => format!("{}/{}", port.container_port, port.protocol),
    }
}

fn service_kind_icon(kind: ServiceKind) -> IconName {
    match kind {
        ServiceKind::App | ServiceKind::Mailpit | ServiceKind::PhpMyAdmin => IconName::Globe,
        ServiceKind::Mysql | ServiceKind::Mariadb | ServiceKind::Postgres => IconName::Database,
        ServiceKind::Redis | ServiceKind::Other => IconName::Server,
    }
}

fn service_state_color(state: &ServiceState) -> Color {
    match state {
        ServiceState::Running {
            health: Some(Health::Unhealthy),
        } => Color::Error,
        ServiceState::Running {
            health: Some(Health::Starting),
        } => Color::Warning,
        ServiceState::Running { .. } => Color::Success,
        ServiceState::Starting | ServiceState::Restarting | ServiceState::Paused => Color::Warning,
        ServiceState::Exited { exit_code } if *exit_code != 0 => Color::Error,
        ServiceState::Exited { .. }
        | ServiceState::Created
        | ServiceState::NotCreated
        | ServiceState::Unknown(_) => Color::Muted,
    }
}

fn service_state_label(state: &ServiceState) -> String {
    match state {
        ServiceState::Running { health: None } => "Running".to_string(),
        ServiceState::Running {
            health: Some(Health::Healthy),
        } => "Running (healthy)".to_string(),
        ServiceState::Running {
            health: Some(Health::Unhealthy),
        } => "Running (unhealthy)".to_string(),
        ServiceState::Running {
            health: Some(Health::Starting),
        } => "Running (starting)".to_string(),
        ServiceState::Starting => "Starting".to_string(),
        ServiceState::Restarting => "Restarting".to_string(),
        ServiceState::Paused => "Paused".to_string(),
        ServiceState::Exited { exit_code } => format!("Exited ({exit_code})"),
        ServiceState::Created => "Created".to_string(),
        ServiceState::NotCreated => "Not created".to_string(),
        ServiceState::Unknown(state) => state.clone(),
    }
}

fn docker_error_message(error: &DockerError) -> String {
    match error {
        DockerError::DaemonUnavailable { detail } | DockerError::CommandFailed { detail, .. }
            if !detail.is_empty() =>
        {
            format!("{error}: {detail}")
        }
        _ => error.to_string(),
    }
}

fn anyhow_error_message(error: &anyhow::Error) -> String {
    match error.downcast_ref::<DockerError>() {
        Some(docker_error) => docker_error_message(docker_error),
        None => format!("{error:#}"),
    }
}

async fn load_compose_state(request: RefreshRequest) -> RefreshOutcome {
    let detection = match docker::read_project_files(request.fs.as_ref(), &request.root).await {
        Ok(files) => files.detect(),
        Err(error) => return RefreshOutcome::Error(anyhow_error_message(&error)),
    };
    if detection.compose_files.is_none() && request.compose_files.is_empty() {
        return RefreshOutcome::NotCompose;
    }
    let canonical = docker::canonical_root(request.fs.as_ref(), &request.root).await;
    let path_variable = request.environment.get("PATH").cloned();
    let Some(docker_binary) = (request.find_docker)(path_variable.as_deref(), &request.root) else {
        return RefreshOutcome::DockerUnavailable(DockerError::DockerNotFound.to_string());
    };
    let launcher = if detection.is_sail && detection.has_vendor_sail_binary {
        ComposeLauncher::Sail {
            sail_script: request.root.join("vendor/bin/sail"),
        }
    } else {
        ComposeLauncher::DockerCompose {
            docker_binary: docker_binary.clone(),
        }
    };
    let mut client = ComposeClient::new(
        request.runner,
        docker_binary,
        request.environment,
        ComposeInvocation {
            launcher,
            project_directory: request.root.to_path_buf(),
            files: request.compose_files.clone(),
            generated_override: None,
        },
    );
    let compose_version = if request.check_availability {
        match client.check_availability().await {
            Ok(version) => Some(version),
            Err(error) => return RefreshOutcome::DockerUnavailable(docker_error_message(&error)),
        }
    } else {
        None
    };
    let override_supported = compose_version
        .as_deref()
        .or(request.known_compose_version.as_deref())
        .is_some_and(ports::compose_supports_port_override);
    let override_path = ports::generated_override_path(&canonical);
    if request.override_enabled && override_supported && request.fs.is_file(&override_path).await {
        let files = if !request.compose_files.is_empty() {
            request.compose_files.clone()
        } else {
            detection
                .compose_files
                .as_ref()
                .map(|files| files.all())
                .unwrap_or_default()
        };
        client = client.reconfigured(files, Some(override_path));
    }
    match client.snapshot(&request.app_service).await {
        Ok(snapshot) => RefreshOutcome::Ready {
            detection,
            snapshot,
            client,
            compose_version,
            assignments: request
                .store
                .load()
                .await
                .log_err()
                .and_then(|registry| {
                    registry
                        .get(&canonical.to_string_lossy().into_owned())
                        .cloned()
                })
                .unwrap_or_default(),
        },
        Err(error) => RefreshOutcome::Error(anyhow_error_message(&error)),
    }
}

impl EventEmitter<PanelEvent> for SailManagerPanel {}

impl Focusable for SailManagerPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SailManagerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("sail-manager-panel")
            .key_context("SailManagerPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::refresh_action))
            .on_action(cx.listener(Self::start_environment_action))
            .on_action(cx.listener(Self::stop_environment_action))
            .on_action(cx.listener(Self::restart_environment_action))
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_header(cx))
            .child(self.render_body(window, cx))
    }
}

impl Panel for SailManagerPanel {
    fn persistent_name() -> &'static str {
        "Sail Manager"
    }

    fn panel_key() -> &'static str {
        "SailManagerPanel"
    }

    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        match SailManagerSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left | DockPosition::Bottom => DockSide::Left,
                DockPosition::Right => DockSide::Right,
            };
            settings.sail_manager.get_or_insert_default().dock = Some(dock);
        });
    }

    fn default_size(&self, _: &Window, cx: &App) -> Pixels {
        SailManagerSettings::get_global(cx).default_width
    }

    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> {
        SailManagerSettings::get_global(cx)
            .button
            .then_some(IconName::Container)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Sail Manager")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.sync_background_tasks(cx);
        if active {
            self.refresh(cx);
        }
    }

    fn activation_priority(&self) -> u32 {
        8
    }

    fn hide_button_setting(&self, _: &App) -> Option<workspace::HideStatusItem> {
        Some(workspace::HideStatusItem::new(|settings| {
            settings.sail_manager.get_or_insert_default().button = Some(false);
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{CommandOutput, LineStream};
    use fs::FakeFs;
    use futures::future::BoxFuture;
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;

    const SAIL_CONFIG: &str = r#"{
        "name": "blog",
        "services": {
            "laravel.test": {"image": "sail-8.4/app", "ports": [{"mode": "ingress", "target": 80, "published": "80", "protocol": "tcp"}]},
            "mysql": {"image": "mysql/mysql-server:8.0", "ports": [{"mode": "ingress", "target": 3306, "published": "3306", "protocol": "tcp"}]},
            "redis": {"image": "redis:alpine", "ports": [{"mode": "ingress", "target": 6379, "published": "6379", "protocol": "tcp"}]},
            "mailpit": {"image": "axllent/mailpit:latest", "ports": [{"mode": "ingress", "target": 8025, "published": "8025", "protocol": "tcp"}]},
            "meilisearch": {"image": "getmeili/meilisearch:latest", "ports": [{"mode": "ingress", "target": 7700, "published": "7700", "protocol": "tcp"}]}
        }
    }"#;

    const SAIL_PS: &str = concat!(
        r#"{"Name":"blog-laravel.test-1","Service":"laravel.test","State":"running","Status":"Up 2 hours","Health":"","ExitCode":0,"Image":"sail-8.4/app","Publishers":[{"URL":"0.0.0.0","TargetPort":80,"PublishedPort":80,"Protocol":"tcp"}]}"#,
        "\n",
        r#"{"Name":"blog-mysql-1","Service":"mysql","State":"running","Status":"Up 2 hours (healthy)","Health":"healthy","ExitCode":0,"Publishers":null}"#,
        "\n",
        r#"{"Name":"blog-redis-1","Service":"redis","State":"exited","Status":"Exited (0) 1 hour ago","Health":"","ExitCode":0,"Publishers":null}"#,
        "\n",
    );

    type Responder = Box<dyn Fn(&[String]) -> CommandOutput + Send + Sync>;

    struct FakeCommandRunner {
        responder: Responder,
        programs: Arc<std::sync::Mutex<Vec<PathBuf>>>,
        calls: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        environments: Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>,
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(
            &self,
            program: &Path,
            args: Vec<String>,
            environment: HashMap<String, String>,
            _working_directory: PathBuf,
        ) -> BoxFuture<'static, anyhow::Result<CommandOutput>> {
            if let Ok(mut environments) = self.environments.lock() {
                environments.push(environment);
            }
            if let Ok(mut programs) = self.programs.lock() {
                programs.push(program.to_path_buf());
            }
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(args.clone());
            }
            let output = (self.responder)(&args);
            Box::pin(async move { Ok(output) })
        }

        fn stream_lines(
            &self,
            _program: &Path,
            _args: Vec<String>,
            _environment: HashMap<String, String>,
            _working_directory: PathBuf,
        ) -> anyhow::Result<LineStream> {
            Ok(LineStream::pending())
        }
    }

    fn runner(
        responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
    ) -> Arc<dyn CommandRunner> {
        recording_runner(responder).0
    }

    fn recording_runner(
        responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
    ) -> (Arc<dyn CommandRunner>, Arc<std::sync::Mutex<Vec<PathBuf>>>) {
        let programs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = Arc::new(FakeCommandRunner {
            responder: Box::new(responder),
            programs: programs.clone(),
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
            environments: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        (runner, programs)
    }

    type RecordedCalls = Arc<std::sync::Mutex<Vec<Vec<String>>>>;

    fn call_recording_runner(
        responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
    ) -> (Arc<dyn CommandRunner>, RecordedCalls) {
        let calls: RecordedCalls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = Arc::new(FakeCommandRunner {
            responder: Box::new(responder),
            programs: Arc::new(std::sync::Mutex::new(Vec::new())),
            calls: calls.clone(),
            environments: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        (runner, calls)
    }

    type RecordedEnvironments = Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>;

    fn environment_recording_runner(
        responder: impl Fn(&[String]) -> CommandOutput + Send + Sync + 'static,
    ) -> (Arc<dyn CommandRunner>, RecordedCalls, RecordedEnvironments) {
        let calls: RecordedCalls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let environments: RecordedEnvironments = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = Arc::new(FakeCommandRunner {
            responder: Box::new(responder),
            programs: Arc::new(std::sync::Mutex::new(Vec::new())),
            calls: calls.clone(),
            environments: environments.clone(),
        });
        (runner, calls, environments)
    }

    fn mutating_calls(calls: &RecordedCalls) -> Vec<Vec<String>> {
        calls
            .lock()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|args| {
                        args.first().is_some_and(|first| {
                            matches!(first.as_str(), "up" | "stop" | "restart" | "down")
                        })
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn ps_call_count(calls: &RecordedCalls) -> usize {
        calls
            .lock()
            .map(|calls| calls.iter().filter(|args| has_argument(args, "ps")).count())
            .unwrap_or_default()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn failing_up_responder(args: &[String]) -> CommandOutput {
        if args.first().is_some_and(|first| first == "up") {
            output(false, "", "port is already allocated")
        } else {
            sail_responder(args)
        }
    }

    fn output(success: bool, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            status_success: success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    fn has_argument(args: &[String], argument: &str) -> bool {
        args.iter().any(|candidate| candidate == argument)
    }

    fn sail_responder(args: &[String]) -> CommandOutput {
        if args.first().is_some_and(|first| first == "info") {
            output(true, "27.0.3\n", "")
        } else if has_argument(args, "version") {
            output(true, "2.29.1\n", "")
        } else if has_argument(args, "config") {
            output(true, SAIL_CONFIG, "")
        } else if has_argument(args, "ps") {
            output(true, SAIL_PS, "")
        } else {
            output(true, "", "")
        }
    }

    fn daemon_down_responder(args: &[String]) -> CommandOutput {
        if args.first().is_some_and(|first| first == "info") {
            output(false, "", "Cannot connect to the Docker daemon")
        } else {
            sail_responder(args)
        }
    }

    fn fake_find_docker(_: Option<&str>, _: &Path) -> Option<PathBuf> {
        Some(PathBuf::from("/usr/bin/docker"))
    }

    fn no_docker(_: Option<&str>, _: &Path) -> Option<PathBuf> {
        None
    }

    fn new_test_panel(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        command_runner: Arc<dyn CommandRunner>,
        find_docker: DockerFinder,
        cx: &mut Context<SailManagerPanel>,
    ) -> SailManagerPanel {
        SailManagerPanel::new_with_ports(
            workspace,
            project,
            command_runner,
            find_docker,
            ports::InMemoryAssignmentStore::new(),
            ports::FakePortProbe::new(&[]),
            cx,
        )
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    async fn sail_project(cx: &mut TestAppContext) -> Entity<Project> {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                "compose.yaml": "services: {}",
                "composer.json": r#"{"require-dev": {"laravel/sail": "^1.0"}}"#,
                "vendor": {"bin": {"sail": "#!/bin/sh"}},
            }),
        )
        .await;
        Project::test(fs, [path!("/work/blog").as_ref()], cx).await
    }

    fn new_panel(
        project: &Entity<Project>,
        command_runner: Arc<dyn CommandRunner>,
        cx: &mut TestAppContext,
    ) -> Entity<SailManagerPanel> {
        let project = project.clone();
        cx.new(|cx| {
            new_test_panel(
                WeakEntity::new_invalid(),
                project,
                command_runner,
                fake_find_docker,
                cx,
            )
        })
    }

    #[test]
    fn test_header_button_states() {
        assert_eq!(
            header_button_states(EnvironmentState::Running, None),
            (false, true, true)
        );
        assert_eq!(
            header_button_states(EnvironmentState::Stopped, None),
            (true, false, false)
        );
        assert_eq!(
            header_button_states(EnvironmentState::Partial, None),
            (true, true, true)
        );
        assert_eq!(
            header_button_states(EnvironmentState::NoServices, None),
            (false, false, false)
        );
        for state in [
            EnvironmentState::Running,
            EnvironmentState::Stopped,
            EnvironmentState::Partial,
            EnvironmentState::NoServices,
        ] {
            assert_eq!(
                header_button_states(state, Some(&PanelOperation::StartingEnvironment)),
                (false, false, false)
            );
        }
    }

    #[gpui::test]
    async fn test_environment_operations_run_sail_without_down(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls) = call_recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();

        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();
        panel.update(cx, |panel, cx| panel.stop_environment(cx));
        cx.run_until_parked();
        panel.update(cx, |panel, cx| panel.restart_environment(cx));
        cx.run_until_parked();

        assert_eq!(
            mutating_calls(&calls),
            vec![
                strings(&["up", "-d"]),
                strings(&["stop"]),
                strings(&["restart"])
            ]
        );
        let all_calls = calls.lock().map(|calls| calls.clone()).unwrap_or_default();
        assert!(
            all_calls
                .iter()
                .all(|args| !has_argument(args, "down") && !has_argument(args, "-v"))
        );
    }

    #[gpui::test]
    async fn test_service_operations_run_sail_for_the_service(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls) = call_recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();

        for (operation, service) in [
            (ServiceOperation::Restart, "mysql"),
            (ServiceOperation::Stop, "redis"),
            (ServiceOperation::Start, "mailpit"),
        ] {
            panel.update(cx, |panel, cx| {
                panel.run_service_operation(service.to_string(), operation, cx)
            });
            cx.run_until_parked();
        }

        assert_eq!(
            mutating_calls(&calls),
            vec![
                strings(&["restart", "mysql"]),
                strings(&["stop", "redis"]),
                strings(&["up", "-d", "mailpit"]),
            ]
        );
    }

    #[gpui::test]
    async fn test_second_operation_is_ignored_while_one_is_running(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls) = call_recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();

        panel.update(cx, |panel, cx| {
            panel.start_environment(cx);
            assert_eq!(panel.operation, Some(PanelOperation::StartingEnvironment));
            panel.stop_environment(cx);
            panel.restart_environment(cx);
            panel.run_service_operation("redis".to_string(), ServiceOperation::Stop, cx);
            assert_eq!(panel.operation, Some(PanelOperation::StartingEnvironment));
        });
        cx.run_until_parked();

        assert_eq!(mutating_calls(&calls), vec![strings(&["up", "-d"])]);
    }

    #[gpui::test]
    async fn test_finished_operation_is_cleared_and_refreshes(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls) = call_recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();
        let refreshes_before = ps_call_count(&calls);

        panel.update(cx, |panel, cx| panel.restart_environment(cx));
        cx.run_until_parked();

        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.operation, None);
            assert!(matches!(panel.state, PanelState::Ready { .. }));
        });
        assert_eq!(ps_call_count(&calls), refreshes_before + 1);
    }

    #[gpui::test]
    async fn test_failed_operation_clears_state_and_refreshes(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls) = call_recording_runner(failing_up_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();
        let refreshes_before = ps_call_count(&calls);

        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();

        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.operation, None);
            assert!(matches!(panel.state, PanelState::Ready { .. }));
        });
        assert!(ps_call_count(&calls) > refreshes_before);
    }

    #[gpui::test]
    async fn test_operation_without_client_does_nothing(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls) = call_recording_runner(daemon_down_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();

        panel.update(cx, |panel, cx| {
            panel.start_environment(cx);
            assert_eq!(panel.operation, None);
        });
        cx.run_until_parked();
        assert!(mutating_calls(&calls).is_empty());
    }

    #[gpui::test]
    async fn test_operation_shows_a_toast(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| theme_settings::init(theme::LoadThemes::JustBase, cx));
        let project = sail_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (command_runner, _) = call_recording_runner(failing_up_responder);
        let weak_workspace = workspace.downgrade();
        let panel = cx.new(|cx| {
            new_test_panel(
                weak_workspace,
                project.clone(),
                command_runner,
                fake_find_docker,
                cx,
            )
        });
        cx.run_until_parked();

        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.has_notification(&NotificationId::unique::<SailManagerToast>()));
        });
    }

    #[test]
    fn test_operation_messages() {
        assert_eq!(
            PanelOperation::StartingEnvironment.success_message(),
            "Environment started"
        );
        assert_eq!(
            PanelOperation::StoppingEnvironment.success_message(),
            "Environment stopped (volumes kept)"
        );
        assert_eq!(
            PanelOperation::Service {
                service: "redis".to_string(),
                operation: ServiceOperation::Restart
            }
            .success_message(),
            "Service redis restarted"
        );
        assert_eq!(
            PanelOperation::RestartingEnvironment.failure_message("boom"),
            "Could not restart the environment: boom"
        );
    }

    #[gpui::test]
    async fn test_folder_without_compose_is_not_compose(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/work/empty"), json!({"README.md": "hello"}))
            .await;
        let project = Project::test(fs, [path!("/work/empty").as_ref()], cx).await;
        let panel = new_panel(&project, runner(sail_responder), cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::NotCompose));
            assert!(!panel.refreshing);
        });
    }

    #[gpui::test]
    async fn test_folder_without_compose_and_without_docker_is_not_compose(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/work/empty"), json!({"README.md": "hello"}))
            .await;
        let project = Project::test(fs, [path!("/work/empty").as_ref()], cx).await;
        let command_runner = runner(sail_responder);
        let panel = cx.new(|cx| {
            new_test_panel(
                WeakEntity::new_invalid(),
                project,
                command_runner,
                no_docker,
                cx,
            )
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::NotCompose));
        });
    }

    #[gpui::test]
    async fn test_compose_project_without_docker_is_docker_unavailable(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let command_runner = runner(sail_responder);
        let panel = cx.new(|cx| {
            new_test_panel(
                WeakEntity::new_invalid(),
                project,
                command_runner,
                no_docker,
                cx,
            )
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::DockerUnavailable(_)));
        });
    }

    #[gpui::test]
    async fn test_sail_project_uses_the_sail_script(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, programs) = recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::Ready { .. }));
        });
        let programs = programs.lock().unwrap();
        let sail = PathBuf::from(path!("/work/blog")).join("vendor/bin/sail");
        assert!(programs.contains(&sail));
        assert!(programs.contains(&PathBuf::from("/usr/bin/docker")));
        assert_eq!(
            programs
                .iter()
                .filter(|program| program.as_path() == Path::new("/usr/bin/docker"))
                .count(),
            2
        );
    }

    #[gpui::test]
    async fn test_project_without_sail_uses_docker(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/plain"),
            json!({"compose.yaml": "services: {}"}),
        )
        .await;
        let project = Project::test(fs, [path!("/work/plain").as_ref()], cx).await;
        let (command_runner, programs) = recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::Ready { .. }));
        });
        let programs = programs.lock().unwrap();
        assert!(!programs.is_empty());
        assert!(
            programs
                .iter()
                .all(|program| program.as_path() == Path::new("/usr/bin/docker"))
        );
    }

    #[gpui::test]
    async fn test_sail_without_vendor_binary_falls_back_to_docker(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                "compose.yaml": "services: {}",
                "composer.json": r#"{"require-dev": {"laravel/sail": "^1.0"}}"#,
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/work/blog").as_ref()], cx).await;
        let (command_runner, programs) = recording_runner(sail_responder);
        let panel = new_panel(&project, command_runner, cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| match &panel.state {
            PanelState::Ready { detection, .. } => {
                assert!(detection.is_sail);
                assert!(!detection.has_vendor_sail_binary);
                let (text, color) = panel.compose_files_description(detection, cx);
                assert_eq!(text, "vendor/bin/sail not found · run composer install");
                assert_eq!(color, Color::Warning);
            }
            state => panic!("unexpected state {state:?}"),
        });
        assert!(
            programs
                .lock()
                .unwrap()
                .iter()
                .all(|program| program.as_path() == Path::new("/usr/bin/docker"))
        );
    }

    #[gpui::test]
    async fn test_sail_project_is_ready_and_partial(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let panel = new_panel(&project, runner(sail_responder), cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| match &panel.state {
            PanelState::Ready {
                project_name,
                detection,
                snapshot,
            } => {
                assert_eq!(project_name, "blog");
                assert!(detection.is_sail);
                assert_eq!(snapshot.services.len(), 5);
                assert_eq!(snapshot.environment_state, EnvironmentState::Partial);
            }
            state => panic!("unexpected state {state:?}"),
        });
    }

    #[gpui::test]
    async fn test_daemon_down_is_docker_unavailable(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let panel = new_panel(&project, runner(daemon_down_responder), cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| match &panel.state {
            PanelState::DockerUnavailable(message) => {
                assert!(message.contains("Docker daemon is not reachable"));
            }
            state => panic!("unexpected state {state:?}"),
        });
    }

    #[gpui::test]
    async fn test_settings_view_follows_external_setting_changes(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| theme_settings::init(theme::LoadThemes::JustBase, cx));
        cx.update(editor::init);
        let project = sail_project(cx).await;
        let (panel, cx) = cx.add_window_view(|_, cx| {
            new_test_panel(
                WeakEntity::new_invalid(),
                project,
                runner(sail_responder),
                fake_find_docker,
                cx,
            )
        });
        cx.run_until_parked();
        panel.update_in(cx, |panel, window, cx| panel.toggle_settings(window, cx));
        let view = panel.read_with(cx, |panel, _| {
            panel.settings_view.clone().expect("settings view is open")
        });
        let worktree_id = panel.read_with(cx, |panel, _| {
            panel.root.as_ref().map(|(worktree_id, _)| *worktree_id)
        });
        let worktree_id = worktree_id.expect("panel has a root");
        assert_eq!(
            view.read_with(cx, |view, cx| view.field_texts(cx)),
            ("localhost".to_string(), "laravel.test".to_string())
        );
        cx.update(|_, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty().into()),
                        settings::LocalSettingsKind::Settings,
                        Some(
                            r#"{"sail_manager": {"app_service": "app", "url_host": "blog.test"}}"#,
                        ),
                        cx,
                    )
                    .expect("local settings are valid");
            });
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, cx| view.field_texts(cx)),
            ("blog.test".to_string(), "app".to_string())
        );
    }

    #[gpui::test]
    async fn test_stale_refresh_result_is_ignored(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let panel = new_panel(&project, runner(sail_responder), cx);
        panel.update(cx, |panel, _| panel.generation += 1);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::Loading));
        });

        panel.update(cx, |panel, cx| panel.refresh(cx));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::Ready { .. }));
            assert!(!panel.refreshing);
        });
    }

    #[gpui::test]
    async fn test_project_settings_override_compose_options(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let panel = new_panel(&project, runner(sail_responder), cx);
        cx.run_until_parked();
        let worktree_id = panel.read_with(cx, |panel, _| {
            panel.root.as_ref().map(|(worktree_id, _)| *worktree_id)
        });
        let worktree_id = worktree_id.expect("panel has a root");
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty().into()),
                        settings::LocalSettingsKind::Settings,
                        Some(
                            r#"{"sail_manager": {"app_service": "app", "url_host": "blog.test", "compose_files": ["docker/compose.yaml"]}}"#,
                        ),
                        cx,
                    )
                    .expect("local settings are valid");
            });
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.observed_settings,
                (
                    "app".to_string(),
                    "blog.test".to_string(),
                    vec!["docker/compose.yaml".to_string()]
                )
            );
            let settings = panel.settings(cx);
            assert_eq!(settings.app_service, "app");
            assert_eq!(settings.url_host, "blog.test");
            assert_eq!(
                settings.compose_files,
                vec!["docker/compose.yaml".to_string()]
            );
            assert_eq!(settings.default_width, gpui::px(320.));
            let global = SailManagerSettings::get_global(cx);
            assert_eq!(global.app_service, "laravel.test");
            assert!(global.compose_files.is_empty());
        });
    }

    fn menu_service(
        name: &str,
        kind: ServiceKind,
        running: bool,
        host_port: Option<u16>,
    ) -> Service {
        Service {
            name: name.to_string(),
            image: None,
            kind,
            state: if running {
                ServiceState::Running { health: None }
            } else {
                ServiceState::NotCreated
            },
            status_text: None,
            container_name: None,
            ports: host_port
                .map(|host_port| PortMapping {
                    host_ip: None,
                    host_port: Some(host_port),
                    container_port: 3306,
                    protocol: "tcp".to_string(),
                })
                .into_iter()
                .collect(),
        }
    }

    fn menu_labels(items: &[ServiceMenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                ServiceMenuItem::Separator => "-".to_string(),
                item => item.label(),
            })
            .collect()
    }

    #[test]
    fn test_service_menu_for_the_app() {
        let app = menu_service("laravel.test", ServiceKind::App, true, None);
        let one_url = ["http://localhost".to_string()];
        assert_eq!(
            menu_labels(&service_menu_items(&app, true, &one_url)),
            [
                "Open in Browser",
                "Open Terminal",
                "Open Root Terminal",
                "View Logs",
                "-",
                "Restart Service",
                "Stop Service",
                "-",
                "Configure URL…"
            ]
        );
        let two_urls = [
            "http://localhost".to_string(),
            "http://localhost:5173".to_string(),
        ];
        assert_eq!(
            menu_labels(&service_menu_items(&app, false, &two_urls)),
            [
                "http://localhost",
                "http://localhost:5173",
                "Open Terminal",
                "View Logs",
                "-",
                "Restart Service",
                "Stop Service",
                "-",
                "Configure URL…"
            ]
        );
        let stopped = menu_service("laravel.test", ServiceKind::App, false, None);
        assert_eq!(
            menu_labels(&service_menu_items(&stopped, true, &[])),
            [
                "Open Terminal",
                "Open Root Terminal",
                "View Logs",
                "-",
                "Start Service",
                "-",
                "Configure URL…"
            ]
        );
    }

    #[test]
    fn test_service_menu_for_databases_and_redis() {
        let labels = |kind, port| {
            menu_labels(&service_menu_items(
                &menu_service("db", kind, true, port),
                true,
                &[],
            ))
        };
        assert_eq!(
            labels(ServiceKind::Mysql, Some(3306)),
            [
                "Open MySQL Client",
                "Open Terminal",
                "View Logs",
                "-",
                "Copy Port",
                "Restart Service",
                "Stop Service"
            ]
        );
        assert_eq!(labels(ServiceKind::Mariadb, None)[0], "Open MariaDB Client");
        assert_eq!(
            labels(ServiceKind::Postgres, None),
            [
                "Open PostgreSQL Client",
                "Open Terminal",
                "View Logs",
                "-",
                "Restart Service",
                "Stop Service"
            ]
        );
        assert_eq!(
            labels(ServiceKind::Redis, Some(6379)),
            [
                "Open redis-cli",
                "Open Terminal",
                "View Logs",
                "-",
                "Restart Service",
                "Stop Service"
            ]
        );
    }

    #[test]
    fn test_service_menu_for_other_services() {
        let mailpit = menu_service("mailpit", ServiceKind::Mailpit, true, Some(8025));
        let urls = ["http://localhost:8025".to_string()];
        assert_eq!(
            menu_labels(&service_menu_items(&mailpit, true, &urls)),
            [
                "Open in Browser",
                "Open Terminal",
                "View Logs",
                "-",
                "Restart Service",
                "Stop Service",
                "-",
                "Configure URL…"
            ]
        );
        let other = menu_service("meilisearch", ServiceKind::Other, true, None);
        assert_eq!(
            menu_labels(&service_menu_items(&other, true, &[])),
            [
                "Open Terminal",
                "View Logs",
                "-",
                "Restart Service",
                "Stop Service",
                "-",
                "Configure URL…"
            ]
        );
    }

    #[test]
    fn test_shell_target_follows_the_preference() {
        assert_eq!(
            shell_target(ShellPreference::Auto, &["zsh", "bash", "sh"], false),
            Some(TerminalTarget::Shell {
                shell: "bash".to_string(),
                root: false
            })
        );
        assert_eq!(
            shell_target(ShellPreference::Zsh, &["zsh", "sh"], true),
            Some(TerminalTarget::Shell {
                shell: "zsh".to_string(),
                root: true
            })
        );
        assert_eq!(shell_target(ShellPreference::Auto, &[], false), None);
        assert_eq!(shell_target(ShellPreference::Sh, &["bash"], false), None);
    }

    #[gpui::test]
    async fn test_project_settings_manual_urls_reach_the_panel(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let panel = new_panel(&project, runner(sail_responder), cx);
        cx.run_until_parked();
        let worktree_id = panel
            .read_with(cx, |panel, _| {
                panel.root.as_ref().map(|(worktree_id, _)| *worktree_id)
            })
            .expect("panel has a root");
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty().into()),
                        settings::LocalSettingsKind::Settings,
                        Some(
                            r#"{"sail_manager": {"shell": "zsh", "manual_urls": {"mailpit": "https://mail.test"}}}"#,
                        ),
                        cx,
                    )
                    .expect("local settings are valid");
            });
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| {
            let settings = panel.settings(cx);
            assert_eq!(settings.shell, ShellPreference::Zsh);
            assert_eq!(
                settings.manual_urls.get("mailpit").map(String::as_str),
                Some("https://mail.test")
            );
            assert_eq!(
                SailManagerSettings::get_global(cx).shell,
                ShellPreference::Auto
            );
            assert!(SailManagerSettings::get_global(cx).manual_urls.is_empty());
        });
    }

    #[test]
    fn test_confirmation_matches_the_actions_that_require_it() {
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
        for action in all {
            assert_eq!(
                quick_action_confirmation(action).is_some(),
                action.requires_confirmation(),
                "{action:?}"
            );
        }
        let update = quick_action_confirmation(QuickAction::ComposerUpdate).unwrap();
        assert_eq!(update.message, "Run composer update?");
        assert_eq!(
            update.detail,
            "This updates dependencies and modifies composer.lock."
        );
        let seed = quick_action_confirmation(QuickAction::MigrateSeed).unwrap();
        assert_eq!(seed.message, "Run migrations with seeders?");
        assert_eq!(
            seed.detail,
            "Seeders will write data to the local database."
        );
    }

    #[test]
    fn test_custom_action_confirmation_follows_the_flag() {
        let mut action = CustomAction::validated(
            "Wipe",
            None,
            vec!["ls".to_string()],
            None,
            CustomActionMode::Terminal,
            false,
        )
        .unwrap();
        assert!(custom_action_confirmation(&action).is_none());
        action.confirm = true;
        assert_eq!(
            custom_action_confirmation(&action).unwrap().message,
            "Run Wipe?"
        );
    }

    async fn refresh_mode_panel(
        mode: &str,
        activate: bool,
        cx: &mut TestAppContext,
    ) -> Entity<SailManagerPanel> {
        init_test(cx);
        cx.update(|cx| theme_settings::init(theme::LoadThemes::JustBase, cx));
        let project = sail_project(cx).await;
        let (panel, cx) = cx.add_window_view(|_, cx| {
            new_test_panel(
                WeakEntity::new_invalid(),
                project,
                runner(sail_responder),
                fake_find_docker,
                cx,
            )
        });
        cx.run_until_parked();
        let worktree_id = panel
            .read_with(cx, |panel, _| {
                panel.root.as_ref().map(|(worktree_id, _)| *worktree_id)
            })
            .expect("panel has a root");
        let content = format!(r#"{{"sail_manager": {{"refresh_mode": "{mode}"}}}}"#);
        cx.update(|_, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty().into()),
                        settings::LocalSettingsKind::Settings,
                        Some(&content),
                        cx,
                    )
                    .expect("local settings are valid");
            });
        });
        cx.run_until_parked();
        if activate {
            panel.update_in(cx, |panel, window, cx| panel.set_active(true, window, cx));
            cx.run_until_parked();
        }
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.state, PanelState::Ready { .. }));
        });
        panel
    }

    #[gpui::test]
    async fn test_refresh_mode_events_and_poll_runs_both_tasks(cx: &mut TestAppContext) {
        let panel = refresh_mode_panel("events_and_poll", true, cx).await;
        panel.read_with(cx, |panel, _| {
            assert!(panel.events_task.is_some());
            assert!(panel.poll_task.is_some());
        });
    }

    #[gpui::test]
    async fn test_refresh_mode_events_does_not_poll(cx: &mut TestAppContext) {
        let panel = refresh_mode_panel("events", true, cx).await;
        panel.read_with(cx, |panel, _| {
            assert!(panel.events_task.is_some());
            assert!(panel.poll_task.is_none());
        });
    }

    #[gpui::test]
    async fn test_refresh_mode_manual_stops_events_and_polling(cx: &mut TestAppContext) {
        let panel = refresh_mode_panel("manual", true, cx).await;
        panel.read_with(cx, |panel, _| {
            assert!(panel.events_task.is_none());
            assert!(panel.poll_task.is_none());
        });
    }

    #[gpui::test]
    async fn test_refresh_mode_change_restarts_the_tasks(cx: &mut TestAppContext) {
        let panel = refresh_mode_panel("manual", true, cx).await;
        let worktree_id = panel
            .read_with(cx, |panel, _| {
                panel.root.as_ref().map(|(worktree_id, _)| *worktree_id)
            })
            .expect("panel has a root");
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty().into()),
                        settings::LocalSettingsKind::Settings,
                        Some(r#"{"sail_manager": {"refresh_mode": "events_and_poll"}}"#),
                        cx,
                    )
                    .expect("local settings are valid");
            });
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.events_task.is_some());
            assert!(panel.poll_task.is_some());
        });
    }

    #[gpui::test]
    async fn test_toggle_settings_view(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| theme_settings::init(theme::LoadThemes::JustBase, cx));
        cx.update(editor::init);
        let project = sail_project(cx).await;
        let (panel, cx) = cx.add_window_view(|_, cx| {
            new_test_panel(
                WeakEntity::new_invalid(),
                project,
                runner(sail_responder),
                fake_find_docker,
                cx,
            )
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(!panel.showing_settings);
            assert!(panel.settings_view.is_none());
        });
        panel.update_in(cx, |panel, window, cx| panel.toggle_settings(window, cx));
        panel.read_with(cx, |panel, _| {
            assert!(panel.showing_settings);
            assert!(panel.settings_view.is_some());
        });
        panel.update_in(cx, |panel, window, cx| panel.toggle_settings(window, cx));
        panel.read_with(cx, |panel, _| {
            assert!(!panel.showing_settings);
            assert!(panel.settings_view.is_none());
        });
    }

    fn stopped_sail_responder(args: &[String]) -> CommandOutput {
        if !is_system_ps(args) && has_argument(args, "ps") {
            output(true, "", "")
        } else if is_system_ps(args) {
            output(true, "", "")
        } else {
            sail_responder(args)
        }
    }

    fn is_system_ps(args: &[String]) -> bool {
        has_argument(args, "{{json .}}")
    }

    fn panel_with_ports(
        project: &Entity<Project>,
        command_runner: Arc<dyn CommandRunner>,
        store: Arc<ports::InMemoryAssignmentStore>,
        probe: Arc<ports::FakePortProbe>,
        cx: &mut TestAppContext,
    ) -> Entity<SailManagerPanel> {
        let project = project.clone();
        cx.new(|cx| {
            SailManagerPanel::new_with_ports(
                WeakEntity::new_invalid(),
                project,
                command_runner,
                fake_find_docker,
                store,
                probe,
                cx,
            )
        })
    }

    fn call_environment(
        calls: &RecordedCalls,
        environments: &RecordedEnvironments,
        first_argument: &str,
    ) -> Vec<HashMap<String, String>> {
        let calls = calls.lock().map(|calls| calls.clone()).unwrap_or_default();
        let environments = environments
            .lock()
            .map(|environments| environments.clone())
            .unwrap_or_default();
        calls
            .iter()
            .zip(environments)
            .filter(|(args, _)| args.first().is_some_and(|first| first == first_argument))
            .map(|(_, environment)| environment)
            .collect()
    }

    #[gpui::test]
    async fn test_start_remaps_busy_ports_and_the_panel_uses_the_override(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let (command_runner, calls, environments) =
            environment_recording_runner(stopped_sail_responder);
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let panel = panel_with_ports(&project, command_runner, store.clone(), probe, cx);
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel
                    .client
                    .as_ref()
                    .and_then(|client| client.invocation().generated_override.clone()),
                None
            );
        });

        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();

        let override_path = ports::generated_override_path(Path::new(path!("/work/blog")));
        let written = fs.load(&override_path).await.expect("override is written");
        assert!(written.contains("ports: !override"));
        assert!(written.contains("\"8080:80\""));
        let expected_files = format!("compose.yaml:{}", override_path.to_string_lossy());
        let up_environments = call_environment(&calls, &environments, "up");
        assert_eq!(up_environments.len(), 1);
        assert_eq!(up_environments[0].get("SAIL_FILES"), Some(&expected_files));
        assert_eq!(
            up_environments[0]
                .get("SAIL_SKIP_CHECKS")
                .map(String::as_str),
            Some("1")
        );

        let ps_environments = call_environment(&calls, &environments, "ps");
        assert_eq!(
            ps_environments
                .last()
                .and_then(|environment| environment.get("SAIL_FILES")),
            Some(&expected_files)
        );
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel
                    .client
                    .as_ref()
                    .and_then(|client| client.invocation().generated_override.clone()),
                Some(override_path.clone())
            );
            assert_eq!(panel.port_assignments.len(), 5);
        });
        let registry = store.snapshot();
        assert_eq!(registry.len(), 1);
        assert!(registry.values().next().is_some_and(|assignments| {
            assignments.iter().any(|assignment| {
                assignment.original_host_port == 80 && assignment.host_port == 8080
            })
        }));
    }

    #[gpui::test]
    async fn test_port_rows_show_original_and_assigned_ports(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, _) = call_recording_runner(stopped_sail_responder);
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let panel = panel_with_ports(&project, command_runner, store, probe, cx);
        cx.run_until_parked();
        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();
        let rows = panel.read_with(cx, |panel, cx| panel.port_rows(cx));
        let web = rows
            .iter()
            .find(|row| row.service == "laravel.test")
            .expect("the app row exists");
        assert_eq!(web.key, "laravel.test:80/tcp");
        assert_eq!(web.original_host_port, 80);
        assert!(!web.pinned);
        let mysql = rows
            .iter()
            .find(|row| row.service == "mysql")
            .expect("the database row exists");
        assert_eq!(mysql.original_host_port, 3306);
        assert_eq!(mysql.host_port, 3306);
    }

    #[gpui::test]
    async fn test_old_compose_starts_without_the_override(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let (command_runner, calls, environments) = environment_recording_runner(|args| {
            if has_argument(args, "version") {
                output(true, "2.20.0\n", "")
            } else {
                stopped_sail_responder(args)
            }
        });
        let probe = ports::FakePortProbe::new(&[80]);
        let panel = panel_with_ports(
            &project,
            command_runner,
            ports::InMemoryAssignmentStore::new(),
            probe,
            cx,
        );
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| {
            let context = panel.settings_context(cx);
            assert!(!context.ports_supported);
        });
        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();

        let override_path = ports::generated_override_path(Path::new(path!("/work/blog")));
        assert!(!fs.is_file(&override_path).await);
        let up_environments = call_environment(&calls, &environments, "up");
        assert_eq!(up_environments.len(), 1);
        assert!(!up_environments[0].contains_key("SAIL_FILES"));
    }

    #[gpui::test]
    async fn test_refresh_ignores_the_override_when_compose_does_not_support_it(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let project = sail_project(cx).await;
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let override_path = ports::generated_override_path(Path::new(path!("/work/blog")));
        fs.create_dir(override_path.parent().expect("override has a parent"))
            .await
            .expect("override directory is created");
        fs.write(&override_path, b"services: {}\n")
            .await
            .expect("override is written");
        let (command_runner, calls, environments) = environment_recording_runner(|args| {
            if has_argument(args, "version") {
                output(true, "2.20.0\n", "")
            } else {
                stopped_sail_responder(args)
            }
        });
        let probe = ports::FakePortProbe::new(&[]);
        let panel = panel_with_ports(
            &project,
            command_runner,
            ports::InMemoryAssignmentStore::new(),
            probe,
            cx,
        );
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel
                    .client
                    .as_ref()
                    .map(|client| client.invocation().generated_override.clone()),
                Some(None)
            );
        });
        assert!(
            call_environment(&calls, &environments, "ps")
                .iter()
                .all(|environment| !environment.contains_key("SAIL_FILES"))
        );
    }

    #[gpui::test]
    async fn test_refresh_uses_the_override_when_compose_supports_it(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let override_path = ports::generated_override_path(Path::new(path!("/work/blog")));
        fs.create_dir(override_path.parent().expect("override has a parent"))
            .await
            .expect("override directory is created");
        fs.write(&override_path, b"services: {}\n")
            .await
            .expect("override is written");
        let (command_runner, _) = call_recording_runner(stopped_sail_responder);
        let probe = ports::FakePortProbe::new(&[]);
        let panel = panel_with_ports(
            &project,
            command_runner,
            ports::InMemoryAssignmentStore::new(),
            probe,
            cx,
        );
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel
                    .client
                    .as_ref()
                    .and_then(|client| client.invocation().generated_override.clone()),
                Some(override_path.clone())
            );
        });
    }

    #[gpui::test]
    async fn test_reset_port_assignments_clears_the_registry_and_the_override(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let project = sail_project(cx).await;
        let fs = project.read_with(cx, |project, _| project.fs().clone());
        let (command_runner, _) = call_recording_runner(stopped_sail_responder);
        let store = ports::InMemoryAssignmentStore::new();
        let probe = ports::FakePortProbe::new(&[80]);
        let panel = panel_with_ports(&project, command_runner, store.clone(), probe, cx);
        cx.run_until_parked();
        panel.update(cx, |panel, cx| panel.start_environment(cx));
        cx.run_until_parked();
        let override_path = ports::generated_override_path(Path::new(path!("/work/blog")));
        assert!(fs.is_file(&override_path).await);
        assert!(!store.snapshot().is_empty());

        panel.update(cx, |panel, cx| panel.reset_port_assignments(cx));
        cx.run_until_parked();

        assert!(!fs.is_file(&override_path).await);
        assert!(store.snapshot().is_empty());
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel
                    .client
                    .as_ref()
                    .and_then(|client| client.invocation().generated_override.clone()),
                None
            );
            assert!(panel.port_assignments.is_empty());
        });
    }

    #[gpui::test]
    async fn test_restart_does_not_reassign_ports(cx: &mut TestAppContext) {
        init_test(cx);
        let project = sail_project(cx).await;
        let (command_runner, calls, environments) = environment_recording_runner(sail_responder);
        let probe = ports::FakePortProbe::new(&[3306]);
        let store = ports::InMemoryAssignmentStore::new();
        let panel = panel_with_ports(&project, command_runner, store.clone(), probe, cx);
        cx.run_until_parked();
        panel.update(cx, |panel, cx| panel.restart_environment(cx));
        cx.run_until_parked();
        assert_eq!(mutating_calls(&calls), vec![strings(&["restart"])]);
        assert!(
            call_environment(&calls, &environments, "restart")
                .iter()
                .all(|environment| !environment.contains_key("SAIL_FILES"))
        );
        assert!(store.snapshot().is_empty());
    }
}
