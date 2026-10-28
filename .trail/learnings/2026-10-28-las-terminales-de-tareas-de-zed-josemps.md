---
title: Las terminales de tareas de Zed pasan la orden por la shell del usuario sin citar los argumentos
author: Josemps
date: 2026-10-28
branch: feat/sail-manager
cost: { minutes: 1, tokens: 0 }
id: l-809c2c21
scope: repo
status: active
src: agent
touches:
  - crates/util/src/shell_builder.rs
  - crates/terminal_view/src/terminal_panel.rs
---

## Decided
- Todo SpawnInTerminal que construye Sail Manager cita el programa y cada argumento con el mecanismo de Zed (`ShellKind::try_quote_prefix_aware` para el programa, `try_quote` para cada arg), because Zed no los cita: una ruta de proyecto con espacios o un comando personalizado se rompería o se interpretaría como shell.

## Watch out
- `TerminalPanel::spawn_task` → `prepare_task_for_spawn` → `ShellBuilder::build_no_quote` concatena command y args con espacios sin citar y lo ejecuta como `shell -i -c "<cadena>"`.
- Con `allow_concurrent_runs: true` y `use_new_terminal: false`, lanzar otra vez una tarea con el mismo `full_label` reemplaza la terminal (vuelve a ejecutar la orden) en lugar de enfocarla, otherwise se perdería la sesión de shell abierta si se usa para terminales interactivas.

In the code:
- crates/util/src/shell_builder.rs `build_no_quote`; crates/terminal_view/src/terminal_panel.rs `spawn_task` / `prepare_task_for_spawn`; crates/sail_manager/src/terminal.rs.
