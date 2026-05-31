---
name: ultracos-mode
description: |
  Activate a two-register prompting convention: dense ASCII form for
  the model's internal reasoning, normal human-readable prose for
  user-visible output. Triggers on phrases like "use ultracos",
  "compact thinking", "dense reasoning", "ultracos mode", "think
  compact". The plugin does NOT measure its own impact; see the
  README for how to instrument an A/B if you want validated numbers.
triggers:
  - use ultracos
  - ultracos mode
  - ultracos thinking
  - compact thinking
  - dense reasoning
  - think compact
  - reasoning in ultracos
  - internal ultracos
---

# UltraCoS Dual-Register Convention

**Dense ASCII form for INTERNAL reasoning. Plain prose for USER-VISIBLE output.**

## What this skill does

Declares a convention. The model is asked to split its output into
two registers:

1. **Internal register** (chain-of-thought, scratchpads, tool-call
   reasoning, intermediate plans, error analysis): dense ASCII
   form. Symbols, key=val, abbreviations, pipe-separated lists.

2. **External register** (the final message the human reads): normal
   human-readable language. Full sentences. No symbols unless they
   improve clarity (tables, code blocks, file paths).

## When in doubt about register

| Content type | Register |
|---|---|
| Planning ("first I'll X, then Y") | INTERNAL |
| Step-by-step debugging logs | INTERNAL |
| Tool-call argument reasoning | INTERNAL |
| Self-critique / verification | INTERNAL |
| Direct answer to the user | EXTERNAL |
| Explanation of what you did | EXTERNAL |
| Code blocks | EXTERNAL (the code itself is unchanged) |
| File paths, commands | EXTERNAL |
| Tables presented to user | EXTERNAL |
| Error messages quoted to user | EXTERNAL |
| Commit messages | EXTERNAL |

## Vocabulary (single-source register-variants table)

The vocabulary below is a SINGLE-SOURCE table. Each row carries a
`level` column. The SessionStart hook reads `~/.ultracos/active-level`
and prunes rows whose `level` exceeds the active intensity. Levels are
cumulative: `lite` is a subset of `full`, which is a subset of `ultra`.
With `active-level=lite` only `lite` rows are kept; `full` keeps
`lite + full`; `ultra` keeps all three. `off` suppresses skill injection
entirely.

This replaces the three duplicated vocab blocks (one per level) that
previously lived here; drift between them was a historical bug, now fixed.

<!-- register-variants:start -->
| field | level | value |
|---|---|---|
| arrows | lite | `->` `|` `=` `@` |
| arrows | full | `->` `:` `-` `@` `#` `=` `|` (avoid 3-tok unicode like triangle, identical, forall, exists, checkmark) |
| arrows | ultra | `->` `:` `-` `@` `#` `=` `|` (3-tok unicode such as triangle/identical/forall/exists/checkmark explicitly BANNED) |
| state | lite | `ok` `~` `x` `?` |
| state | full | `ok` healthy / `~` degraded / `x` failed / `?` not-known |
| state | ultra | `ok|~|x|?` (healthy|degraded|failed|not-known) |
| prefixes | lite | `V=verify` `A=analyze` `I=impl` `R=read` `W=write` `F=fix` `E=error` |
| prefixes | full | `V=verify A=analyze I=impl R=read W=write F=fix E=error` |
| prefixes | ultra | `V=verify A=analyze I=impl R=read W=write F=fix E=error` |
| abbreviations | full | `env=environment cfg=config var=variable max=maximum min=minimum` |
| abbreviations | ultra | `env cfg var max min loc fn arg ret err st sts` (definitions dropped; assumed) |
| grammar | full | `-> chain a->b->c` / `state name(ok|~|x)` / `subj: action` / `k=v lines` / `@subj scope` |
| grammar | ultra | `-> (chain: a->b->c) | name(state) | subj:action | k=v | @scope | #tag` |
<!-- register-variants:end -->

A second dialect tuned for the Opus 4.7 tokenizer exists separately
and is not bundled in this plugin.

## Examples

**Internal reasoning, prose form:**
> "I need to first read the file to understand the context, then I'll
> identify the relevant function, and after that I'll modify it to
> handle the edge case the user mentioned."

**Internal reasoning, dense form:**
> R:file -> locate:fn -> I:patch edge_case @user_req

Both express the same plan. The dense form is shorter; how much
shorter in TOKENS depends on the tokenizer and the specific
substitutions, which is why this plugin does not claim a fixed
percentage.

**External output, dense form (BAD — confuses the user):**
> "I:patched fn `foo` @line:42 ok"

**External output, prose (GOOD):**
> "Patched `foo` at line 42. The edge case now returns `None`
> instead of panicking."

## How to use this skill

Type one of the trigger phrases listed above in a normal prompt.
The companion `UserPromptSubmit` hook injects the dual-register
directive into the model's context for the rest of the turn.

To deactivate: say `stop ultracos`.

## What this skill does NOT do

- Does not measure token consumption or billed cost.
- Does not verify the model is adhering to the directive.
- Does not include A/B instrumentation.

See the plugin README for how to instrument your own measurement
across `input_tokens`, `cache_creation_input_tokens`,
`cache_read_input_tokens`, and `output_tokens` if you want
validated numbers on your traffic.

## Companion components

- `UserPromptSubmit/ultracos-mode-detector.py` — detects trigger
  phrases per turn; injects the dual-register directive.
- `PreCompact/ultracos-compact-instructions.py` — adds a dense-form
  mandate to the compaction summary template, with rules to
  preserve restart-critical literals (file paths, IDs, commands,
  error strings, decisions).
- `PostToolUse/ultracos_codec.py` — lossless tool-result codec
  (ANSI strip, JSON minify, blank-line collapse, trailing-marker
  truncation, JSON/YAML/TOML data-shape short-circuit, schema-tag
  prefix, break-even guard).

## Compaction tag schema (read this — affects model behavior)

When a tool_result starts with a line shaped like:

```
[ultracos:compact-v1 shape=<text|json|yaml|toml> ratio=<float> applied=<csv>]
```

it means the payload below was compacted by `ultracos_codec.py`
**losslessly**. The transforms listed in `applied=` are deterministic
and information-preserving for the declared `shape`:

- `ansi-strip` — terminal escape sequences removed
- `json-minify` — JSON whitespace removed (round-trip equal)
- `blank-collapse` — 3+ consecutive blank lines collapsed
- `truncate` — payload exceeded `ULTRACOS_TRUNCATE_BYTES`; a single
  trailing `[truncated: N lines / M bytes hidden]` marker indicates
  the cut. The visible portion is still byte-identical to the
  original prefix.

`ratio` is `compact_tokens / original_tokens`. Values close to 1.0
indicate the codec found little to compact.

### Critical instruction for the model

**Do NOT compensate by being more verbose in your reply.** The
compaction is lossless on the listed dimensions; no information
was rephrased, paraphrased, or summarized. Verbose compensation
(observed in similar tools — see rtk issue #582) inflates output
tokens by ~50% with zero quality gain. Treat tagged payloads as
equivalent to their pre-compaction form.

If you need byte-exact original output (e.g. for a reproducible
artifact like a checksum), ask the user to re-run the command with
`ULTRACOS_DISABLE=1`.

## Schema-tag format (canonical reference)

The compaction marker emitted by `ultracos_codec.py` is referred to
elsewhere in the codebase (hooks, tests, A/B harness) as the
**schema-tag**. It is a single line at the very start of a
`tool_result` payload with this exact shape:

```
[ultracos:compact-v1 shape=<text|json|yaml|toml> ratio=<float> applied=<csv>]
```

Field semantics:

- `ultracos:compact-v1` — fixed namespace + version. If the version
  bumps (e.g. `compact-v2`), the transform list semantics may change;
  the model should re-read this section.
- `shape` — declared data shape of the payload. `text` is the
  default for unstructured output; `json`/`yaml`/`toml` mean the
  payload round-trips through the corresponding parser unchanged.
- `ratio` — `compact_tokens / original_tokens`. Lower = more
  compaction achieved. `~1.0` means the codec found little to strip.
- `applied` — comma-separated list of the lossless transforms that
  ran (subset of `ansi-strip`, `json-minify`, `blank-collapse`,
  `truncate`). Order is informational, not load-bearing.

The tag prefix is configurable via `ULTRACOS_TAG_PREFIX` (default
`[ultracos:compact-v1`); the closing `]` and the fields above are
fixed by the codec.

### What the model should do when it sees the schema-tag

1. **Trust the lossless claim.** Every transform in `applied=` is
   information-preserving for the declared `shape`. Do not ask the
   user to re-run "to see the full output" unless they need byte-
   exact bytes (checksum, diff artifact) — in which case suggest
   `ULTRACOS_DISABLE=1`.
2. **Do not echo or paraphrase the tag** in user-visible output.
   The tag is metadata for the model, not for the human.
3. **Do not inflate the reply** to compensate for the compacted
   input. See "Critical instruction for the model" above — verbose
   compensation is the failure mode this tag is designed to prevent.
4. **If `applied=` contains `truncate`,** the payload was cut at
   `ULTRACOS_TRUNCATE_BYTES`. The visible prefix is byte-identical
   to the original; only the tail is missing. If the user's task
   needs the tail, ask before re-running.
5. **If the tag is absent,** the payload was passed through
   unchanged (e.g. below break-even, or codec disabled). No special
   handling required.
