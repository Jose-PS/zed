use anyhow::{Context as _, Result, anyhow};
use gpui::{App, Entity, Task};
use project::{Project, ProjectPath};
use settings::{
    CustomActionContent, CustomActionModeContent, NodePackageManagerContent, RefreshModeContent,
    SettingsContent, SettingsStore, ShellPreferenceContent,
};
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::compose::{
    CustomAction, CustomActionMode, NodePackageManager, RefreshMode, ShellPreference,
};

pub fn update_project_settings_file(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    update: impl FnOnce(&mut SettingsContent) + 'static,
    cx: &mut App,
) -> Task<Result<()>> {
    cx.spawn(async move |cx| {
        let rel_path: std::sync::Arc<RelPath> = paths::local_settings_file_relative_path().into();
        let worktree = project
            .read_with(cx, |project, cx| project.worktree_for_id(worktree_id, cx))
            .ok_or_else(|| anyhow!("the worktree no longer exists"))?;

        let needs_creation = worktree.read_with(cx, |worktree, _| {
            worktree.entry_for_path(&rel_path).is_none()
        });
        if needs_creation {
            worktree
                .update(cx, |worktree, cx| {
                    worktree.create_entry(rel_path.clone(), false, None, cx)
                })
                .await
                .context("Failed to create the project settings file")?;
        }

        let project_path = ProjectPath {
            worktree_id,
            path: rel_path,
        };
        let buffer_store = project.read_with(cx, |project, _| project.buffer_store().clone());
        let buffer = buffer_store
            .update(cx, |store, cx| store.open_buffer(project_path, cx))
            .await
            .context("Failed to open the project settings file")?;

        let old_text = buffer.read_with(cx, |buffer, _| buffer.text());
        let new_text = cx.read_global(|store: &SettingsStore, _| {
            store.new_text_for_update(old_text.clone(), update)
        })?;
        if new_text != old_text {
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..buffer.len(), new_text)], None, cx);
            });
        }

        buffer_store
            .update(cx, |store, cx| store.save_buffer(buffer, cx))
            .await
            .context("Failed to save the project settings file")?;
        Ok(())
    })
}

pub fn set_manual_url(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    service: String,
    url: Option<String>,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            let manual_urls = content
                .sail_manager
                .get_or_insert_default()
                .project
                .manual_urls
                .get_or_insert_default();
            match url {
                Some(url) => {
                    manual_urls.insert(service, url);
                }
                None => {
                    manual_urls.remove(&service);
                }
            }
        },
        cx,
    )
}

pub fn set_app_service(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    app_service: String,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content
                .sail_manager
                .get_or_insert_default()
                .project
                .app_service = Some(app_service);
        },
        cx,
    )
}

pub fn set_url_host(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    url_host: String,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content
                .sail_manager
                .get_or_insert_default()
                .project
                .url_host = Some(url_host);
        },
        cx,
    )
}

pub fn set_shell(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    shell: ShellPreference,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content.sail_manager.get_or_insert_default().project.shell = Some(match shell {
                ShellPreference::Auto => ShellPreferenceContent::Auto,
                ShellPreference::Zsh => ShellPreferenceContent::Zsh,
                ShellPreference::Bash => ShellPreferenceContent::Bash,
                ShellPreference::Sh => ShellPreferenceContent::Sh,
            });
        },
        cx,
    )
}

pub fn set_compose_files(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    compose_files: Vec<String>,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            let project_content = &mut content.sail_manager.get_or_insert_default().project;
            project_content.compose_files = if compose_files.is_empty() {
                None
            } else {
                Some(compose_files)
            };
        },
        cx,
    )
}

pub fn set_refresh_mode(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    refresh_mode: RefreshMode,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content
                .sail_manager
                .get_or_insert_default()
                .project
                .refresh_mode = Some(match refresh_mode {
                RefreshMode::EventsAndPoll => RefreshModeContent::EventsAndPoll,
                RefreshMode::Events => RefreshModeContent::Events,
                RefreshMode::Manual => RefreshModeContent::Manual,
            });
        },
        cx,
    )
}

pub fn set_auto_assign_ports(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    enabled: bool,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content
                .sail_manager
                .get_or_insert_default()
                .project
                .auto_assign_ports = Some(enabled);
        },
        cx,
    )
}

pub fn set_port_pin(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    key: String,
    port: Option<u16>,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            let project_content = &mut content.sail_manager.get_or_insert_default().project;
            let pins = project_content.port_pins.get_or_insert_default();
            match port {
                Some(port) => {
                    pins.insert(key, port);
                }
                None => {
                    pins.remove(&key);
                }
            }
            if pins.is_empty() {
                project_content.port_pins = None;
            }
        },
        cx,
    )
}

pub fn set_node_package_manager(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    package_manager: Option<NodePackageManager>,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content
                .sail_manager
                .get_or_insert_default()
                .project
                .node_package_manager = Some(match package_manager {
                None => NodePackageManagerContent::Auto,
                Some(NodePackageManager::Npm) => NodePackageManagerContent::Npm,
                Some(NodePackageManager::Pnpm) => NodePackageManagerContent::Pnpm,
                Some(NodePackageManager::Yarn) => NodePackageManagerContent::Yarn,
                Some(NodePackageManager::Bun) => NodePackageManagerContent::Bun,
            });
        },
        cx,
    )
}

pub fn custom_action_content(action: &CustomAction) -> CustomActionContent {
    CustomActionContent {
        name: action.name.clone(),
        service: action.service.clone(),
        command: action.command.clone(),
        working_directory: action.working_directory.clone(),
        mode: Some(match action.mode {
            CustomActionMode::Terminal => CustomActionModeContent::Terminal,
            CustomActionMode::Background => CustomActionModeContent::Background,
        }),
        confirm: action.confirm.then_some(true),
    }
}

pub fn add_custom_action(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    action: CustomAction,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            content
                .sail_manager
                .get_or_insert_default()
                .project
                .actions
                .get_or_insert_default()
                .push(custom_action_content(&action));
        },
        cx,
    )
}

pub fn remove_custom_action(
    project: Entity<Project>,
    worktree_id: WorktreeId,
    name: String,
    cx: &mut App,
) -> Task<Result<()>> {
    update_project_settings_file(
        project,
        worktree_id,
        move |content| {
            let actions = content
                .sail_manager
                .get_or_insert_default()
                .project
                .actions
                .get_or_insert_default();
            if let Some(position) = actions.iter().position(|action| action.name == name) {
                actions.remove(position);
            }
        },
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sail_manager_settings::SailManagerSettings;
    use fs::{FakeFs, Fs as _};
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::{Settings, SettingsLocation};
    use util::path;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn manual_urls_at(
        worktree_id: WorktreeId,
        cx: &TestAppContext,
    ) -> collections::HashMap<String, String> {
        cx.read(|cx| {
            SailManagerSettings::get(
                Some(SettingsLocation {
                    worktree_id,
                    path: RelPath::empty(),
                }),
                cx,
            )
            .manual_urls
            .clone()
        })
    }

    #[gpui::test]
    async fn test_set_manual_url_keeps_the_rest_of_the_file(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                ".zed": {
                    "settings.json": r#"{
  "tab_size": 2,
  "sail_manager": {
    "app_service": "app",
    "manual_urls": { "mailpit": "http://mail.test" }
  }
}"#
                },
                "compose.yaml": "services: {}",
            }),
        )
        .await;
        let project = project::Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
                .expect("the project has a worktree")
        });
        assert_eq!(
            manual_urls_at(worktree_id, cx)
                .get("mailpit")
                .map(String::as_str),
            Some("http://mail.test")
        );

        cx.update(|cx| {
            set_manual_url(
                project.clone(),
                worktree_id,
                "laravel.test".to_string(),
                Some("https://blog.test:8443".to_string()),
                cx,
            )
        })
        .await
        .expect("the url is saved");
        cx.run_until_parked();

        let saved = fs
            .load(path!("/work/blog/.zed/settings.json").as_ref())
            .await
            .expect("the settings file exists");
        let saved: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
        assert_eq!(saved["tab_size"], 2);
        assert_eq!(saved["sail_manager"]["app_service"], "app");
        assert_eq!(
            saved["sail_manager"]["manual_urls"],
            json!({
                "mailpit": "http://mail.test",
                "laravel.test": "https://blog.test:8443"
            })
        );

        let urls = manual_urls_at(worktree_id, cx);
        assert_eq!(
            urls.get("laravel.test").map(String::as_str),
            Some("https://blog.test:8443")
        );
        assert_eq!(
            urls.get("mailpit").map(String::as_str),
            Some("http://mail.test")
        );

        cx.update(|cx| {
            set_manual_url(
                project.clone(),
                worktree_id,
                "mailpit".to_string(),
                None,
                cx,
            )
        })
        .await
        .expect("the url is removed");
        cx.run_until_parked();
        let urls = manual_urls_at(worktree_id, cx);
        assert!(!urls.contains_key("mailpit"));
        assert!(urls.contains_key("laravel.test"));
    }

    #[gpui::test]
    async fn test_set_manual_url_creates_the_settings_file(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/work/blog"), json!({"compose.yaml": "services: {}"}))
            .await;
        let project = project::Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
                .expect("the project has a worktree")
        });

        cx.update(|cx| {
            set_manual_url(
                project.clone(),
                worktree_id,
                "mailpit".to_string(),
                Some("http://mail.test".to_string()),
                cx,
            )
        })
        .await
        .expect("the url is saved");
        cx.run_until_parked();

        let saved = fs
            .load(path!("/work/blog/.zed/settings.json").as_ref())
            .await
            .expect("the settings file is created");
        let saved: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
        assert_eq!(
            saved["sail_manager"]["manual_urls"]["mailpit"],
            "http://mail.test"
        );
        assert_eq!(
            manual_urls_at(worktree_id, cx)
                .get("mailpit")
                .map(String::as_str),
            Some("http://mail.test")
        );
    }

    fn first_worktree_id(project: &Entity<Project>, cx: &mut TestAppContext) -> WorktreeId {
        project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
                .expect("the project has a worktree")
        })
    }

    fn settings_at(worktree_id: WorktreeId, cx: &TestAppContext) -> SailManagerSettings {
        cx.read(|cx| {
            SailManagerSettings::get(
                Some(SettingsLocation {
                    worktree_id,
                    path: RelPath::empty(),
                }),
                cx,
            )
            .clone()
        })
    }

    fn seed_action() -> CustomAction {
        CustomAction::validated(
            "Seed",
            Some("laravel.test"),
            vec!["php".into(), "artisan".into(), "db:seed".into()],
            Some("/var/www/html"),
            CustomActionMode::Background,
            false,
        )
        .expect("valid action")
    }

    #[gpui::test]
    async fn test_add_and_remove_custom_action_keeps_the_rest_of_the_file(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                ".zed": {
                    "settings.json": r#"{
  "tab_size": 2,
  "sail_manager": {
    "app_service": "app",
    "actions": [
      { "name": "Existing", "command": ["ls"] }
    ]
  }
}"#
                },
                "compose.yaml": "services: {}",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = first_worktree_id(&project, cx);

        cx.update(|cx| add_custom_action(project.clone(), worktree_id, seed_action(), cx))
            .await
            .expect("the action is saved");
        cx.run_until_parked();

        let saved = fs
            .load(path!("/work/blog/.zed/settings.json").as_ref())
            .await
            .expect("the settings file exists");
        let saved: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
        assert_eq!(saved["tab_size"], 2);
        assert_eq!(saved["sail_manager"]["app_service"], "app");
        assert_eq!(
            saved["sail_manager"]["actions"],
            json!([
                { "name": "Existing", "command": ["ls"] },
                {
                    "name": "Seed",
                    "service": "laravel.test",
                    "command": ["php", "artisan", "db:seed"],
                    "working_directory": "/var/www/html",
                    "mode": "background"
                }
            ])
        );
        let settings = settings_at(worktree_id, cx);
        assert_eq!(settings.actions.len(), 2);
        assert_eq!(settings.actions[1], seed_action());

        cx.update(|cx| {
            remove_custom_action(project.clone(), worktree_id, "Existing".to_string(), cx)
        })
        .await
        .expect("the action is removed");
        cx.run_until_parked();
        let settings = settings_at(worktree_id, cx);
        assert_eq!(settings.actions, vec![seed_action()]);
        let saved = fs
            .load(path!("/work/blog/.zed/settings.json").as_ref())
            .await
            .expect("the settings file exists");
        let saved: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
        assert_eq!(saved["tab_size"], 2);
        assert_eq!(saved["sail_manager"]["app_service"], "app");
    }

    #[gpui::test]
    async fn test_invalid_custom_actions_are_ignored_when_reading(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                ".zed": {
                    "settings.json": r#"{
  "sail_manager": {
    "actions": [
      { "name": "", "command": ["ls"] },
      { "name": "No command", "command": [] },
      { "name": "Missing command" },
      { "name": "Bad service", "service": "bad name", "command": ["ls"] },
      { "name": "Good", "service": "mysql", "command": ["ls", "-la"], "confirm": true },
      { "name": "Plain", "command": ["pwd"], "mode": "terminal" }
    ]
  }
}"#
                },
                "compose.yaml": "services: {}",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = first_worktree_id(&project, cx);
        let settings = settings_at(worktree_id, cx);
        let names: Vec<&str> = settings
            .actions
            .iter()
            .map(|action| action.name.as_str())
            .collect();
        assert_eq!(names, ["Good", "Plain"]);
        assert_eq!(settings.actions[0].service.as_deref(), Some("mysql"));
        assert!(settings.actions[0].confirm);
        assert_eq!(settings.actions[1].mode, CustomActionMode::Terminal);
        assert!(!settings.actions[1].confirm);
        assert_eq!(settings.node_package_manager, None);
    }

    #[gpui::test]
    async fn test_set_node_package_manager(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/work/blog"), json!({"compose.yaml": "services: {}"}))
            .await;
        let project = Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = first_worktree_id(&project, cx);
        assert_eq!(settings_at(worktree_id, cx).node_package_manager, None);

        cx.update(|cx| {
            set_node_package_manager(
                project.clone(),
                worktree_id,
                Some(NodePackageManager::Pnpm),
                cx,
            )
        })
        .await
        .expect("the package manager is saved");
        cx.run_until_parked();
        assert_eq!(
            settings_at(worktree_id, cx).node_package_manager,
            Some(NodePackageManager::Pnpm)
        );

        cx.update(|cx| set_node_package_manager(project.clone(), worktree_id, None, cx))
            .await
            .expect("the package manager is reset");
        cx.run_until_parked();
        assert_eq!(settings_at(worktree_id, cx).node_package_manager, None);
    }

    async fn saved_json(fs: &FakeFs) -> serde_json::Value {
        let saved = fs
            .load(path!("/work/blog/.zed/settings.json").as_ref())
            .await
            .expect("the settings file exists");
        serde_json::from_str(&saved).expect("valid json")
    }

    #[gpui::test]
    async fn test_scalar_setters_keep_the_rest_of_the_file(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                ".zed": {
                    "settings.json": r#"{
  "tab_size": 2,
  "sail_manager": {
    "manual_urls": { "mailpit": "http://mail.test" },
    "actions": [{ "name": "Existing", "command": ["ls"] }]
  }
}"#
                },
                "compose.yaml": "services: {}",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = first_worktree_id(&project, cx);

        cx.update(|cx| set_app_service(project.clone(), worktree_id, "app".to_string(), cx))
            .await
            .expect("the service is saved");
        cx.update(|cx| set_url_host(project.clone(), worktree_id, "blog.test".to_string(), cx))
            .await
            .expect("the host is saved");
        cx.update(|cx| set_shell(project.clone(), worktree_id, ShellPreference::Zsh, cx))
            .await
            .expect("the shell is saved");
        cx.update(|cx| set_refresh_mode(project.clone(), worktree_id, RefreshMode::Manual, cx))
            .await
            .expect("the refresh mode is saved");
        cx.run_until_parked();

        let saved = saved_json(&fs).await;
        assert_eq!(saved["tab_size"], 2);
        assert_eq!(saved["sail_manager"]["app_service"], "app");
        assert_eq!(saved["sail_manager"]["url_host"], "blog.test");
        assert_eq!(saved["sail_manager"]["shell"], "zsh");
        assert_eq!(saved["sail_manager"]["refresh_mode"], "manual");
        assert_eq!(
            saved["sail_manager"]["manual_urls"],
            json!({ "mailpit": "http://mail.test" })
        );
        assert_eq!(
            saved["sail_manager"]["actions"],
            json!([{ "name": "Existing", "command": ["ls"] }])
        );

        let settings = settings_at(worktree_id, cx);
        assert_eq!(settings.app_service, "app");
        assert_eq!(settings.url_host, "blog.test");
        assert_eq!(settings.shell, ShellPreference::Zsh);
        assert_eq!(settings.refresh_mode, RefreshMode::Manual);
        assert_eq!(settings.actions.len(), 1);
    }

    #[gpui::test]
    async fn test_set_compose_files_replaces_and_clears_the_list(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                ".zed": {
                    "settings.json": r#"{
  "tab_size": 2,
  "sail_manager": {
    "app_service": "app",
    "compose_files": ["compose.yaml"]
  }
}"#
                },
                "compose.yaml": "services: {}",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = first_worktree_id(&project, cx);

        cx.update(|cx| {
            set_compose_files(
                project.clone(),
                worktree_id,
                vec![
                    "compose.yaml".to_string(),
                    "docker/override.yaml".to_string(),
                ],
                cx,
            )
        })
        .await
        .expect("the files are saved");
        cx.run_until_parked();
        let saved = saved_json(&fs).await;
        assert_eq!(
            saved["sail_manager"]["compose_files"],
            json!(["compose.yaml", "docker/override.yaml"])
        );
        assert_eq!(
            settings_at(worktree_id, cx).compose_files,
            vec![
                "compose.yaml".to_string(),
                "docker/override.yaml".to_string()
            ]
        );

        cx.update(|cx| set_compose_files(project.clone(), worktree_id, Vec::new(), cx))
            .await
            .expect("the list is cleared");
        cx.run_until_parked();
        let saved = saved_json(&fs).await;
        assert!(saved["sail_manager"].get("compose_files").is_none());
        assert_eq!(saved["sail_manager"]["app_service"], "app");
        assert_eq!(saved["tab_size"], 2);
        assert!(settings_at(worktree_id, cx).compose_files.is_empty());
    }

    #[gpui::test]
    async fn test_set_port_settings_keep_the_rest_of_the_file(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work/blog"),
            json!({
                ".zed": {
                    "settings.json": r#"{
  "tab_size": 2,
  "sail_manager": {
    "app_service": "app"
  }
}"#
                },
                "compose.yaml": "services: {}",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/work/blog").as_ref()], cx).await;
        cx.run_until_parked();
        let worktree_id = first_worktree_id(&project, cx);
        assert!(settings_at(worktree_id, cx).auto_assign_ports);
        assert!(settings_at(worktree_id, cx).port_pins.is_empty());

        cx.update(|cx| set_auto_assign_ports(project.clone(), worktree_id, false, cx))
            .await
            .expect("the flag is saved");
        cx.update(|cx| {
            set_port_pin(
                project.clone(),
                worktree_id,
                "laravel.test:80/tcp".to_string(),
                Some(8085),
                cx,
            )
        })
        .await
        .expect("the pin is saved");
        cx.run_until_parked();
        let saved = saved_json(&fs).await;
        assert_eq!(saved["tab_size"], 2);
        assert_eq!(saved["sail_manager"]["app_service"], "app");
        assert_eq!(saved["sail_manager"]["auto_assign_ports"], false);
        assert_eq!(
            saved["sail_manager"]["port_pins"],
            json!({"laravel.test:80/tcp": 8085})
        );
        let settings = settings_at(worktree_id, cx);
        assert!(!settings.auto_assign_ports);
        assert_eq!(settings.port_pins.get("laravel.test:80/tcp"), Some(&8085));

        cx.update(|cx| {
            set_port_pin(
                project.clone(),
                worktree_id,
                "laravel.test:80/tcp".to_string(),
                None,
                cx,
            )
        })
        .await
        .expect("the pin is removed");
        cx.run_until_parked();
        let saved = saved_json(&fs).await;
        assert!(saved["sail_manager"].get("port_pins").is_none());
        assert_eq!(saved["sail_manager"]["app_service"], "app");
        assert!(settings_at(worktree_id, cx).port_pins.is_empty());
    }
}
