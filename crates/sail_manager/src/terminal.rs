use std::collections::HashMap;

use anyhow::{Result, anyhow};
use task::{HideStrategy, RevealStrategy, RevealTarget, ShellKind, SpawnInTerminal, TaskId};

use crate::compose::{
    self, ComposeCommand, ComposeInvocation, CustomAction, CustomActionMode, NodePackageManager,
    QuickAction, Service, TerminalTarget,
};

const LOGS_TAIL_LINES: u32 = 200;

pub fn logs_spawn(
    invocation: &ComposeInvocation,
    project_name: &str,
    service: &str,
    project_environment: &HashMap<String, String>,
    shell_kind: ShellKind,
) -> Result<SpawnInTerminal> {
    let command = invocation.logs_command(service, true, LOGS_TAIL_LINES)?;
    terminal_spawn(
        TaskId(format!("sail-manager-logs-{project_name}-{service}")),
        format!("{project_name} · logs · {service}"),
        command,
        invocation,
        project_environment,
        shell_kind,
        true,
        RevealStrategy::Always,
    )
}

fn terminal_suffix(target: &TerminalTarget) -> &'static str {
    match target {
        TerminalTarget::Shell { root: true, .. } => " · root",
        TerminalTarget::Shell { root: false, .. } => "",
        TerminalTarget::Client => " · client",
    }
}

pub fn service_terminal_label(
    project_name: &str,
    service_name: &str,
    target: &TerminalTarget,
) -> String {
    format!("{project_name} · {service_name}{}", terminal_suffix(target))
}

pub fn service_terminal_spawn(
    invocation: &ComposeInvocation,
    project_name: &str,
    service: &Service,
    is_sail: bool,
    app_service: &str,
    target: &TerminalTarget,
    project_environment: &HashMap<String, String>,
    shell_kind: ShellKind,
) -> Result<SpawnInTerminal> {
    let command = compose::terminal_command(invocation, service, is_sail, app_service, target)?;
    let label = service_terminal_label(project_name, &service.name, target);
    let suffix = terminal_suffix(target);
    terminal_spawn(
        TaskId(format!(
            "sail-manager-terminal-{project_name}-{}{}",
            service.name,
            suffix.replace(" · ", "-")
        )),
        label,
        command,
        invocation,
        project_environment,
        shell_kind,
        false,
        RevealStrategy::Always,
    )
}

pub fn quick_action_label(
    project_name: &str,
    action: QuickAction,
    package_manager: NodePackageManager,
) -> String {
    format!("{project_name} · {}", action.label(package_manager))
}

pub fn composer_custom_label(project_name: &str, composer_arguments: &[String]) -> String {
    format!("{project_name} · composer {}", composer_arguments.join(" "))
}

pub fn custom_action_label(project_name: &str, action: &CustomAction) -> String {
    format!("{project_name} · {}", action.name)
}

pub fn quick_action_spawn(
    invocation: &ComposeInvocation,
    project_name: &str,
    action: QuickAction,
    is_sail: bool,
    app_service: &str,
    package_manager: NodePackageManager,
    project_environment: &HashMap<String, String>,
    shell_kind: ShellKind,
) -> Result<SpawnInTerminal> {
    let command =
        compose::quick_action_command(invocation, is_sail, app_service, action, package_manager)?;
    let label = quick_action_label(project_name, action, package_manager);
    terminal_spawn(
        TaskId(format!("sail-manager-action-{label}")),
        label,
        command,
        invocation,
        project_environment,
        shell_kind,
        true,
        RevealStrategy::Always,
    )
}

pub fn composer_custom_spawn(
    invocation: &ComposeInvocation,
    project_name: &str,
    composer_arguments: &[String],
    is_sail: bool,
    app_service: &str,
    project_environment: &HashMap<String, String>,
    shell_kind: ShellKind,
) -> Result<SpawnInTerminal> {
    let command =
        compose::composer_custom_command(invocation, is_sail, app_service, composer_arguments)?;
    let label = composer_custom_label(project_name, composer_arguments);
    terminal_spawn(
        TaskId(format!("sail-manager-action-{label}")),
        label,
        command,
        invocation,
        project_environment,
        shell_kind,
        true,
        RevealStrategy::Always,
    )
}

pub fn custom_action_spawn(
    invocation: &ComposeInvocation,
    project_name: &str,
    action: &CustomAction,
    is_sail: bool,
    app_service: &str,
    project_environment: &HashMap<String, String>,
    shell_kind: ShellKind,
) -> Result<SpawnInTerminal> {
    let command = compose::custom_action_command(invocation, is_sail, app_service, action)?;
    let label = custom_action_label(project_name, action);
    let reveal = match action.mode {
        CustomActionMode::Terminal => RevealStrategy::Always,
        CustomActionMode::Background => RevealStrategy::NoFocus,
    };
    terminal_spawn(
        TaskId(format!("sail-manager-action-{label}")),
        label,
        command,
        invocation,
        project_environment,
        shell_kind,
        true,
        reveal,
    )
}

fn terminal_spawn(
    id: TaskId,
    label: String,
    command: ComposeCommand,
    invocation: &ComposeInvocation,
    project_environment: &HashMap<String, String>,
    shell_kind: ShellKind,
    show_command: bool,
    reveal: RevealStrategy,
) -> Result<SpawnInTerminal> {
    let program = command.program.to_string_lossy().into_owned();
    let quoted_program = shell_kind
        .try_quote_prefix_aware(&program)
        .ok_or_else(|| anyhow!("the command {program:?} cannot be quoted for the shell"))?
        .into_owned();
    let mut args = Vec::with_capacity(command.args.len());
    for argument in &command.args {
        let quoted = shell_kind
            .try_quote(argument)
            .ok_or_else(|| anyhow!("the argument {argument:?} cannot be quoted for the shell"))?;
        args.push(quoted.into_owned());
    }
    let command_label = std::iter::once(quoted_program.as_str())
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let mut environment = project_environment.clone();
    environment.extend(command.environment);

    Ok(SpawnInTerminal {
        id,
        full_label: label.clone(),
        label,
        command: Some(quoted_program),
        args,
        command_label,
        cwd: Some(invocation.project_directory.clone()),
        env: environment.into_iter().collect(),
        use_new_terminal: false,
        allow_concurrent_runs: true,
        reveal,
        reveal_target: RevealTarget::Dock,
        hide: HideStrategy::Never,
        show_summary: false,
        show_command,
        show_rerun: false,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{ComposeLauncher, ServiceKind, ServiceState};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn sail_invocation(root: &str) -> ComposeInvocation {
        ComposeInvocation {
            launcher: ComposeLauncher::Sail {
                sail_script: PathBuf::from(root).join("vendor/bin/sail"),
            },
            project_directory: PathBuf::from(root),
            files: vec!["compose.yaml".to_string()],
            generated_override: None,
        }
    }

    fn project_environment() -> HashMap<String, String> {
        HashMap::from_iter([("PATH".to_string(), "/usr/bin".to_string())])
    }

    #[test]
    fn test_logs_spawn_for_sail() {
        let spawn = logs_spawn(
            &sail_invocation("/work/blog"),
            "blog",
            "laravel.test",
            &project_environment(),
            ShellKind::Posix,
        )
        .expect("spawn is built");

        assert_eq!(
            spawn.id,
            TaskId("sail-manager-logs-blog-laravel.test".into())
        );
        assert_eq!(spawn.full_label, "blog · logs · laravel.test");
        assert_eq!(spawn.label, spawn.full_label);
        assert_eq!(spawn.command.as_deref(), Some("/work/blog/vendor/bin/sail"));
        assert_eq!(
            spawn.args,
            ["logs", "--tail", "200", "-f", "laravel.test"].map(String::from)
        );
        assert_eq!(spawn.cwd, Some(PathBuf::from("/work/blog")));
        assert_eq!(
            spawn.env.get("SAIL_SKIP_CHECKS").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            spawn.env.get("SAIL_FILES").map(String::as_str),
            Some("compose.yaml")
        );
        assert_eq!(spawn.env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(spawn.allow_concurrent_runs);
        assert!(!spawn.use_new_terminal);
        assert_eq!(spawn.reveal, RevealStrategy::Always);
        assert_eq!(spawn.reveal_target, RevealTarget::Dock);
        assert_eq!(spawn.hide, HideStrategy::Never);
        assert!(!spawn.show_summary);
        assert!(spawn.show_command);
        assert!(!spawn.show_rerun);
    }

    #[test]
    fn test_logs_spawn_label_is_stable() {
        let first = logs_spawn(
            &sail_invocation("/work/blog"),
            "blog",
            "mysql",
            &HashMap::default(),
            ShellKind::Posix,
        )
        .expect("spawn is built");
        let second = logs_spawn(
            &sail_invocation("/work/blog"),
            "blog",
            "mysql",
            &project_environment(),
            ShellKind::Posix,
        )
        .expect("spawn is built");
        assert_eq!(first.full_label, second.full_label);
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn test_logs_spawn_quotes_a_command_with_spaces() {
        let spawn = logs_spawn(
            &sail_invocation("/work/my blog"),
            "blog",
            "redis",
            &HashMap::default(),
            ShellKind::Posix,
        )
        .expect("spawn is built");
        assert_eq!(
            spawn.command.as_deref(),
            Some("'/work/my blog/vendor/bin/sail'")
        );
        assert_eq!(spawn.cwd, Some(PathBuf::from("/work/my blog")));
    }

    #[test]
    fn test_logs_spawn_for_docker_compose() {
        let invocation = ComposeInvocation {
            launcher: ComposeLauncher::DockerCompose {
                docker_binary: PathBuf::from("/usr/bin/docker"),
            },
            project_directory: PathBuf::from("/work/my blog"),
            files: vec!["compose.yaml".to_string()],
            generated_override: None,
        };
        let spawn = logs_spawn(
            &invocation,
            "blog",
            "redis",
            &HashMap::default(),
            ShellKind::Posix,
        )
        .expect("spawn is built");
        assert_eq!(spawn.command.as_deref(), Some("/usr/bin/docker"));
        assert_eq!(
            spawn.args,
            [
                "compose",
                "--project-directory",
                "'/work/my blog'",
                "-f",
                "compose.yaml",
                "logs",
                "--tail",
                "200",
                "-f",
                "redis"
            ]
            .map(String::from)
        );
        assert!(!spawn.env.contains_key("SAIL_SKIP_CHECKS"));
    }

    #[test]
    fn test_logs_spawn_rejects_invalid_service() {
        for service in ["", "a b", "x;rm -rf /", "$(id)", "-f"] {
            assert!(
                logs_spawn(
                    &sail_invocation("/work/blog"),
                    "blog",
                    service,
                    &HashMap::default(),
                    ShellKind::Posix,
                )
                .is_err(),
                "service {service:?} must be rejected"
            );
        }
    }

    fn named_service(name: &str, kind: ServiceKind) -> Service {
        Service {
            name: name.to_string(),
            image: None,
            kind,
            state: ServiceState::Running { health: None },
            status_text: None,
            container_name: None,
            ports: Vec::new(),
        }
    }

    fn terminal(
        invocation: &ComposeInvocation,
        service: &Service,
        is_sail: bool,
        target: TerminalTarget,
    ) -> SpawnInTerminal {
        service_terminal_spawn(
            invocation,
            "blog",
            service,
            is_sail,
            "laravel.test",
            &target,
            &project_environment(),
            ShellKind::Posix,
        )
        .expect("spawn is built")
    }

    #[test]
    fn test_service_terminal_labels_and_flags() {
        let invocation = sail_invocation("/work/my blog");
        let app = named_service("laravel.test", ServiceKind::App);
        let shell = terminal(
            &invocation,
            &app,
            true,
            TerminalTarget::Shell {
                shell: "bash".into(),
                root: false,
            },
        );
        assert_eq!(shell.full_label, "blog · laravel.test");
        assert_eq!(shell.label, shell.full_label);
        assert_eq!(
            shell.command.as_deref(),
            Some("'/work/my blog/vendor/bin/sail'")
        );
        assert_eq!(
            shell.args,
            ["exec", "-u", "sail", "laravel.test", "bash"].map(String::from)
        );
        assert_eq!(shell.cwd, Some(PathBuf::from("/work/my blog")));
        assert_eq!(
            shell.env.get("SAIL_SKIP_CHECKS").map(String::as_str),
            Some("1")
        );
        assert!(shell.allow_concurrent_runs);
        assert!(!shell.use_new_terminal);
        assert_eq!(shell.reveal, RevealStrategy::Always);
        assert_eq!(shell.reveal_target, RevealTarget::Dock);
        assert_eq!(shell.hide, HideStrategy::Never);
        assert!(!shell.show_summary);
        assert!(!shell.show_command);
        assert!(!shell.show_rerun);

        let root = terminal(
            &invocation,
            &app,
            true,
            TerminalTarget::Shell {
                shell: "bash".into(),
                root: true,
            },
        );
        assert_eq!(root.full_label, "blog · laravel.test · root");
        assert_eq!(
            root.args,
            ["exec", "-u", "root", "laravel.test", "bash"].map(String::from)
        );

        let mysql = named_service("mysql", ServiceKind::Mysql);
        let client = terminal(&invocation, &mysql, true, TerminalTarget::Client);
        assert_eq!(client.full_label, "blog · mysql · client");
        assert_eq!(client.args, ["mysql"].map(String::from));

        let labels = [&shell, &root, &client].map(|spawn| spawn.id.clone());
        assert_eq!(
            labels.iter().collect::<HashSet<_>>().len(),
            3,
            "ids must be distinct"
        );
    }

    #[test]
    fn test_service_terminal_quotes_arguments_without_sail() {
        let invocation = ComposeInvocation {
            launcher: ComposeLauncher::DockerCompose {
                docker_binary: PathBuf::from("/usr/bin/docker"),
            },
            project_directory: PathBuf::from("/work/my blog"),
            files: vec!["compose.yaml".to_string()],
            generated_override: None,
        };
        let redis = named_service("cache", ServiceKind::Redis);
        let spawn = terminal(&invocation, &redis, false, TerminalTarget::Client);
        assert_eq!(
            spawn.args,
            [
                "compose",
                "--project-directory",
                "'/work/my blog'",
                "-f",
                "compose.yaml",
                "exec",
                "cache",
                "redis-cli"
            ]
            .map(String::from)
        );
    }

    #[test]
    fn test_service_terminal_without_client_is_an_error() {
        let invocation = sail_invocation("/work/blog");
        let mailpit = named_service("mailpit", ServiceKind::Mailpit);
        assert!(
            service_terminal_spawn(
                &invocation,
                "blog",
                &mailpit,
                true,
                "laravel.test",
                &TerminalTarget::Client,
                &HashMap::default(),
                ShellKind::Posix,
            )
            .is_err()
        );
    }

    #[test]
    fn test_quick_action_spawn() {
        let invocation = sail_invocation("/work/my blog");
        let spawn = quick_action_spawn(
            &invocation,
            "blog",
            QuickAction::Migrate,
            true,
            "laravel.test",
            NodePackageManager::Npm,
            &project_environment(),
            ShellKind::Posix,
        )
        .unwrap();
        assert_eq!(spawn.full_label, "blog · Migrate");
        assert_eq!(spawn.label, "blog · Migrate");
        assert_eq!(
            spawn.command.as_deref(),
            Some("'/work/my blog/vendor/bin/sail'")
        );
        assert_eq!(spawn.args, ["artisan", "migrate"].map(String::from));
        assert!(spawn.show_command);
        assert!(!spawn.use_new_terminal);
        assert!(spawn.allow_concurrent_runs);
        assert_eq!(spawn.reveal, RevealStrategy::Always);
        assert_eq!(
            spawn.env.get("SAIL_SKIP_CHECKS").map(String::as_str),
            Some("1")
        );

        let composer = quick_action_spawn(
            &invocation,
            "blog",
            QuickAction::ComposerUpdate,
            true,
            "laravel.test",
            NodePackageManager::Npm,
            &project_environment(),
            ShellKind::Posix,
        )
        .unwrap();
        assert_eq!(composer.full_label, "blog · composer update");

        let node = quick_action_spawn(
            &invocation,
            "blog",
            QuickAction::NodeDev,
            true,
            "laravel.test",
            NodePackageManager::Pnpm,
            &project_environment(),
            ShellKind::Posix,
        )
        .unwrap();
        assert_eq!(node.full_label, "blog · pnpm run dev");
        assert_eq!(node.args, ["pnpm", "run", "dev"].map(String::from));
    }

    fn custom_action(mode: CustomActionMode, command: &[&str]) -> CustomAction {
        CustomAction::validated(
            "Seed users",
            Some("laravel.test"),
            command
                .iter()
                .map(|argument| argument.to_string())
                .collect(),
            Some("/var/www/html"),
            mode,
            false,
        )
        .unwrap()
    }

    #[test]
    fn test_custom_action_spawn_reveal_follows_the_mode() {
        let invocation = sail_invocation("/work/blog");
        let spawn = |mode| {
            custom_action_spawn(
                &invocation,
                "blog",
                &custom_action(mode, &["php", "artisan", "db:seed"]),
                true,
                "laravel.test",
                &project_environment(),
                ShellKind::Posix,
            )
            .unwrap()
        };
        let terminal = spawn(CustomActionMode::Terminal);
        let background = spawn(CustomActionMode::Background);
        assert_eq!(terminal.full_label, "blog · Seed users");
        assert_eq!(terminal.reveal, RevealStrategy::Always);
        assert_eq!(background.reveal, RevealStrategy::NoFocus);
        assert!(background.show_command);
        assert!(!background.use_new_terminal);
        assert!(background.allow_concurrent_runs);
        assert_eq!(
            terminal.args,
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
            .map(String::from)
        );
    }

    #[test]
    fn test_custom_action_spawn_quotes_shell_metacharacters() {
        let invocation = sail_invocation("/work/blog");
        let spawn = custom_action_spawn(
            &invocation,
            "blog",
            &custom_action(
                CustomActionMode::Terminal,
                &["echo", "a b", ";", "&&", "$(id)"],
            ),
            true,
            "laravel.test",
            &project_environment(),
            ShellKind::Posix,
        )
        .unwrap();
        let tail = &spawn.args[spawn.args.len() - 5..];
        assert_eq!(
            tail,
            ["echo", "'a b'", "';'", "'&&'", "'$(id)'"].map(String::from)
        );
    }

    #[test]
    fn test_composer_custom_spawn() {
        let invocation = sail_invocation("/work/blog");
        let arguments = ["require", "laravel/horizon"].map(String::from);
        let spawn = composer_custom_spawn(
            &invocation,
            "blog",
            &arguments,
            true,
            "laravel.test",
            &project_environment(),
            ShellKind::Posix,
        )
        .unwrap();
        assert_eq!(spawn.full_label, "blog · composer require laravel/horizon");
        assert_eq!(
            spawn.args,
            ["composer", "require", "laravel/horizon"].map(String::from)
        );
        assert!(
            composer_custom_spawn(
                &invocation,
                "blog",
                &[],
                true,
                "laravel.test",
                &project_environment(),
                ShellKind::Posix,
            )
            .is_err()
        );
    }
}
