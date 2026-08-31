# Stylon Technical Design

## Introduction

Stylon is a Rust command-line tool that finds and automatically repairs a fixed
set of project style conventions. It is intended for local development and CI
on large Rust workspaces where formatting alone cannot enforce module layout,
name qualification, import policy, test placement, dependency policy, and item
ordering.

The design has four controlling requirements:

- A full scan of a 100,000-line Rust project must finish in at most five
  seconds. The acceptance corpus is larger: Battlement currently contains
  138,483 tracked Rust lines.
- Every style finding must have a deterministic machine-applicable fix.
  Conditions that prevent a safe fix are operational errors, not unfixable
  style findings.
- Adding a rule must require a small, isolated Rust implementation rather than
  changes throughout the scanner or fixer.
- Rules may be enabled or disabled for the project or selected directories only
  through one root `stylon.toml`. Source annotations and line-level suppression
  comments have no effect.

Stylon meets the performance requirement by parsing source directly, building
only the project facts needed by its rules, and never invoking `rustc` or the
rust-analyzer semantic engine during a normal scan. It meets the fix guarantee
by composing all edits in memory, proving that the result parses and is clean,
then using recoverable writes and Cargo validation.

Rust 1.94 is the initial minimum supported Rust version for building Stylon.

## Related Information

- [rust-analyzer architecture][ra-architecture] explains the lossless,
  per-file syntax trees used as the parsing model. Its syntax layer supports
  parallel parsing without requiring compilable code or a semantic database.
- [Cargo workspaces][cargo-workspaces] defines workspace membership and the
  inheritance behavior of `[workspace.dependencies]`, including additive
  member features and the restriction against optional workspace entries.
- [Rustdoc intra-doc links][rustdoc-links] defines link syntax, scope, and the
  important fact that backticks inside a link do not turn the link into plain
  code.
- [Battlement at the audited revision][battlement-revision] is the correctness
  and performance stress corpus used by this design.

[ra-architecture]: https://rust-analyzer.github.io/book/contributing/architecture.html
[cargo-workspaces]: https://doc.rust-lang.org/cargo/reference/workspaces.html
[rustdoc-links]: https://doc.rust-lang.org/rustdoc/write-documentation/linking-to-items-by-name.html
[battlement-revision]: https://github.com/thurn/battlement/tree/725660cccf08e66d2151ad5c5566fc4245e7070d

## User Interface

Stylon has one scan command rather than separate lint and check subcommands.
With no path, it scans the current project. A supplied path may name one file or
one directory.

```text
stylon [--fix] [--format human|json] [--config PATH] [--timings] [PATH]
```

The command behaves as follows:

- Check mode is the default. It reads files and reports enabled findings but
  does not change project content.
- `--fix` applies all enabled fixes as one transaction. It never applies a
  subset when another enabled fix cannot be planned or validated.
- `--format human` emits compiler-style diagnostics to standard output and
  operational errors to standard error.
- `--format json` emits one JSON document containing a schema version,
  diagnostics, errors, and a summary.
- `--config PATH` selects an explicit root configuration. Without it, Stylon
  searches ancestors for `stylon.toml`.
- `--timings` reports discovery, Cargo metadata, parsing, indexing, rule
  evaluation, fix planning, output, and Cargo validation separately.

Exit status is a stable automation interface:

- `0` means check mode found no enabled violations, or fix mode completed and
  passed validation.
- `1` means check mode found one or more style violations.
- `2` means configuration, discovery, parsing, analysis, planning, filesystem,
  preflight, or post-fix validation failed.

Human diagnostics use project-relative paths and one-based positions.

```text
crates/cards/src/deck.rs:18:9: path.type-qualification:
type name must be unqualified; replace `cards::Card` with `Card`
```

### JSON diagnostics

JSON output is versioned independently from the Stylon binary. Version 1 is a
single UTF-8 object so callers can validate the whole run before consuming a
partial result.

```json
{
  "schema_version": 1,
  "diagnostics": [{
    "rule_id": "path.type-qualification",
    "message": "type name must be unqualified",
    "path": "crates/cards/src/deck.rs",
    "range": {"byte_start": 412, "byte_end": 423,
              "start": {"line": 18, "column": 9},
              "end": {"line": 18, "column": 20}},
    "fix": "machine-applicable"
  }],
  "errors": [],
  "summary": {"files": 469, "findings": 1, "fixed": 0, "remaining": 1}
}
```

Every diagnostic contains a stable rule ID, message, project-relative path,
half-open UTF-8 byte range, one-based line and Unicode-scalar column range, and
the literal fix value `machine-applicable`. `byte_end` is the first byte after
the finding. CRLF occupies two bytes but advances one line. Edit text is
intentionally not part of the public JSON schema.

In check mode, `diagnostics` contains current findings. In successful fix mode,
it contains the original findings with an additional `applied: true` field;
the final clean scan is reflected by `summary.remaining = 0`. Errors contain a
stable category, message, sorted project-relative paths, and captured validation
output when relevant. Paths must be UTF-8; a selected non-UTF-8 path is an
operational error.

JSON mode reserves standard output for the single JSON object. Diagnostics,
validation output, and operational details are never also printed to standard
error unless JSON serialization itself fails. Errors and diagnostics use the
same path-and-position sort order. Validation output is captured, truncated to
one MiB per stream with an explicit truncation flag, and included only on
failure.

Consumers must reject unknown `schema_version` values and ignore unknown object
fields within a known version. Adding an optional field is compatible; removing
or changing a field increments the schema version. Rule IDs are stable once
released.

Diagnostics are sorted by normalized path, byte offset, and rule ID. Parallel
execution must never alter output order.

## Configuration

Stylon uses at most one root `stylon.toml` for a run. If no configuration is
found, every rule is enabled, Git-aware ignore behavior applies, and the
default validation command is used.

The configuration schema begins with `version = 1`. Unknown keys, unknown rule
IDs, invalid globs, multiple applicable configuration files, and unsupported
schema versions are fatal configuration errors. They never silently fall back
to defaults.

```toml
version = 1
exclude = ["generated/**"]

[validation]
command = ["cargo", "check", "--workspace", "--all-targets", "--all-features"]

[macros]
constant_definitions = ["thread_local", "my_crate::define_constants"]

[tests]
additional_attributes = ["my_test_framework::case"]

[rules]
"tests.file-suffix" = true

[[overrides]]
paths = ["crates/legacy/**"]
[overrides.rules]
"tests.file-suffix" = false
```

Configuration paths use `/` separators and are matched relative to the
configuration root. `**` crosses directories. Root rule settings override the
built-in enabled defaults. Matching override blocks are then applied in file
order, and the last assignment to a rule wins.

Globs are case-sensitive on every platform. A leading `/` is rejected, a
trailing `/` means all descendants, `\` escapes the following metacharacter,
and path normalization removes `.` but rejects `..`. A matching excluded
directory prunes traversal. Rule enablement for an existing finding uses its
original path; after any virtual move, all rules are recomputed against the
destination path and the result must also be clean.

`exclude` removes matching files and directories before parsing. It is the
appropriate mechanism for tracked generated code and deliberately invalid Rust
fixtures. Disabling every rule for a path is not equivalent because the file
would still participate in project indexing.

`thread_local` is always treated as a constant-defining macro. Configured names
are exact macro paths after removing the trailing `!`; they extend rather than
replace that default.

The validation command is an argument array executed directly in the project
root, never a shell string. When omitted, it is:

```text
cargo check --workspace --all-targets --all-features
```

Stylon adds `--locked` to its default command when a `Cargo.lock` already
exists. A configured command is used exactly as written. Projects with mutually
exclusive features or non-Cargo validation replace the complete command.

The configuration format has no per-line, per-item, severity, or source
annotation setting. Text such as `// stylon-ignore`, `#[allow(stylon)]`, or
similar spellings is ordinary source text and does not suppress diagnostics.

Additional test attributes are exact paths after removing `#[` arguments and
`]`. They extend the built-in recognition of `test`, any path ending in
`::test`, `rstest`, and `test_case`.

Schema version 1 remains readable for the lifetime of Stylon 1.x. A later
binary may add optional keys but must reject removed or reinterpreted keys
unless `version` changes.

## Source Discovery and Project Model

Discovery must include new untracked source while avoiding build output and
large generated trees. Stylon therefore walks the requested directory with
Git-aware ignore semantics rather than relying on `git ls-files` or only on
Cargo target reachability.

**Analysis root** means the directory that owns configuration, path matching,
locking, diagnostics, and transactions. Stylon determines it as follows:

1. With `--config`, use the configuration file's parent and require the
   requested path to be inside it.
2. Otherwise, collect every ancestor `stylon.toml`; exactly one is allowed and
   its parent is the root.
3. Without configuration, use the nearest enclosing Cargo workspace root, then
   the nearest Git worktree root, then the requested directory or file parent.

An explicit configuration disables ancestor selection. A second
`stylon.toml` below the analysis root that could govern a selected path is a
configuration error; nested configuration never establishes inheritance.

**Selected files** are files eligible to receive diagnostics. **Context files**
are manifests and Rust modules read only to resolve selected files. A directory
selects its non-ignored descendants. An explicitly named `.rs` or `Cargo.toml`
overrides Git ignore rules but not `exclude` from configuration. Context may
extend to the analysis root, Cargo workspace catalog, target root, parent module
declarations, and reachable sibling modules. It never produces findings outside
the selected set.

Fixes normally change selected files. A structural fix may also change a
context manifest, parent module declaration, or literal include reference when
that edit is required to keep a selected-file move valid. These files form the
**structural closure** and are listed before preflight. Stylon never changes any
other context file.

The walker has these invariants:

- It considers non-ignored `.rs` files and files named `Cargo.toml`.
- It honors repository ignore files, global Git excludes, and `exclude` from
  `stylon.toml`.
- It does not follow directory symlinks or traverse `.git`, `target`, or sibling
  worktree content when those paths are ignored.
- It includes workspace members, excluded sample crates, nested fixture crates,
  and unattached source files when their paths are otherwise in scope.
- It reads each selected file once into an immutable in-memory snapshot.

Outside Git, the same walker honors `.ignore` files and built-in exclusions for
`.git` and `target`; other hidden directories are included unless excluded.
Directory symlinks are never followed. A selected file symlink, symlinked
manifest, hard-linked file with link count greater than one, non-UTF-8 path, or
path crossing outside the analysis root is an operational error. Filesystem
boundaries are allowed only when the canonical path remains below the analysis
root.

Stylon canonicalizes discovered manifests, walks their Cargo workspace
ownership, and retains only the outermost owning invocation for each package.
Cargo metadata itself may create or update `Cargo.lock`, so Stylon never runs it
in the source tree. It creates a temporary **metadata sandbox** that mirrors
manifest-relative directories, Cargo configuration, every discovered manifest,
the existing lockfile if present, and empty files at declared or conventional
target entry paths. Path dependencies remain at the same relative locations.
No source bytes are needed for metadata.

It invokes the following command inside that sandbox once for each distinct
workspace and once for each package not owned by a workspace:

```text
cargo metadata --no-deps --format-version 1 --manifest-path ABSOLUTE_PATH
```

The subprocess inherits the environment and uses the mirrored Cargo
configuration because those define the actual project. Its working directory is
the mirrored manifest parent. Returned canonical paths are mapped back to their
source-tree witnesses; a path escaping the mirrored analysis root is an error.
Sandbox files and any generated lockfile are deleted after reading the result,
so check mode is read-only even when the project has no lockfile. Metadata
failure is a `cargo-metadata` operational error. Stylon supports the Cargo
shipped with Rust 1.94 or newer. Metadata identifies
workspace membership, package targets, crate roots, explicit test targets, and
proc-macro crates. It does not load dependency source or build the project.

Module identity is built by starting at every Cargo target root and recursively
following external `mod` declarations and literal `#[path]` attributes. A
declaration without `#[path]` considers exactly Rust's two conventional
candidates: `name.rs` and `name/mod.rs` relative to its module directory. Zero
or two existing candidates is an error when the declaration affects a selected
file. Literal `include!` source participates in the enclosing module but remains
a distinct physical file.

An unattached file under a unique package source directory receives the module
path implied by its relative `.rs` path. Otherwise it receives local syntax
rules only. A rule requiring module, visibility, or Cargo identity reports an
analysis error for that file rather than guessing. Local rules are item spacing,
item ordering, import placement, and syntactically self-contained path checks;
all other rules require project identity when they construct a fix.

Conditional declarations are indexed as alternatives after normalizing
`cfg`, compound `cfg(all/any/not)`, and literal `cfg_attr` predicates. Stylon
does not evaluate them for the host. A reference is unambiguous only when every
alternative that could define the name leads to the same canonical item and
module. Otherwise a fix depending on that reference is an analysis error.

Rust edition and prelude are read per Cargo target from metadata, defaulting as
Cargo does when omitted. Resolver and per-package `rust-version` do not change
syntax parsing; they are preserved for validation and manifest edits.

## Analysis Architecture

The normal scan has three bounded computation stages: parallel parsing, one
project index merge, and parallel rule evaluation. There is no persistent
correctness cache and no compiler process.

### Lossless syntax

Stylon pins `ra_ap_syntax = "=0.0.350"`. The rust-analyzer syntax API is not a
stable compatibility boundary, so upgrades require the complete parser and
Battlement corpus tests before changing the pin.

The parser retains comments, whitespace, attributes, and exact byte ranges.
Each file produces both a syntax tree and a compact set of facts during one
walk:

- item kind, name, visibility, attributes, and complete source range;
- imports and the scopes containing them;
- module declarations and path attributes;
- public function and type declarations;
- paths in type, expression, pattern, and call positions;
- Rustdoc comment ranges and link candidates;
- test attributes and inline test modules; and
- top-level trivia needed to move items without losing comments.

Parser errors do not stop other files from being inspected, but any parser
error makes the run operationally unsuccessful. `--fix` refuses all writes
until every selected Rust file parses.

### Project index

The project index stores only facts required by enabled rules. It is merged
once from per-file facts and then exposed read-only to parallel checks.

It contains:

- Cargo packages, targets, workspace members, and manifest ownership;
- the module graph and each file's enclosing module;
- locally defined types and unrestricted public functions;
- imports, aliases, glob sources, and visibility;
- known public paths for workspace items; and
- an embedded Rust 1.94 standard-library free-function index.

The standard-library index is generated from Rust 1.94 rustdoc JSON, normalized
to sorted records of public path, item kind, and canonical module, reviewed in
the same change as any Rust-version update, and compiled into the binary.
Generation is reproducible from the pinned toolchain and a checked-in command;
runtime analysis does not scan the sysroot. Standard path aliases are resolved
through imports, so `fs::File` inherits the exemption of `std::fs::File`.

Third-party dependency APIs are not indexed. Direct-function-import checking
covers project and standard-library public functions. Syntax-position rules
still normalize third-party type and variant paths without loading dependency
source.

### Macro boundary

Arbitrary macro token trees are opaque. Stylon may inspect a macro's path,
attributes, delimiter range, and top-level position, but it never interprets or
rewrites Rust-looking tokens inside the invocation.

Unknown top-level macro declarations and invocations are ordering barriers.
Items on opposite sides are not compared for ordering and are never moved
across the barrier. `thread_local!` and configured constant-defining macros are
known items rather than barriers.

Stylon recognizes literal path arguments to `include!`, `include_str!`, and
`include_bytes!` only for updating references during a file move. Their other
tokens remain opaque.

### Rule interface

Rules are compiled into Stylon. There is no plugin ABI, dynamic library, or
external rule process. A rule declares its stable ID, input file kinds, project
facts, and check function against immutable context.

An **interest** selects syntax or manifest facts that cause a rule to run. A
**change recipe** is a declarative set of byte edits, creates, moves, deletes,
visibility changes, or manifest operations plus hashes and resolution
preconditions. A **known public path** is a bare-`pub` path reachable from a
Cargo target root through declarations or re-exports. Restricted visibility
means every `pub(...)` form; **unrestricted public** always means bare `pub`.

```rust
trait Rule: Sync {
  fn id(&self) -> RuleId;
  fn interests(&self) -> Interests;
  fn check(&self, context: &RuleContext<'_>, output: &mut Findings);
}
```

A `Finding` contains a source span and one recipe built through shared planners.
Recipes use operations equivalent to the following closed set:

```rust
enum Operation {
  Replace { file: FileId, range: TextRange, text: String },
  Create { path: PathId, bytes: Vec<u8>, mode: u32 },
  Move { from: PathId, to: PathId },
  Delete { path: PathId },
  Manifest(ManifestOperation),
}
```

Rules do not write files and do not invoke other rules. Shared import selection,
module resolution, item movement, and text editing belong to common services so
new rules do not reproduce correctness-sensitive logic. Rules may request a
shared planner operation by semantic key; this is how two findings reuse one
import instead of emitting overlapping replacements.

The compile-time registry validates unique IDs and requires each rule type to
implement recipe construction. Runtime planning still verifies every recipe;
the registry does not claim to prove applicability at compile time. A condition
that prevents construction or validation is promoted to an operational error
before the rule's findings are reported. Adding a convention consists of
registering one rule, its focused fixtures, and any explicitly declared shared
facts.

Per-file rule panics are caught under Stylon's required unwind panic profile,
converted to sorted `internal-rule` errors, and prevent fix mode. A poisoned or
shared-state panic aborts the scan with exit `2`; partial results are never
treated as a clean run.

## Fix Planning and Transactions

Fix mode separates deciding changes from writing them. This is necessary
because a module extraction can create imports that another rule must rewrite,
and two independently correct edits can overlap.

Planning uses a virtual filesystem initialized from the immutable scan. It
applies rule changes in a deterministic priority order:

1. manifest and filesystem structure changes;
2. module and test extraction or renaming;
3. import and path rewrites;
4. item ordering; and
5. blank-line normalization.

Within a priority, rules sort by rule ID and findings sort by normalized path
and source offset. Identical replacements and operations carrying the same
shared-planner key are merged. Any other overlapping byte ranges, moves, or
manifest fields conflict.

After each pass, changed virtual files are reparsed. Stylon conservatively
rebuilds the module, import, symbol, test, and manifest facts for every Cargo
target touched by the change. Destination-path configuration and rule interests
are recomputed. Planning ends only when no enabled findings remain.

A virtual state hash covers every path, file byte string, file mode, manifest
model, and effective rule policy. Repeating a hash is a cycle. More than eight
passes is a `non-converging-fix` error; eight permits every priority to expose a
later rule and still leaves three guard passes beyond the expected maximum.

For example, extracting an inline test can expose three later changes:

```text
pass 1: `mod tests { ... }` -> external `parser_tests.rs`
pass 2: `use super::*` -> absolute, explicit imports in the new file
pass 3: direct function imports -> module-qualified calls
pass 4: reorder items and insert blank lines; pass 5 is clean
```

Destination collisions include case-fold-equivalent paths, an existing
different file, and simultaneous `foo.rs` plus `foo/mod.rs`. An existing file
with identical bytes is still a conflict unless it is the recorded source of
the same move; Stylon does not silently adopt unrelated files.

Before touching disk, Stylon verifies all of these properties:

- every changed Rust file parses without errors;
- every enabled finding has disappeared;
- another fix pass produces no edits;
- every edit's expected input bytes still match the original snapshot; and
- every create, move, and delete has an unambiguous destination.

`--fix` with no findings performs no preflight and exits `0` after the clean
scan. Validation exists to establish the safety of planned writes, not as an
independent build command.

### Validation and recoverable writes

The transaction state sequence is snapshot, plan, lock, preflight journal,
preflight, extend journal, replace, validate, then commit or restore. It
guarantees recovery and exact rollback of Stylon's source changes. It does not
pretend that several filesystem renames become one atomic operation visible to
unrelated readers.

Stylon acquires an exclusive create-only lock under the analysis root before
preflight. The lock records the process ID, process start token, root, and
transaction ID. Another live owner causes a `project-locked` error. A dead owner
is stale only after its journal is recovered; Stylon never removes a stale lock
without examining recovery state.

The lock coordinates Stylon processes, not arbitrary editors. Stylon hashes the
selected and structural-closure inputs after locking, before each replacement,
and before validation. A third-state change aborts and restores only paths still
matching Stylon's recorded replacement hash.

`--fix` runs the configured validation command before writing. A failing
baseline stops immediately because Stylon could not attribute a later failure
to its edits.

Before each validation, Stylon inventories and hashes selected Rust files,
manifests, `stylon.toml`, `Cargo.lock`, and the structural closure. Changes to
those inputs made by validation are validation failures. Stylon restores them,
including deleting a `Cargo.lock` created when none existed. Build output and
other ignored paths are outside the inventory. A custom validation command is
contractually required not to mutate other project inputs.

The original inventory and recovery bytes are flushed into a preflight-phase
journal before baseline validation starts. If preflight fails normally, Stylon
restores any monitored side effects and removes the journal before returning.
If Stylon or the machine dies, startup recovery recognizes the preflight phase,
terminates any surviving owned validator, restores the inventory, and removes
the journal. A failed preflight therefore leaves no journal after recovery, not
because no journal ever existed.

After a clean preflight, Stylon extends the durable journal with each affected
regular file and directory, mode, original hash or absence, replacement hash or
absence, and recovery location. Original bytes are retained until the
transaction completes. Hard links and symlinks were rejected during discovery;
ownership, ACLs, extended attributes, and timestamps are outside the
preservation contract. File contents, executable mode, names, directory
existence, and original absence are preserved.

Recovery files are private to the current user, collision-resistant, and placed
on the same filesystem as their targets. Journal data, recovery data, replaced
files, and parent directories are flushed in that order. Individual
same-filesystem renames are atomic, but another process can briefly observe a
partially replaced multi-file tree. The project lock tells cooperating tools not
to read during that interval.

The validation command then runs again. Success removes recovery data and the
journal. Failure, interruption, or child-process termination restores original
bytes, modes, names, and absence of newly created files before Stylon exits.

Each validation command runs as a new Unix process group or Windows Job Object
owned by the transaction. On interruption or failure, Stylon sends graceful
termination to the whole group, waits up to five seconds, force-terminates the
remaining group, and reaps it completely before restoring a single path. A
configured validator must not detach or daemonize outside that ownership
boundary; doing so voids validation safety and is reported when detectable.

The journal records completion after every rename. On startup, Stylon detects an
unfinished journal before scanning and replays completed steps in reverse,
including partially created directories. It restores automatically when every
target still matches either the recorded original or replacement hash. If
another process changed a target to a third state, Stylon does not overwrite it
and reports exact manual recovery paths. `SIGINT` and `SIGTERM` trigger the same
in-process rollback; `SIGKILL` or power loss relies on startup recovery.

The transaction never uses Git reset or assumes a clean working tree. Existing
user changes are preserved byte for byte. The project must not be edited
concurrently while `--fix` owns its project lock.

The semantic guarantee is relative to the configured validation command. The
default is comprehensive for host targets and all features, but cannot validate
other operating-system targets. Replacing it is an explicit project decision;
Stylon guarantees parsing, a clean fixed point, transaction safety, and success
of that command, not correctness the command does not exercise.

The five-second performance requirement ends after fix planning. Preflight and
post-fix validation durations are displayed separately and may take as long as
the configured command requires.

## Rule Semantics

Each convention has its own stable switch. Rules may share a change, so one
import insertion can satisfy both type and enum-variant findings without
duplicating edits.

### Path qualification

The path rules inspect syntax position first and consult the project index where
identity is available. Rust permits unconventional identifier case, but Stylon's
syntax-only third-party policy treats an uppercase-leading segment in a type,
constructor, pattern, or associated-call position as a type. This naming-policy
assumption is what lets Stylon normalize third-party enum and type paths without
loading dependency APIs. A path that remains ambiguous under these syntax and
case rules is an analysis error rather than a finding.

A **qualifier** is a named path segment before the final called function or
variant, excluding a leading `::`. Exempt roots bypass the count completely.
Aliases count as the one segment they introduce, not as their expanded path.

#### `path.function-qualification`

A free-function call may be unqualified or have one module qualifier.

```rust
run();                    // allowed
runner::run();            // allowed
workflow::runner::run();  // fixed by importing `runner`
```

For associated calls, the type portion is handled independently by the type
rule. The function rule exempts syntactic `<T as Trait>::method` calls and the
common trait-associated names `default`, `from`, `try_from`, `from_str`, and
`from_iter`.

When several modules publicly expose a function, the fixer chooses the shortest
path accessible from the use site, then the lexicographically smallest full path
as a tie-breaker. It imports the immediate parent module. A module alias first
uses the module leaf, then prepends the shortest unique parent segments in
snake_case until it avoids type, value, macro, generic, and local-binding names.

#### `path.enum-variant-qualification`

An enum variant may be unqualified or qualified only by its enum type. The
normal fix imports the enum type and retains `Enum::Variant`.

```rust
battlement::UiValue::F32(value)  // becomes `UiValue::F32(value)`
UiValue::F32(value)              // allowed
```

`Some`, `None`, `Ok`, and `Err` are always exempt terminal variants.

#### `path.type-qualification`

Types are unqualified at use sites. The fix imports the type into the file's
top-level module and rewrites every affected type occurrence.

```rust
fn draw(card: cards::Card) {}  // becomes `fn draw(card: Card) {}`
```

If distinct types have the same leaf name, qualification is allowed only to the
shortest module suffix that distinguishes every type. Stylon shortens longer
paths instead of inventing aliases.

```rust
alpha::Card
beta::Card
```

The collision universe is the set of types actually referenced or planned for
import in the same file module, not every type in the workspace. For example,
`crate::ui::alpha::Card` and `crate::game::beta::Card` become the shown paths
after importing `crate::ui::alpha` and `crate::game::beta` as modules. Nested
generic arguments, struct patterns, tuple constructors, and qualified aliases
use the same type-path operation.

All three path rules exempt paths rooted in `std`, `core`, `alloc`, or
`proc_macro`, including imported aliases of those modules. Paths rooted in
`crate`, `self`, or `super` are also exempt from qualification checks. The
separate import rule still prohibits `use self::...` and `use super::...`.
Qualified-self associated types and method-call syntax such as `value.into()`
are exempt. A trait-method exemption never exempts a type embedded in the call;
for example, `battlement::PanelPoint::default()` becomes
`PanelPoint::default()` when the type rule is enabled.

Local function declarations and closures are unqualified values and never
receive module imports. Renamed imports retain their alias only when it does not
hide the selected module; otherwise all bound call sites receive the generated
module alias. Function pointers without call syntax are outside the
function-qualification rule, while a later call through the local pointer is
already unqualified.

### Imports

Import rules share a top-level import planner. It preserves attributes and
comments, splits grouped use trees when necessary, and chooses collision-free
module aliases deterministically.

#### `imports.public-function`

An unrestricted public function defined in the scanned project or standard
library may not be imported directly. The defining or exporting module is used
at the call site.

```rust
use crate::cards::shuffle;  // removed
shuffle(&mut deck);         // becomes `cards::shuffle(&mut deck)`
```

Restricted and private functions are outside this rule. Visibility-preserving
`pub use` and `pub(crate) use` declarations generated at a module boundary are
re-exports and are exempt. Glob imports from a known project module are
expanded when they would otherwise import a public function; referenced types,
traits, constants, and macros remain imported explicitly.

For a renamed function import, the alias is removed and every resolved call is
rewritten through the selected module. A glob is fixable only when each
unqualified reference has exactly one declaration among local items, explicit
imports, known glob exports, and the prelude. Ambiguity is an analysis error
before any glob finding is emitted.

Third-party public functions are outside this rule because their APIs are not
loaded. Calls to them still follow the function-qualification syntax rule.

#### `imports.absolute-crate-path`

Every `use self::...` and `use super::...` is rewritten to its absolute
`crate::...` path. This rule applies inside test modules as well as production
modules. If a file has no unambiguous module identity, Stylon reports an
analysis error before offering findings.

#### `imports.top-level`

`use` declarations are permitted only at a file module's top level. Imports in
functions are hoisted to that file module, retaining normalized `cfg`
conditions. Relative paths first resolve in the function's module and then
render as `crate::...` or external-crate paths. Conflicting leaves use the
shortest unique module alias described by the path rules, and bound uses in the
function are rewritten.

```rust
fn read() { use crate::alpha::io; io::load(); }
fn write() { use crate::beta::io; io::save(); }
// top-level aliases become `alpha_io` and `beta_io`; calls follow them
```

An ordinary inline module containing any `use`, including a re-export, is
extracted intact to a collision-free external module instead of hoisting the
declaration across an API boundary. Its uses are then file-top-level, while its
name, visibility, attributes, and relative resolution remain unchanged. This
structural extraction runs before import and path rewriting.

An inline module is test-exempt when its effective condition syntactically
requires `test`, including `cfg(test)` and `cfg(all(test, ...))`. A disjunction
such as `cfg(any(test, feature = "x"))` is not exempt because it can compile as
ordinary code. Literal `cfg_attr` is normalized before this decision. The
exemption applies only to import placement; direct-function and absolute
crate-path rules still apply. Top-level inline `mod tests` is separately
extracted by the test-layout rule.

Inside a nested module, `use super::card::Card` first resolves to the canonical
module and then becomes an absolute path such as
`use crate::game::card::Card`. The rewrite never merely substitutes the token
`super` with `crate`.

### `rustdoc.type-links`

Stylon parses Rustdoc Markdown link destinations that look like type names. A
short type link is valid when it resolves to a type defined in the item's
module, explicitly imported there, supplied as a generic parameter, or provided
by the Rust prelude.

```rust
/// Draws a [Card].     // valid if `Card` is local or imported
/// Draws a [Widget].   // becomes "Draws a `Widget`." if unresolved
```

Resolution follows the defining module, matching Rustdoc rather than a later
re-export scope. Qualified, labelled, and disambiguated intra-doc links are
parsed using Rustdoc's documented forms. When a type-like target is unresolved,
the fix removes link markup. Matching display text becomes a code span; custom
display text remains plain display text. Backticks inside an existing link do
not count as a fix because Rustdoc still treats that construct as a link.

Stylon uses a CommonMark event parser with byte offsets, then applies Rustdoc's
documented shortcut, reference, disambiguator, generic, and associated-item path
grammar. It checks method and impl generic parameters in their lexical scope.
Qualified external links are accepted when their first segment is an external
crate dependency; Stylon does not claim the target item exists. An unqualified
external type must be explicitly imported, matching the convention.

```rust
/// See [the widget][Widget].  // unresolved -> "See the widget."
/// See [`Widget`].            // unresolved -> "See `Widget`."
/// See [struct@Widget].       // checked in the type namespace
```

Reference definitions are updated or removed when their last use is repaired.
Primitive links and Rustdoc namespace disambiguators are recognized explicitly;
associated-type paths are exempt under the same rule as Rust source paths.

Ordinary URLs, reference labels that do not look like Rust paths, primitives,
function links, macro links, and links inside arbitrary macro tokens are outside
this rule.

### `modules.root-layout`

Every `lib.rs` and `mod.rs`, including a nested module's `mod.rs`, may contain
crate/module inner attributes, inner documentation, external module
declarations, and use or re-export declarations. Inline modules and every other
ordinary item must move to an external file.

Inline modules are extracted directly to external modules while preserving
their names, visibility, attributes, and public paths. Remaining ordinary items
move together to one private, collision-free module named from
`implementation`, adding a numeric suffix only when necessary.

For `lib.rs`, the destination is sibling `implementation.rs`. For a nested
`mod.rs`, it is `implementation.rs` inside that module directory. The search
then tries `implementation_2.rs`, `implementation_3.rs`, and so on, comparing
case-folded canonical paths. Stylon creates a directory only when extracting a
submodule from a `name.rs` file; root extraction itself does not choose
`implementation/mod.rs`.

The original root receives explicit re-exports grouped by original visibility.
Private root items gain only the minimum child-to-parent visibility required for
a private parent import. This preserves which descendants can name the item
without exposing formerly private APIs outside their original scope.

```rust
mod implementation;
pub use implementation::{Card, draw};
pub(crate) use implementation::DeckCache;
use implementation::reset_cache;
```

The implementation file retains original item order and receives imports needed
by moved code. Bare `pub` stays `pub`. Restricted visibility is translated to
the same canonical visibility scope; a relative `pub(in path)` is first
resolved, then re-rendered from the new module. A private item becomes
`pub(super)` only when the parent must import its old binding. The parent import
uses the original visibility, so descendants see exactly the old name and
scope. Re-exports sort by visibility and original source order, and Rust's type,
value, and macro namespaces are checked independently for collisions.

```rust
// Before, in lib.rs: `pub struct Card; fn reset() {}`
// After, in implementation.rs:
pub struct Card;
pub(super) fn reset() {}
// lib.rs contains `pub use implementation::Card; use implementation::reset;`
```

Item attributes and relevant `cfg` conditions are copied to matching
re-exports. Imports needed by moved code move with it. References from sibling
modules are rewritten through the preserved root binding or the shortest valid
module path.

Procedural-macro crates have a required exception. Functions bearing
`#[proc_macro]`, `#[proc_macro_attribute]`, or `#[proc_macro_derive]` may remain
in a proc-macro crate's `lib.rs`; their helpers must still move. Crate-level
attributes are also retained. No other crate kind receives this exception.

Macro definitions move with the implementation while retaining relative order.
`#[macro_export]` continues to export at the crate root. A non-exported macro
that must remain visible through the old root receives the narrowest valid macro
re-export. Post-fix Cargo validation is authoritative for macro scoping that
syntax-only analysis cannot prove.

`macro_rules!` declarations are known macro definitions for extraction but are
opaque ordering barriers unless listed as constant-defining. Their bodies are
never rewritten. Literal relative paths in moved attributes and include macros
are recalculated to keep the same canonical target. Compatibility covers Rust
and Cargo paths plus validated compilation; arbitrary tools that depend on
physical source filenames are outside the contract.

### Test layout

Test placement has separate switches because projects may adopt filename and
inline-module conventions independently.

#### `tests.no-inline-module`

A top-level inline module named `tests` is prohibited. The fix moves its body to
an external file ending in `_tests.rs` and leaves an external declaration with
the same module name. A `#[path = "..."]` attribute preserves the name when the
required filename differs from Rust's conventional lookup path.

For `foo.rs`, the destination is sibling `foo_tests.rs`; for `foo/mod.rs`, it
is `foo/foo_tests.rs`; and for a crate root it is sibling
`crate_root_tests.rs`. The retained declaration is:

```rust
#[cfg(test)]
#[path = "foo_tests.rs"]
mod tests;
```

The path string is always relative to the declaring file under Rust's path
attribute rules. Multiple conditional inline `mod tests` declarations are
merged only when their conditions are disjoint and their combined external
module parses; an existing external declaration or destination is otherwise a
planning conflict.

Nested modules and their visibility remain unchanged. Imports are re-evaluated
after extraction, so `use super::*` is replaced through the ordinary import
rules rather than copied as a permanent exception.

#### `tests.file-suffix`

A Rust file containing a test function must end in `_tests.rs`. A test function
is one bearing built-in `#[test]`, an attribute whose final path segment is
`test`, or a configured recognized test attribute. Stylon also recognizes the
common `rstest` and `test_case` attributes. Doctests and macro-generated tests
are outside the filename rule.

Attribute recognition is syntactic across all `cfg` alternatives. `cfg_attr`
counts only when its literal expansion contains a recognized test attribute.
Matching a final `test` segment is intentional framework support; a project
with a non-test attribute of that form must disable this rule for the directory
because source-level suppression is unavailable.

The fix moves the file and updates every known structural reference:

- external module declarations and `#[path]` attributes;
- explicit Cargo target paths;
- literal Rust include macros; and
- generated declarations from inline-test extraction.

For an integration test, Stylon preserves the old Cargo test target name with
an explicit `[[test]]` entry pointing to the new `_tests.rs` path. Existing
scripts using `cargo test --test old_name` therefore continue to work. A target
or destination collision is a pre-write planning error.

Special Cargo entry files are not renamed. If `lib.rs`, `main.rs`, `build.rs`, a
`src/bin` root, example root, or benchmark root directly contains test
functions, Stylon moves those functions into a sibling external module ending
in `_tests.rs` and leaves a conditional module declaration. Referenced private
helpers remain reachable through absolute crate/module paths. An ordinary
module file is renamed and its parent receives a literal `#[path]` preserving
the original module name.

```text
tests/gameplay.rs -> tests/gameplay_tests.rs
Cargo.toml adds: [[test]] name = "gameplay" path = "tests/gameplay_tests.rs"
src/parser.rs -> src/parser_tests.rs
parent adds: #[path = "parser_tests.rs"] mod parser;
```

When a moved file contains a literal relative include, Stylon rewrites the
literal so its canonical target remains unchanged. References in arbitrary
scripts, documentation, or macro-generated strings are outside structural
discovery; post-fix validation may detect them, but the documented compatibility
contract covers Cargo, Rust module declarations, and literal include macros.

### Cargo dependencies

Cargo edits use a lossless TOML editor so comments, unrelated tables, quoting,
and formatting survive. A dependency entry moves with its directly attached
comments.

#### `cargo.dependency-order`

The rule applies to `[dependencies]` and `[workspace.dependencies]`. It does not
reorder dev, build, or target-specific dependency tables.

Entries containing `path` come first and are sorted by dependency key. All
remaining entries follow, also sorted by dependency key. A path dependency is
internal for ordering even when its canonical destination is outside the
workspace. Cargo dependency keys are ASCII; Stylon compares decoded literal
keys bytewise without treating `-` and `_` as equal. Feature arrays retain
their order and duplicate spelling because this rule moves whole entries.

#### `cargo.workspace-inheritance`

Every external dependency in `[dependencies]` of a non-root workspace member
must use `workspace = true`. **External** means an entry without `path`,
including registry, Git, and renamed packages. Path dependencies are exempt.
The workspace root package is exempt because it owns the shared policy and may
also need a package-specific direct dependency.

When the workspace catalog already contains the dependency, Stylon removes the
member's source/version policy and retains member-only `features` and `optional`
settings. Cargo defines inherited features as additive.

When the catalog lacks the dependency, Stylon promotes version, registry or Git
source, package alias, and `default-features` policy to
`[workspace.dependencies]`. Every feature list and `optional` flag remains in
its member entry, so promotion does not add a feature to other members.

Before emitting findings, Stylon groups specifications by literal dependency
key and resolved package identity. Promotion is compatible only when normalized
version requirement, source URL and revision, package alias, and
`default-features` value are exactly equal. Semver intersection is not enough
because it changes accepted future versions. An existing workspace entry must
match those same fields. Distinct literal keys intentionally aliasing one
package remain distinct workspace entries.

Any mismatch is an operational conflict. Stylon writes nothing and identifies
every conflicting manifest rather than choosing a version or rewriting Rust
crate names. Omitted and explicit `default-features = true` normalize to Cargo's
same default before comparison.

```toml
# Workspace root, after promotion
[workspace.dependencies]
rayon = "1.12.0"

# Member, after promotion
[dependencies]
rayon = { workspace = true, optional = true, features = ["web_spin_lock"] }
```

Dev, build, and target-specific tables are intentionally excluded because the
agreed convention names only normal member dependencies. They retain their
existing order and inheritance policy.

### Item layout

Item ordering and spacing operate on complete syntax nodes. Leading outer
attributes, Rustdoc, and contiguous explanatory comments move with the following
item; same-line trailing comments move with the preceding item.

#### `items.order`

Within each run not interrupted by an opaque macro barrier, top-level items use
this order:

1. private constants;
2. private statics;
3. `thread_local!` and configured constant-defining macros;
4. unrestricted public type aliases;
5. unrestricted public constants and statics;
6. unrestricted public traits;
7. unrestricted public structs, enums, and unions;
8. unrestricted public functions;
9. restricted-visibility type aliases;
10. restricted-visibility constants and statics;
11. restricted-visibility traits;
12. restricted-visibility structs, enums, and unions;
13. restricted-visibility functions;
14. all remaining private and uncategorized ordinary items; and
15. test module declarations.

Restricted visibility includes `pub(crate)`, `pub(super)`, `pub(self)`, and
`pub(in path)`. Items keep their relative order within one category. Stylon
collects the source ranges of orderable items in one macro-bounded run, stable
sorts their complete text, and writes the results back into those same item
slots. Imports and ordinary external module declarations therefore stay at
their byte positions while ordinary items may exchange slots on either side.

A test module declaration is an external module whose normalized effective
condition requires `test`, using the same compound-`cfg` rules as import
placement, or the declaration produced by inline-test extraction. Unknown
macro invocations, `macro_rules!` definitions, global assembly, and foreign
macro item families are barriers. Ordinary impls, extern blocks, and foreign
modules are uncategorized private items. No finding compares items across a
barrier.

#### `items.blank-lines`

At least one empty physical line separates adjacent orderable code items.
Consecutive `const` declarations are the only spacing exception. Consecutive
imports and ordinary external module declarations may remain compact because
they are not orderable code items.

Adjacent means no import, ordinary module declaration, or macro barrier lies
physically between the two item ranges. An empty line contains no characters or
only spaces/tabs before LF or CRLF. The fix inserts a missing blank line and
never removes additional blank lines. Attributes, documentation, and comments
without an intervening blank line remain attached to their item. A freestanding
comment group separated by blank lines stays in its original slot and prevents
the two surrounding code items from being spacing-adjacent.

```rust
use crate::Card;
pub struct Deck; // imports stay fixed while ordered item slots are rewritten

opaque_macro!(); // ordering barrier
fn helper() {}
```

## Failure Handling and Observability

Stylon distinguishes style violations from states in which it cannot prove or
apply the style safely. This keeps exit status and the universal fix promise
honest.

Operational error categories include:

- invalid or conflicting configuration;
- unreadable, non-UTF-8, or concurrently modified selected files;
- Rust or TOML parse errors;
- ambiguous Cargo or module ownership;
- incompatible workspace dependency specifications;
- colliding file-move destinations;
- incompatible overlapping edits or a non-converging fix cycle;
- failed baseline or post-fix validation;
- interrupted recovery with externally modified targets; and
- internal panics isolated to a file or rule.

Check mode continues gathering independent diagnostics after a local parse or
rule error, then exits `2` and returns both diagnostics and errors. Fix mode
plans nothing after an operational error and never writes a partial subset.

`--timings` provides duration and count data for each stage, including files,
bytes, syntax nodes, findings per rule, planned edits, Cargo metadata calls, and
validation duration. Human timing output goes to standard error. JSON includes
the same data in the summary without changing diagnostic ordering.

No source text, file contents, absolute home path, or environment variable is
included in timing output.

## Performance Contract

Performance is a release-blocking acceptance criterion. The reference machine
is an Apple M5 Max MacBook Pro with 18 cores and 64 GB of memory. The corpus is
Battlement commit `725660cccf08e66d2151ad5c5566fc4245e7070d`, scanned from its
repository root with its ignore rules and no `stylon.toml` overrides.

The release binary must satisfy all of these conditions:

- A full check-mode scan has p95 wall time at or below 5.0 seconds over 20
  consecutive measured runs.
- No persistent Stylon cache exists between or within the runs; every selected
  file is parsed and indexed each time.
- `--fix` scanning and in-memory planning obey the same 5.0-second boundary on
  an already compliant disposable corpus. Cargo preflight and post-fix
  validation are excluded and reported separately.
- Human output is redirected during timing so terminal rendering is not the
  bottleneck. A separate JSON benchmark confirms serialization remains inside
  the same budget.

The benchmark harness records the exact macOS build, hardware identifier, power
mode, Rust version, Stylon binary hash, Battlement revision, configuration hash,
file count, byte count, and command line. It performs three unmeasured warm-ups,
then 20 isolated runs with no other repository commands active. Filesystem cache
may be warm, but every run starts a new Stylon process and rebuilds all Stylon
analysis state. The nearest-rank p95 is the 19th value after sorting 20 wall
times. Process startup, configuration, discovery, Cargo metadata, parsing,
planning, sorting, and serialization are inside the measured interval.

The fix-planning benchmark uses the disposable Battlement tree produced and
validated by the end-to-end fix test, so it is reproducibly compliant. Its
validation command is replaced by a recorded successful no-op only for timing;
the separate end-to-end test uses
`cargo check --workspace --all-targets --all-features --locked` when the pinned
workspace supports it.

The implementation must preserve the following performance properties:

- file discovery and parsing use bounded parallel workers;
- each source file is read once and parsed once in check mode;
- per-file workers produce compact facts without shared locks;
- the project index is merged once, then read concurrently;
- rules declare interests so irrelevant files and facts are skipped;
- dependency source, compiler metadata, macro expansion, and type inference are
  never loaded; and
- output is sorted once after parallel evaluation.

Phase timing is part of every benchmark artifact. A regression is investigated
at the responsible phase rather than relaxing the five-second total.

## Battlement Stress Audit

Battlement is deliberately used because it is larger than the target and
contains workspaces, sample crates, fixtures, proc macros, extensive tests,
macros, and generated Unity trees. Git-aware discovery avoids its ignored
multi-gigabyte Unity output while retaining tracked Rust samples.

The pre-implementation audit used lexical and structural searches rather than
Stylon's future parser. Counts are therefore candidate lower or upper bounds,
not promised diagnostic totals:

- 469 tracked Rust files contain 138,483 lines.
- Approximately 1,138 non-standard multi-segment path occurrences are
  candidates for qualification checks.
- `crates/battlement-fake/src/client/ui.rs` alone has 205 qualified type or
  variant candidates; another Reactant test has 53.
- 29 imports begin with `self::` or `super::`.
- At least 23 `lib.rs` or `mod.rs` files contain top-level ordinary code.
- 19 production/source files contain top-level inline `mod tests` modules.
- Approximately 95 files with test attributes do not end in `_tests.rs`.
- 12 external dependencies in actual workspace members do not inherit from the
  workspace catalog.
- A column-zero approximation found 275 adjacent item-category inversions in
  143 files.
- Approximately 128 top-level item boundaries lack a blank line.
- The audit found no dependency-order violation and no obvious unresolved
  type-like Rustdoc link.

These concentrations influenced the design directly:

- Root extraction must preserve APIs and exempt proc-macro entry points.
- Test fixes must preserve Cargo target names and module paths rather than only
  renaming files.
- Ordering must stop at unknown macro barriers because Battlement contains many
  macro-generated item families.
- Path fixes need one shared import planner because a single file may contain
  hundreds of overlapping type and variant rewrites.
- Workspace promotion must retain optional and feature settings because several
  Battlement members use them.

The completed Stylon scanner replaces the lexical audit with a checked-in JSON
snapshot tied to the pinned revision. Each finding is reviewed once as an
intended convention violation or exposes a rule bug that must be corrected.
The approved snapshot is not used to weaken rules through Battlement-specific
exemptions.

## Automated Validation

Tests concentrate on observable rule behavior and transaction safety rather
than private helper structure.

Every rule has table-driven fixtures covering:

- valid source, one minimal violation, and multiple violations;
- every documented exemption and configuration-disabled behavior;
- exact human and JSON spans;
- expected fixed source;
- a clean second scan; and
- a second `--fix` producing no byte changes.

Cross-rule fixtures cover shared and conflicting edits, including:

- type and variant findings that need the same import;
- hoisted imports followed by direct-function rewriting;
- test extraction followed by absolute crate import rewriting;
- module extraction followed by item ordering and Rustdoc resolution;
- path collisions requiring shortest unique qualification; and
- comments and `cfg` attributes moving with their items.

Black-box Cargo fixtures validate compile-sensitive behavior:

- library root extraction preserves public, restricted, and private access;
- exported and private `macro_rules!` definitions retain required scope;
- proc-macro entry points remain at the crate root while helpers move;
- explicit and auto-discovered integration tests retain their old target names;
- alternative `cfg` declarations agree or produce a planning error;
- workspace dependency promotion preserves features and optionality; and
- incompatible dependency sources fail before any file changes.

Transaction tests hash every source before and after forced preflight failure,
post-fix failure, interrupt, simulated process death, and recovery. They verify
that modes, names, contents, user changes, and absent files are restored
exactly. A concurrent third-state edit must block automatic recovery.

The pinned Battlement test runs against a disposable copy:

1. Confirm the exact audit snapshot and record per-rule counts.
2. Confirm check mode completes inside the performance contract.
3. Run `--fix` with Battlement's comprehensive validation command.
4. Confirm validation succeeds and public tests retain their target names.
5. Run Stylon again and require zero enabled findings.
6. Run `--fix` again and require an empty filesystem diff.

Parser and dependency upgrades must pass this complete suite. A syntax API
upgrade is not accepted solely because Stylon compiles.

## Manual QA

Manual QA uses a disposable copy of pinned Battlement plus small purpose-built
Cargo workspaces. Never point destructive failure scenarios at the developer's
only checkout.

- Run `stylon` at the Battlement root. Confirm compiler-style paths are
  relative, rule IDs are present, the observed concentrations match the
  approved audit, and ignored Unity and worktree trees are absent.
- Repeat with one explicit file, one directory, an untracked file, and a
  non-Git fixture. Confirm only selected files receive findings while required
  manifests and parent modules are context. Confirm an explicitly named ignored
  file is scanned and a configured exclusion still wins.
- Exercise ancestor discovery, `--config`, nested configs, invalid versions,
  unknown keys, invalid globs, and two matching overrides. Confirm the last
  override wins only in the valid case and every invalid case exits `2`.
- Run with `--format json`, validate schema version 1, and compare finding order
  with human output. Repeat the command and confirm byte-identical JSON after
  removing timing fields.
- Trigger exits `0`, `1`, and `2` in both formats. Confirm UTF-8 half-open byte
  ranges, Unicode-scalar columns, sorted errors, empty JSON-mode standard error,
  captured validation output, and timing output without private paths.
- Add a root `stylon.toml` that disables only `tests.file-suffix` under one
  crate. Confirm inline-test findings remain there, filename findings disappear
  only under the matching path, and deleting the config restores defaults.
- Add `// stylon-ignore` and `#[allow(stylon)]` beside a violation. Confirm the
  finding remains and no special suppression behavior appears.
- In a disposable clean Battlement copy, run `stylon --fix`. Inspect the
  generated implementation modules, visibility-matched re-exports, extracted
  test modules, preserved Cargo test names, promoted dependencies, and moved
  comments. Confirm both Cargo validations pass.
- Run check mode and fix mode again. Confirm zero findings and no changed bytes.
- Configure a validation command that exits unsuccessfully after the preflight
  succeeds. Confirm Stylon restores the exact original hashes, modes, names,
  and untracked-file state.
- Fail the initial preflight and confirm no journal or source edit appears. Run
  two Stylon fixers concurrently and confirm lock contention. Modify an input
  from an editor after planning and confirm the hash check aborts safely.
- Use a validator that spawns a long-running child, then interrupt Stylon.
  Confirm the complete process group is gone before file restoration begins and
  no descendant rewrites a restored path.
- Terminate Stylon after writes but before validation finishes. Restart it and
  confirm automatic recovery. Then repeat while manually changing one affected
  file and confirm recovery stops without overwriting that third state.
- Interrupt after each individual rename in a multi-file transaction and while
  creating directories. Confirm reverse replay handles every partial state.
  Make validation create or modify `Cargo.lock` and confirm original lockfile
  presence and bytes are restored as specified.
- Create two member manifests with incompatible specifications for one external
  dependency. Confirm Stylon reports both paths, exits `2`, and writes nothing.
- Create a proc-macro fixture with helpers in `lib.rs`. Confirm only the three
  permitted proc-macro entry-point forms remain at the root and the resulting
  crate compiles.
- Create same-named types in two modules. Confirm the fixer uses the shortest
  unique qualification and does not generate aliases.
- Exercise free and associated calls, enum patterns, nested generic types,
  renamed and glob function imports, standard-library module aliases, nested
  `super` imports, conflicting function-local imports, and an ambiguous
  third-party path. Confirm intended fixes and the required analysis error.
- Exercise shortcut, labelled, qualified, backticked, disambiguated, generic,
  primitive, and unresolved Rustdoc links. Confirm only unresolved type links
  lose navigation and custom display text remains readable.
- Exercise item ordering with fixed imports, restricted visibility, comments,
  compound test conditions, known constant macros, opaque barriers, CRLF, and
  missing blank lines. Confirm stable order and exact trivia ownership.
- Exercise dependency ordering separately from inheritance. Confirm path-first
  grouping, exact compatibility checks, member-only features, existing catalog
  conflicts, renamed packages, and untouched dev/build/target tables.
- Apply test suffix fixes to ordinary modules, integration tests, `lib.rs`,
  `main.rs`, examples, benchmarks, and build scripts. Confirm Cargo target and
  Rust module names stay stable and every generated path is relative correctly.
- Run the release benchmark for 20 measured Battlement scans on the reference
  Mac. Confirm p95 is at most 5.0 seconds and inspect phase timings for any run
  approaching the limit.
