# CLAUDE.md

master-of-agents (moa) is in the **design phase**. Read `docs/context.md` first. It's
local only and not in the repo, on purpose. It has the vision, what's been tried,
measurements from this machine, the draft constitution and the open questions.

Design method: Superpowers (`brainstorming`, then `writing-plans`). Design docs go in
`docs/`. Don't write implementation code until the design is approved.

`spike/` holds throwaway prototypes used to learn what to build (the moa prototype is
`spike/moa-proto/moa`). `spike/LEARNINGS.md` records what they taught us. None of it is
product code.

## Repo rules

- **Public, open-source (MIT).** Never commit machine, employer or account specifics:
  hostnames, security tooling, internal hosts, work accounts. `docs/context.md` stays local.
- **PRs only, linear history.** Branch from `main`, open a PR, and merge by squash or
  rebase. No merge commits and no direct pushes to `main` (a ruleset enforces this).
- **CI must pass** (the `ci` check: build and tests) before a PR can merge.
