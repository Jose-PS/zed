---
title: "Los ajustes de proyecto de Sail Manager se escriben bajo la clave de nivel superior, no bajo la de proyecto"
author: Josemps
date: 2026-10-28
branch: feat/sail-manager
cost: { minutes: 1, tokens: 16940 }
id: l-dae06391
scope: repo
status: active
src: agent
touches:
  - ".zed/settings.json"
  - crates/settings_ui/src/settings_ui.rs
---

## Decided
- Al escribir `.zed/settings.json` desde Sail Manager, los campos de proyecto (app_service, url_host, compose_files, shell, manual_urls) se escriben por `content.sail_manager.project.*`, because al leer el fichero la clave `sail_manager` se rellena en `content.sail_manager` (aplanada con flattened_deserialize, como terminal) y no en `content.project.sail_manager`.

## Tried and ruled out
- Escribir por `content.project.sail_manager`: si la clave ya existe en el fichero, el cambio se pierde por la serialización de clave duplicada.

## Watch out
- La escritura va por buffer + `SettingsStore::new_text_for_update` (edición mínima, conserva el resto del fichero), imitando la cola de ajustes de proyecto de settings_ui.

In the code:
- crates/sail_manager/src/project_settings.rs `update_project_settings_file`, `set_manual_url`; crates/settings_ui/src/settings_ui.rs `ProjectSettingsUpdateQueue::process_entry`.
