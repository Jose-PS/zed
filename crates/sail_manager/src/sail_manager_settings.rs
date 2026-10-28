use crate::compose::{
    CustomAction, CustomActionMode, NodePackageManager, RefreshMode, ShellPreference,
};
use collections::HashMap;
use gpui::Pixels;
pub use settings::{DockSide, Settings};
use settings::{IntoGpui, RegisterSetting, merge_from::MergeFrom};

#[derive(Debug, Clone, PartialEq, RegisterSetting)]
pub struct SailManagerSettings {
    pub button: bool,
    pub dock: DockSide,
    pub default_width: Pixels,
    pub app_service: String,
    pub url_host: String,
    pub compose_files: Vec<String>,
    pub shell: ShellPreference,
    pub manual_urls: HashMap<String, String>,
    pub node_package_manager: Option<NodePackageManager>,
    pub actions: Vec<CustomAction>,
    pub refresh_mode: RefreshMode,
    pub auto_assign_ports: bool,
    pub port_pins: HashMap<String, u16>,
}

impl Settings for SailManagerSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let panel = content.sail_manager.as_ref().unwrap();
        let mut project = panel.project.clone();
        project.merge_from_option(content.project.sail_manager.as_ref());
        Self {
            button: panel.button.unwrap(),
            dock: panel.dock.unwrap(),
            default_width: panel.default_width.unwrap().into_gpui(),
            app_service: project.app_service.unwrap(),
            url_host: project.url_host.unwrap(),
            compose_files: project.compose_files.unwrap(),
            shell: match project.shell.unwrap() {
                settings::ShellPreferenceContent::Auto => ShellPreference::Auto,
                settings::ShellPreferenceContent::Zsh => ShellPreference::Zsh,
                settings::ShellPreferenceContent::Bash => ShellPreference::Bash,
                settings::ShellPreferenceContent::Sh => ShellPreference::Sh,
            },
            manual_urls: project.manual_urls.unwrap(),
            node_package_manager: match project.node_package_manager.unwrap() {
                settings::NodePackageManagerContent::Auto => None,
                settings::NodePackageManagerContent::Npm => Some(NodePackageManager::Npm),
                settings::NodePackageManagerContent::Pnpm => Some(NodePackageManager::Pnpm),
                settings::NodePackageManagerContent::Yarn => Some(NodePackageManager::Yarn),
                settings::NodePackageManagerContent::Bun => Some(NodePackageManager::Bun),
            },
            actions: project
                .actions
                .unwrap()
                .into_iter()
                .filter_map(custom_action_from_content)
                .collect(),
            refresh_mode: match project.refresh_mode.unwrap() {
                settings::RefreshModeContent::EventsAndPoll => RefreshMode::EventsAndPoll,
                settings::RefreshModeContent::Events => RefreshMode::Events,
                settings::RefreshModeContent::Manual => RefreshMode::Manual,
            },
            auto_assign_ports: project.auto_assign_ports.unwrap(),
            port_pins: project.port_pins.unwrap(),
        }
    }
}

fn custom_action_from_content(content: settings::CustomActionContent) -> Option<CustomAction> {
    let mode = match content.mode {
        Some(settings::CustomActionModeContent::Background) => CustomActionMode::Background,
        Some(settings::CustomActionModeContent::Terminal) | None => CustomActionMode::Terminal,
    };
    CustomAction::validated(
        &content.name,
        content.service.as_deref(),
        content.command,
        content.working_directory.as_deref(),
        mode,
        content.confirm.unwrap_or(false),
    )
    .ok()
}
