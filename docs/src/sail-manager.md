---
title: Sail Manager - Zed
description: Manage Laravel Sail and Docker Compose environments from a panel in Zed. Start, stop and restart services, open logs and terminals, and keep host ports from clashing.
---

# Sail Manager

Sail Manager is a panel for managing the containers of a Laravel Sail or Docker Compose project without leaving the editor. It shows every service of the project with its state and published ports, and lets you start, stop and restart the whole environment or a single service, open logs and terminals, open the web interfaces in the browser, and run common Artisan, Composer and Node commands.

## Opening the panel {#opening}

The panel is docked on the right by default. Open it with the `Sail Manager` button in the status bar, or with {#action sail_manager::ToggleFocus} from the command palette. The other actions are {#action sail_manager::Refresh}, {#action sail_manager::StartEnvironment}, {#action sail_manager::StopEnvironment} and {#action sail_manager::RestartEnvironment}.

The panel works on the first visible folder of the project. It needs a `compose.yaml` (or `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`) in that folder, or the `compose_files` setting. Without a Compose file, with Docker unavailable, or in a remote or collaborative project, the panel shows a message explaining why it has nothing to show. Project settings can be edited from the panel with the settings button in its header.

## Sail projects {#sail}

A project is treated as a Sail project when `laravel/sail` is in `composer.json` and `vendor/bin/sail` exists. In that case every Compose operation (`up`, `stop`, `restart`, `ps`, `config`, `events`, `logs`, `exec`, Artisan, Composer and Node commands) goes through `vendor/bin/sail`, not through `docker compose` directly. Sail exports `WWWUSER`, `WWWGROUP`, `APP_SERVICE` and the list of Compose files before calling Compose, so calling Compose directly would not see the same environment.

Every call to `vendor/bin/sail` is made with `SAIL_SKIP_CHECKS=1`. Sail's own pre-flight checks can run a `down` when it thinks the containers are stale, and the panel must never destroy anything on its own. Only the availability check (`docker info` and `docker compose version`) talks to Docker directly.

**Stop** runs `stop`: it never runs `down` and never removes volumes (`-v`). Stopped containers, networks and data stay as they were.

## Project settings {#settings}

Settings live under the `sail_manager` key of the user settings file, and the project-level ones can be overridden in `.zed/settings.json` of the project. The panel writes them there when you change them from its settings view.

| Key | Default | Description |
| --- | --- | --- |
| `button` | `true` | Show the panel button in the status bar (user settings only). |
| `dock` | `"right"` | Where to dock the panel: `"left"` or `"right"` (user settings only). |
| `default_width` | `320` | Initial width of the panel (user settings only). |
| `app_service` | `"laravel.test"` | Name of the main application service. Terminals, Artisan, Composer and Node commands run in it. |
| `url_host` | `"localhost"` | Host used to build the URLs of web services. |
| `compose_files` | `[]` | Compose files to use, relative to the project root. Empty lets Compose (or Sail) detect `compose.yaml` and its overrides. |
| `shell` | `"auto"` | Shell for service terminals: `"auto"`, `"zsh"`, `"bash"` or `"sh"`. |
| `manual_urls` | `{}` | URL to open for a service, by service name, when its web interface cannot be detected from its ports. |
| `node_package_manager` | `"auto"` | `"auto"`, `"npm"`, `"pnpm"`, `"yarn"` or `"bun"`. Auto detects it from the lock files. |
| `actions` | `[]` | Custom actions shown in the panel (see below). |
| `refresh_mode` | `"events_and_poll"` | How the panel stays up to date: `"events_and_poll"` (Docker events plus a refresh every 30 seconds), `"events"` (Docker events only) or `"manual"`. |
| `auto_assign_ports` | `true` | Assign a free host port when a published port is taken (see below). |
| `port_pins` | `{}` | Fixed host ports, keyed by `service:container_port/protocol`. |

A custom action runs `command` inside a service with `docker compose exec` (through Sail in Sail projects). `service` defaults to `app_service`, `working_directory` is optional, `mode` is `"terminal"` (default, opens a terminal) or `"background"`, and `confirm` asks for confirmation before running.

```json
{
  "sail_manager": {
    "app_service": "laravel.test",
    "url_host": "blog.test",
    "compose_files": ["compose.yaml", "compose.override.yaml"],
    "shell": "bash",
    "node_package_manager": "pnpm",
    "refresh_mode": "events_and_poll",
    "manual_urls": {
      "mailpit": "http://localhost:8025"
    },
    "actions": [
      {
        "name": "Fresh Migrate With Seeders",
        "command": ["php", "artisan", "migrate:fresh", "--seed"],
        "mode": "terminal",
        "confirm": true
      },
      {
        "name": "Clear Redis",
        "service": "redis",
        "command": ["redis-cli", "flushall"],
        "mode": "background",
        "confirm": true
      }
    ],
    "auto_assign_ports": true,
    "port_pins": {
      "laravel.test:80/tcp": 8080
    }
  }
}
```

## Port assignment {#ports}

Several projects on the same machine usually publish the same host ports (`80`, `3306`, `6379`). When you start the environment from the panel, Sail Manager reads the resolved configuration (`config`), checks which published ports are taken by other processes or reserved by other projects, and moves the conflicting ones to a free port. Ports of containers that are already running are never moved, and privileged ports below 1024 are moved above 8000. The assignment is remembered so the same port is reused on the next start. The panel reports the ports it moved after starting, and the settings view lists original and assigned ports, where you can pin a port or reset all assignments.

Your `.env`, `compose.yaml` and overrides are never edited. Instead, the panel writes a Compose override outside of the repository, at `~/.local/share/zed/sail_manager/<hash>/ports.override.yaml` (the directory depends on the platform and `<hash>` identifies the project root). It redefines only the `ports` of the services that changed, using the `!override` tag so the list replaces the original one instead of being added to it:

```yaml
services:
  laravel.test:
    ports: !override
      - "8081:80"
      - "5173:5173"
```

The override is passed to Compose by appending it to `SAIL_FILES` (Sail projects) or to the file list (plain Compose projects), always after the project's own files and with the primary file first, so the Compose project directory remains the project root. Setting `SAIL_FILES` turns off Sail's file auto-detection, so the panel lists `compose.yaml` and `compose.override.yaml` (if present) explicitly, or the files from `compose_files`.

Things to know:

- A `sail up` launched from an external terminal does not carry the override and uses the original ports. The assigned ports only apply to what is started from the panel.
- `!override` needs Docker Compose 2.24.4 or newer. With an older version the panel does not use the override and says so in the settings view.
- With `auto_assign_ports` turned off, only the `port_pins` are applied. If a pinned port is busy, the start fails with an error naming the port instead of moving it.
- If nothing conflicts, no override is written and an existing one is removed on the next start.

## Credits {#credits}

The `container`, `database`, `globe` and `scroll-text` icons used by the panel come from [Lucide](https://lucide.dev) and are distributed under the ISC license.
