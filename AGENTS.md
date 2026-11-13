## Agent skills

### Development commands

Run builds, tests and checks through the recipes in `justfile` (`just --list`).
For a heavy command without a recipe, use `heavy -- <command>` so concurrent
agents and worktrees share the machine's slots. Run long-lived application
processes outside `heavy`; build them with `just build` first.

### Commit messages

Use the project's Conventional Commits style: `<type>: <summary>`, with a
lowercase type and no scope. Prefer a short Russian summary, keeping technical
names as written; older commits also use English. Describe the concrete change
and omit the trailing period.

Use `feat`, `fix`, `refactor`, `test`, `docs` or `build` for the corresponding
change. The project also uses `ai` for agent tooling and skills, and `prototype`
for prototypes. When including an issue or PR number, append it as `(#N)`.
Example: ``feat: команда `loader` на веб-ленте (#20)``.

### Issue tracker

Issues are tracked in GitHub Issues for this repository. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses the five canonical label names unchanged. See `docs/agents/triage-labels.md`.

### Domain docs

This repository uses a single-context domain documentation layout. See `docs/agents/domain.md`.
