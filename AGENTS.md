# Stylon

Stylon is a deterministic Rust project style checker and fixer. Read the
relevant implementation and `STYLON_TECHNICAL_DESIGN.md` section before changing
behavior; tests and code are the source of truth.

## Work and validation

- Use the `wt` skill at `~/.llms/skills/wt/SKILL.md` unless explicitly asked to
  work on `master`. Never edit the primary checkout otherwise.
- Create each task worktree through Tollgate from the certified `release` ref.
  Continue follow-ups in that worktree until promotion.
- Run focused tests while developing, then `cargo fmt --all -- --check`,
  `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets`.
- Stage every intended change before final validation. Preserve unrelated user
  changes and keep generated or temporary artifacts out of the commit.
- Commit once with a Conventional Commit message, submit the exact commit as a
  Tollgate candidate, authorize it, and wait for certified promotion and remote
  synchronization. Worktree branches remain local.

## Code and policy

- Keep rules deterministic and syntax-driven. Do not depend on host compiler
  inference or network access.
- Diagnostic-only rules must never mutate source under `--fix`.
- Add regression coverage for accepted and rejected forms, configuration
  defaults and overrides, and whole-engine behavior when orchestration changes.
- Preserve exact source text outside machine-applicable edits and keep JSON
  diagnostics stable.
- Update the technical design when a public rule or configuration contract
  changes. Describe current behavior rather than implementation history.
