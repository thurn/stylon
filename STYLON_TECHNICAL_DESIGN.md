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
  141,784 tracked Rust lines at the pinned revision.
- Every style finding must have a deterministic machine-applicable fix.
  Conditions that prevent a safe fix are operational errors, not unfixable
  style findings.
- Adding a rule must require a small, isolated Rust implementation rather than
  changes throughout the scanner or fixer.
- Rules may be enabled or disabled for the project or selected directories only
  through one root `stylon.toml`. Source annotations and line-level suppression
  comments have no effect.

Stylon parses source directly and builds only the facts its rules need; normal
scans do not invoke `rustc` or the rust-analyzer semantic engine. Initial
measurements support the five-second target, but a complete scanner and fixer
must pass the benchmarks before this is a verified product claim. Parsing and a
clean scan do not prove semantic equivalence: fixes also require conservative
resolution checks and the configured validation command.

Rust 1.98 is the initial minimum supported Rust version for building Stylon,
matching `ra_ap_syntax` 0.0.350. Commit a tested dependency lockfile; pinning the
parser alone does not pin its transitive dependencies. Analyzed projects may use
older editions and toolchains.

## Implementation Readiness

The design is ready for a staged implementation, not an unconditional promise
of automatic repair for arbitrary Rust. The remaining gates are explicit:

1. Build discovery, parsing, the diagnostic model, and the two item-layout rules
   first. Measure the complete scan on pinned Battlement before adding a resolver.
2. Add Cargo edits and a minimal, conservative module/import resolver. Fixture
   tests must cover shadowing, re-exports, conditional modules, macro use, and
   references used as values before enabling name-changing fixes.
3. Add test-file changes and import/path fixes through the same planner. Prove
   module and Cargo target identities survive each structural edit. Keep
   unsupported cases as operational errors; do not claim a compiler-equivalent
   resolver or silently guess.
4. Complete crash recovery, failing-validation tests, and the dirty-corpus fix
   benchmark before shipping `--fix`.

Ordinary items may remain in `lib.rs` and `mod.rs`. There is no special root
layout convention, implementation-module extraction, visibility widening, or
root re-export synthesis. Test layout and import placement retain their
separately documented rules.

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
[cargo-metadata]: https://doc.rust-lang.org/cargo/commands/cargo-metadata.html
[cargo-targets]: https://doc.rust-lang.org/cargo/reference/cargo-targets.html
[rust-modules]: https://doc.rust-lang.org/reference/items/modules.html
[rust-macros]: https://doc.rust-lang.org/reference/macros-by-example.html
[rustdoc-json]: https://doc.rust-lang.org/nightly/rustdoc/unstable-features.html

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
  "summary": {"files": 480, "findings": 1, "fixed": 0, "remaining": 1}
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

Stylon obtains package and target identity directly from Cargo; it does not
reimplement workspace membership or create a synthetic source tree. Canonicalize
manifests, query the root workspace first, and skip member manifests already
returned by that query. Query each remaining independent package/workspace once,
from its manifest directory:

```text
cargo metadata --no-deps --format-version 1 --frozen --manifest-path ABSOLUTE_PATH
```

[`--no-deps`][cargo-metadata] omits dependency resolution; `--frozen` prohibits
network access and lockfile changes. The readiness probe succeeded both on the
pinned corpus and on a package with no lockfile and an unavailable dependency.
Keep these as regression fixtures for supported Cargo versions. A Cargo failure
is an operational error, never a reason to retry with network or lockfile writes
enabled. There is no metadata sandbox or dependency-source scan.

Metadata identifies workspace members, target names, entry files, editions, and
proc-macro crates. External path dependencies may be recorded as dependencies,
but source outside the analysis root is not indexed or rewritten. Metadata calls
and discovery of independent sample/fixture workspaces count toward scan time.

Module identity is built by starting at every Cargo target root and recursively
following external `mod` declarations and literal `#[path]` attributes. A
declaration without `#[path]` considers exactly Rust's two conventional
candidates: `name.rs` and `name/mod.rs` relative to its module directory.
Two existing candidates are ambiguous. A missing conditional module can refer
to generated or platform-specific source; record it as unresolved and report an
error only when an enabled fix needs that identity. An unconditional missing
selected module is an error. Follow the [Rust module lookup rules][rust-modules],
including inline-module directories and `#[path]`, rather than treating all paths
as relative to the physical file. A file may belong to multiple targets; retain
all identities and require a proposed edit to agree in every context.
Literal `include!` source is parsed in its inclusion context and remains a
distinct physical file; unsupported expression/type fragments are opaque, not
incorrectly parsed as complete source files.

An unattached file receives self-contained syntax rules only. Its filename is not proof
of a Rust module identity. A rule requiring module, visibility, or Cargo identity
reports an analysis error for that file rather than guessing. Item spacing and ordering can operate locally. Any import/path fix requiring
module identity must fail analysis rather than invent one.

Conditional declarations are retained as alternatives with their `cfg` and
literal `cfg_attr` predicates. Use conservative structural checks, not a general
Boolean solver; an unproved relationship remains unknown. Stylon
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

Stylon pins `ra_ap_syntax = "=0.0.350"` and commits `Cargo.lock`. The readiness
probe required `unicode-ident = 1.0.24` in the lockfile: 1.0.26 uses Unicode 18,
while the lexer's `unicode-properties` 0.1.4 uses Unicode 17 and rejects that
combination at compile time. The rust-analyzer syntax API is not a
stable compatibility boundary; upgrades require the parser and Battlement tests.

The parser retains comments, whitespace, attributes, and exact byte ranges.
Each worker parses a file and extracts compact, owned facts in one walk. Keep
red syntax nodes local to the worker; only owned facts and shareable immutable
green trees cross worker boundaries. A rule worker reconstructs its local tree
view without reparsing. Facts include:

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
Rustdoc JSON generation requires [unstable rustdoc support][rustdoc-json]; the
implementation must pin a compatible generator toolchain and matching standard
library source, check in the generated artifact and command, and test its
contents before enabling standard-library function-import checks. This is a
build-time gate, not a claim that stable Rust 1.94 can emit JSON unaided. Runtime
analysis does not scan the sysroot. APIs absent from the embedded version are
unknown, not guessed from capitalization. Standard path aliases are resolved
through imports, so `fs::File` inherits the exemption of `std::fs::File`.

Third-party dependency APIs are not indexed. Direct-function-import checking
covers project and standard-library public functions. Syntax-position rules may normalize unambiguous third-party type paths, but
variant and associated-call classification must respect the resolution boundary.

### Macro boundary

Arbitrary macro token trees are opaque. Stylon may inspect a macro's path,
attributes, delimiter range, and top-level position, but it never interprets or
rewrites Rust-looking tokens inside the invocation.

Unknown top-level macro declarations and invocations are ordering barriers,
because [macro textual scope][rust-macros] depends on source order.
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
or manifest operations plus hashes and resolution preconditions. A **known public path** is a bare-`pub` path reachable from a
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
because a test extraction can create imports that another rule must rewrite,
and two independently correct edits can overlap.

Planning uses a virtual filesystem initialized from the immutable scan. It
applies rule changes in a deterministic priority order:

1. manifest and filesystem structure changes;
2. inline-module/test extraction or file renaming;
3. import and path rewrites;
4. item ordering; and
5. blank-line normalization.

Within a priority, rules sort by rule ID and findings sort by normalized path
and source offset. Identical replacements and operations carrying the same
shared-planner key are merged. Any other overlapping byte ranges, moves, or
manifest fields conflict.

Each priority stage consumes fresh ranges from the current virtual snapshot;
edits within a file apply from highest byte offset to lowest. Reparse changed
files between stages before running the next stage. Never apply original-scan
ranges after an earlier stage moved or rewrote text. After each pass, changed
virtual files are reparsed as needed. Stylon conservatively
rebuilds the module, import, symbol, test, and manifest facts for every Cargo
target touched by the change. Destination-path configuration and rule interests
are recomputed. Planning ends only when no enabled findings remain.

A virtual state hash covers every path, file byte string, file mode, manifest
model, and effective rule policy. Repeating a hash is a cycle. More than eight
passes is a `non-converging-fix` error. This is a defensive limit, not a proof
that arbitrary rule combinations converge.

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

Use one transaction journal and one project lock. The state sequence is:

```text
plan -> lock and recheck inputs -> preflight journal -> baseline validation
     -> extend journal -> replace -> validate -> commit or restore
```

The validator must leave project inputs unchanged; generated build output is
outside this contract. Before baseline validation, retain a durable inventory
of selected files, context inputs, manifests, configuration, and lockfiles so
an interrupted or misbehaving validator cannot silently change those inputs.
This inventory is the preflight phase of the same journal, not a second
transaction system. A failing baseline stops all source edits. Lockfile creation
or mutation by validation is a failure and is reported with recovery data.
Projects without a lockfile must prepare validation separately before fixing.

The lock coordinates Stylon processes. Recheck hashes of all analysis inputs
under the lock and immediately before replacement; an editor changing those
inputs invalidates the plan. The user must not edit the project during a fix.
Preserve dirty and untracked input bytes; never use Git reset. Validators must
not modify project inputs or detach child processes.

Before writes, durably record each affected path's original bytes or absence,
mode, replacement hash or absence, and any created directories. Record intent
before each rename and completion afterward, so recovery also handles a crash
between those events. Flush backup data and journal intent before replacement,
and flush target directories before recording completion. Each replacement uses
an atomic rename on the target filesystem. Multi-file visibility is not atomic.
Keep recovery files private to the current user and originals until commit.

Post-fix validation success durably marks the transaction committed before
cleanup. Failure or interruption restores originals after terminating the
validator's process group (Unix) or Job Object (Windows). Allow five seconds for
graceful termination, then force termination. Startup recovery must verify an
owned validator is stopped before touching files; if ownership or termination
cannot be established, stop with recovery instructions.

Startup recovery acquires the lock and inspects unfinished journal entries,
including incomplete renames. Restore paths only when they still match a known
original or replacement state. An unknown third state is never overwritten;
report the path and retained backup for manual recovery. This also applies to
unexpected validator mutations: do not claim to distinguish those from editor
changes. A committed journal requires cleanup only. Check mode detecting an
unfinished transaction exits `2` with instructions to run `--fix` for recovery;
it never changes source files automatically.

The preservation contract covers bytes, executable modes, filenames, created
directories, and original absence. Symlinks and hard links are rejected;
ownership, ACLs, extended attributes, and timestamps are outside the contract.
Fault-injection tests must cover every durable state transition before release.

Validation proves only that the configured command succeeds. `cargo check` does
not run tests or prove runtime behavior, macro equivalence, or correctness on
other targets. Projects needing those guarantees configure a command covering
them. Baseline validation, disk writes, durability operations, and post-fix
validation are outside the five-second scan/planning budget and timed separately.

## Rule Semantics

Each convention has its own stable switch. Rules may share a change, so one
import insertion can satisfy both type and enum-variant findings without
duplicating edits.

### Path qualification

Path rules inspect syntax position and consult the project index when identity
is needed. Capitalization is not proof that a segment is a type, enum, or module.
An explicit type position can establish that a full path denotes a type;
expression/pattern/associated-call classification requires a known declaration.
Unresolved third-party paths in those positions are analysis errors only when
classification is necessary to decide or construct an enabled fix. Preserve the
strict error policy rather than reporting speculative machine-applicable fixes.

The resolver supports explicit lexical bindings, imports, workspace declarations,
and known standard-library entries. It tracks type/value/macro namespaces and
local shadowing. Glob expansion is allowed only for a completely known export
set; traits imported solely for method lookup must not be dropped. Unknown macro
expansion, procedural attributes, or third-party exports may prevent proof. A
successful `cargo check` cannot substitute for binding identity: changed code can
compile while selecting a different function or trait implementation.

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
use the same type-path operation. Check every affected lexical scope for generic
parameters, local bindings, and existing imports that would shadow the new name;
retain a distinguishing qualifier or fail planning if identity cannot be preserved.

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

Restricted and private functions are outside this rule. All re-export declarations (`pub use` and `pub(...) use`) are exempt;
rewriting a public API is outside this rule. Glob imports from a known project module are
expanded when they would otherwise import a public function; referenced types,
traits, constants, and macros remain imported explicitly.

For a renamed function import, the alias is removed and every resolved value
reference is rewritten through the selected module, including function pointers,
callback arguments, and calls. Block-local shadowing must be distinguished from
the imported binding. An import referenced inside opaque macro tokens cannot be
safely removed or renamed; report an analysis error for that proposed fix. A glob is fixable only when each
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

Compute the path using Rust's module-directory and `#[path]` rules, including
inline ancestors. Preserve the original declaration's attributes and condition;
the example assumes the original was `#[cfg(test)]`. Do not add `cfg(test)` to
an unconditional module. Keep conditional declarations separate in uniquely
named `_tests.rs` files rather than merging bodies with potentially duplicate
items. An existing destination is a planning conflict.

Nested modules and their visibility remain unchanged. Imports are re-evaluated
after extraction, so `use super::*` is replaced through the ordinary import
rules rather than copied as a permanent exception.

#### `tests.file-suffix`

A Rust file directly containing a test function must end in `_tests.rs`.
Functions inside an inline module belong to that module for this rule; they do
not also force a rename of the enclosing production file. Inline `tests`
extraction is handled by the separate rule, avoiding rename/extraction cycles. A test function
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

Snapshot the complete target inventory and verify that the explicit path/name
entry preserves it after the move, including `harness` and `required-features`.
A Cargo 1.98.1 probe confirms that an explicit target path suppresses a duplicate
auto-discovered target for that file; retain this regression fixture. Keep
`autotests` unchanged. Target names and paths follow [Cargo's rules][cargo-targets].

Special Cargo entry files are not renamed. If `lib.rs`, `main.rs`, `build.rs`, a
`src/bin` root, example root, or benchmark root directly contains test
functions, Stylon moves those functions into a sibling external module ending
in `_tests.rs` only when their condition implies `test` and all references can
be preserved. Retain imports needed for trait lookup and original attributes.
A test-like framework attribute alone does not justify hiding a function behind
`cfg(test)`; uncertain cases are analysis errors. Referenced private helpers
remain reachable through absolute crate/module paths. An ordinary
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
  the original corpus with violations, including all virtual passes and the
  final clean scan. An already compliant corpus is a separate no-op benchmark.
  Validation and filesystem writes are excluded and reported separately.
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

The fix-planning benchmark invokes the production in-memory planner on a fresh
snapshot of the original corpus for each run. No no-op validator is necessary:
the timed interval ends before lock acquisition and validation. A planning error
or non-convergence fails acceptance, even if fast. The separate end-to-end fix
test performs actual writes and runs the real configured validator. If Battlement
needs exclusions or another validation command, document and review that
configuration explicitly; a corpus that exits `2` is not a passing release run.

### Feasibility evidence

A local component probe on the reference M5 Max (Mac17,6, 18 cores, 64 GiB),
macOS 26.5.2 (25F84), Rust/Cargo 1.98.1, measured the following with three warm-ups
and 20 fresh-process runs:

| Component | Work | Median | p95 |
| --- | --- | --- | --- |
| Read, parse, walk syntax | 480 files, 4,293,719 bytes, 1,013,539 nodes | 0.023 s | 0.026 s |
| Cargo metadata | Seven workspace/package invocations, sequential | 0.121 s | 0.132 s |

All source files parsed without errors. These component costs make a five-second
scan plausible with substantial headroom. They are not an end-to-end measurement:
the probe excludes Git-aware traversal of the real checkout, retained project
facts, name resolution, rules, diagnostics, and iterative planning. Separate
component p95 values must not be presented as a measured total. Other desktop
activity was not controlled. See [raw results](benchmarks/readiness/results.json)
and the [reproducible probe](benchmarks/readiness/run.py), with its locked parser
dependencies, for exact samples and limitations.

Five seconds is verified only when the full acceptance run passes. LOC alone
cannot bound arbitrary Rust workloads: record source bytes, file count, findings,
and metadata invocation count as well. Cold filesystem caches and projects with
far more independent workspaces are outside this reference-corpus guarantee.
The original clean-tree fix benchmark offered no evidence for dirty-tree planning;
that remains the main unmeasured performance goal.

The implementation must preserve the following performance properties:

- file discovery and parsing use bounded parallel workers;
- each source file is read once and parsed once in check mode;
- per-file workers produce compact facts without shared locks;
- the project index is merged once, then read concurrently;
- rules declare interests so irrelevant files and facts are skipped;
- dependency source, compiler metadata, macro expansion, and type inference are
  never loaded; and
- output is sorted once after parallel evaluation; and
- name/export lookups use indexed maps with cycle detection, avoiding repeated
  full-workspace searches or enumeration of every `cfg` combination.

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

- A fresh tracked-file count found 480 Rust files and 141,784 lines. The earlier
  469-file / 138,483-line count is superseded; rule candidate counts below remain
  historical estimates requiring a fresh scanner audit.
- Approximately 1,138 non-standard multi-segment path occurrences are
  candidates for qualification checks.
- `crates/battlement-fake/src/client/ui.rs` alone has 205 qualified type or
  variant candidates; another Reactant test has 53.
- 29 imports begin with `self::` or `super::`.
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
- inline-module extraction followed by item ordering and Rustdoc resolution;
- path collisions requiring shortest unique qualification; and
- comments and `cfg` attributes moving with their items.

Black-box Cargo fixtures validate compile-sensitive behavior:

- ordinary library/root items remain in place unless another enabled rule applies;
- inline-module extraction preserves relative paths and macro scope;
- explicit and auto-discovered integration tests retain their old target names;
- alternative `cfg` declarations agree or produce a planning error;
- workspace dependency promotion preserves features and optionality; and
- incompatible dependency sources fail before any file changes.

Transaction tests hash every source before and after forced preflight failure,
post-fix failure, interrupt, simulated process death, and recovery. They verify
that known states restore modes, names, contents, and original absence exactly.
A concurrent or validator-created third state must block automatic recovery
and retain backups rather than overwrite unknown changes.

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

Use disposable pinned Battlement and focused Cargo fixtures. Automated fixtures
cover rule permutations; manual QA concentrates on the user-visible workflow:

- Check the root, one file, a directory, an untracked file, and a non-Git fixture.
  Confirm ignore/exclusion behavior, context boundaries, and relative diagnostics.
- Exercise configuration errors, directory overrides, ignored source annotations,
  all exit codes, deterministic human/JSON output, and timing output.
- Fix a dirty corpus, inspect the diff, run project validation, then require a
  clean scan and a second fix with no filesystem changes. Check test target
  inventory, comments, imports, and dependency feature settings specifically.
- Force baseline and post-fix failures. Interrupt during validation and each
  filesystem transition, then restart. Confirm child termination, journal
  recovery, original absence/modes, and preservation of user changes.
- Run two fixers, edit an input after planning, and introduce a third-state
  change during recovery. Confirm clear errors and retained manual recovery data.
- Run the release benchmarks on the reference machine and inspect phase timings.
  A parser microbenchmark alone is not release acceptance.
