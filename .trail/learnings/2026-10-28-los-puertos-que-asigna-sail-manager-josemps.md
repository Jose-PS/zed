---
title: "Los puertos que asigna Sail Manager deben ganar al .env sin tocarlo, con un override de Compose propio"
author: Josemps
date: 2026-10-28
branch: feat/sail-manager
cost: { minutes: 8, tokens: 43908 }
id: l-c31457ff
scope: repo
status: active
src: agent
---

## Decided
- En proyectos Sail todo se lanza con `vendor/bin/sail` (up, stop, ps, config, events, artisan, composer, npm, shell, logs), nunca con `docker compose` directo, porque Sail exporta WWWUSER/WWWGROUP/APP_SERVICE y los ficheros de SAIL_FILES antes de llamar a Compose. Solo `docker info` / `docker compose version` (disponibilidad) van directos a docker.
- Los puertos configurados en Sail Manager (Fase 5) tienen que prevalecer sobre lo definido en el `.env` del proyecto, sin modificar `.env` ni los compose del usuario.
- Mecanismo previsto: un fichero de override de Compose generado por Sail Manager fuera del repo (por proyecto), que redefine los `ports` con el tag `!override` (Compose >= 2.24), añadido a la lista de ficheros vía SAIL_FILES (o COMPOSE_FILE) junto al compose.yaml y sus overrides habituales.

## Tried and ruled out
- Pasar APP_PORT, FORWARD_DB_PORT, etc. como variables de entorno al proceso de `sail`: Sail hace `source ./.env` al arrancar y reasigna esas variables, así que el `.env` gana y el override no surte efecto.
- Escribir en `.env` o en los compose del proyecto: prohibido por la spec (no sobrescribir ficheros del usuario).

## Watch out
- Al poner SAIL_FILES, Sail deja de autodetectar ficheros: hay que listar explícitamente compose.yaml + compose.override.yaml (si existe) + el override de Sail Manager, con el primario primero para que el directorio del proyecto Compose siga siendo la raíz.
- Un `sail up` lanzado desde una terminal externa no lleva el override: los puertos solo cambian cuando se arranca desde Sail Manager. Hay que avisarlo en la UI.
- Un `ports:` en un override sin `!override` se fusiona (añade) en vez de sustituir.

In the code:
- crates/sail_manager/src/compose.rs (ComposeInvocation) y docker.rs (ComposeClient) son donde se construyen programa, args y entorno.
