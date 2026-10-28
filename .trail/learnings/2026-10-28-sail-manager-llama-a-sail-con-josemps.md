---
title: Sail Manager llama a Sail con SAIL_SKIP_CHECKS=1 porque sus comprobaciones pueden hacer un down
author: Josemps
date: 2026-10-28
branch: feat/sail-manager
cost: { minutes: 5, tokens: 19433 }
id: l-15ca0747
scope: repo
status: active
src: agent
---

## Decided
- Toda orden lanzada a `vendor/bin/sail` desde Sail Manager lleva `SAIL_SKIP_CHECKS=1` en el entorno. La disponibilidad de Docker se comprueba aparte con `docker info` directo.

## Tried and ruled out
- Llamar a Sail sin esa variable: al arrancar, el script ejecuta `compose ps $APP_SERVICE` y, si el contenedor de la app está en `exited`, hace `compose down` antes de seguir. Un simple refresco del panel (`sail ps`) podía borrar los contenedores parados; rompe la garantía de no hacer `down` en acciones normales.

## Watch out
- `SAIL_FILES` exige que cada fichero exista y se resuelve contra el cwd: el proceso se lanza siempre con la raíz del proyecto como directorio de trabajo.

In the code:
- vendor/laravel/sail/bin/sail, bloque `if [ -z "$SAIL_SKIP_CHECKS" ]` (docker info + ps + down) y lectura de SAIL_FILES.
- crates/sail_manager/src/compose.rs, entorno de ComposeLauncher::Sail.
