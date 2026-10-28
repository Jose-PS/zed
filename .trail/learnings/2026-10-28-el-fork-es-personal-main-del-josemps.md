---
title: "El fork es personal: main del fork es la base y las features entran por PR con squash"
author: Josemps
date: 2026-10-28
branch: feat/sail-manager
cost: { minutes: 4, tokens: 36340 }
id: l-bb881a8d
scope: repo
status: active
src: agent
---

## Decided
- El fork Jose-PS/zed es un Zed personal adaptado a desarrollo Laravel; no se aporta nada a upstream.
- La rama principal es `main` del remoto `fork`. Cada feature va en su rama `feat/<nombre>` que sale de `main`, se sube a `fork` y entra con PR con squash; el merge lo hace Jose.
- El trabajo de git previo (merge, cherry-pick, pestaña de ramas) entra en `main` una sola vez con merge normal (PR #1) para conservar sus 19 commits.

## Tried and ruled out
- PR con squash para el trabajo de git: perdía el historial de 19 commits ya ordenados.
- Sacar las features de `main` tal como estaba (espejo de upstream): dejaba fuera el trabajo de git.

## Watch out
- `fork/main` no es ancestro de `git-merge-cherry-pick` (tenía 13 commits de upstream propios); por eso no hay fast-forward y hace falta merge commit.
- `origin` (zed-industries/zed) solo se usa para traer cambios; nunca push ni PR allí.

In the code:
- remotos: `origin` = git@github.com:zed-industries/zed.git, `fork` = git@github.com:Jose-PS/zed.git
- PR #1: git-merge-cherry-pick -> main (merge commit); feature actual: feat/sail-manager
