# Security Policy

## Supported versions

UltraCoS is pre-1.0. Only the latest tagged release receives fixes.

| Version | Supported |
|---|---|
| 0.1.x   | ✅ |
| < 0.1   | ❌ |

## Reporting a vulnerability

Email **mikko.parkkola@iki.fi** with subject prefix `[ultracos-security]`.

- **Expected response**: within 5 business days
- **Disclosure timeline**: 90-day standard; coordinated disclosure preferred
- **Encrypted contact**: PGP key available on request

Please do **not** open a public GitHub issue for security concerns.

## Local-only contract (BLOCKING)

UltraCoS makes a hard guarantee:

**No hook, skill, or bundled binary makes outbound network calls.**

All compaction, classification, audit logging, and (when shipped) ML
inference happens on the operator's local machine. Tool output read
from `tool_response` content is transformed in-process and written
back via `updatedToolOutput`. Audit rows go to local disk. ML model
weights (when G26 ships) download from Hugging Face on first opt-in
and never re-fetch.

**Network egress is permitted in exactly one place**: `bench/bench.py`
makes Anthropic API calls when the operator explicitly runs it. Bench
is opt-in, off-path, and clearly marked.

### What this means for adopters

- **No signup.** Install plugin, done.
- **No API key.** Plugin does not authenticate against anything.
- **No telemetry.** Audit JSONL stays on disk; never transmitted.
- **No DPA / SOC2 / BAA dependency on UltraCoS itself.** Compliance posture inherits whatever your local environment already has — UltraCoS adds zero new external trust dependencies.
- **Tool output never leaves the device.** Period.

### Enforcement

The `hooks/PostToolUse/ultracos_codec.py` module imports zero
network-capable modules (verified by lint: no `urllib`, no `requests`,
no `socket`, no `http.client`, no `urllib3`). Future PRs touching the
hook MUST preserve this invariant; CI will lint for this invariant
(G5 GitHub Actions) lands.

## Threat model

UltraCoS is a client-side Claude Code plugin. It:

- **Reads** the contents of every tool_result that passes through its
  PostToolUse hook (`hooks/PostToolUse/ultracos_codec.py`).
- **Transforms** that content using deterministic regex passes
  (ANSI strip, JSON whitespace minify, blank-line collapse, truncation)
  and emits a modified `updatedToolOutput`.
- **Writes** audit rows to `~/.ultracos/audit.jsonl` summarising what
  was compacted (no payload content stored).
- **Never** transmits content over the network. There are zero outbound
  network calls in the codec or any hook.

### In scope

- Payload-injection vulnerabilities (e.g. a crafted tool_result causing
  the codec to corrupt or exfiltrate other content)
- Path-traversal vulnerabilities in tee / audit file handling
- Resource-exhaustion vulnerabilities (oversized input, regex DoS)
- Privilege-escalation vulnerabilities in the hook execution path
- Fail-open contract violations (any path where an exception causes
  the hook to **block** input rather than pass through)

### Out of scope

- Third-party LLM provider security (Anthropic, OpenAI, etc. own the
  API security boundary)
- Claude Code platform / IDE vulnerabilities (report to Anthropic)
- Local-machine-level threats outside the plugin's execution
  (`HOME` environment variable manipulation by a malicious actor with
  shell access is not a plugin vulnerability)
- Adversarial prompt injection by the user themselves

## Defensive coding posture

- `#!/usr/bin/env python3` only; no compiled binaries
- Fail-open on every exception path; the hook contract is to never
  block tool execution even on internal error
- 5-second timeout cap in hooks.json
- `ULTRACOS_DISABLE=1` environment variable forces immediate
  pass-through with no transforms (kill switch)
- `ULTRACOS_MAX_INPUT_BYTES` caps stdin read; oversized payloads bail
  to pass-through with an audit row
- No `eval`, `exec`, `subprocess` (other than in test harness)
- No file writes outside `~/.ultracos/` and the operator-chosen tee
  directory

## Safe harbor

UltraCoS welcomes good-faith security research. If you make a good-faith effort to comply with this policy during your research, we will:

- Not pursue legal action against you for accidental, good-faith violations of this policy.
- Work with you to understand and resolve the issue quickly.
- Recognise your contribution publicly (see Hall of Fame below) if you are the first reporter and wish to be credited.

To qualify for safe harbor:

- Make a good-faith effort to avoid privacy violations, data destruction, and degradation of services.
- Only interact with accounts you own, with explicit permission of the account holder.
- Do not exfiltrate data beyond the minimum necessary to demonstrate the vulnerability.
- Give us reasonable time to investigate and remediate before any public disclosure (see SLA targets below).
- Do not perform attacks that target physical safety of users.

This safe harbor applies to security research conducted against the UltraCoS codebase and its bundled artifacts only. It does not extend to third-party systems (Anthropic API, GitHub, Hugging Face). Consult their respective security policies for those.

## Service-level targets

| Stage | Target |
|---|---|
| Acknowledgement of receipt | 2 business days |
| Initial triage and severity assessment | 5 business days |
| Status update cadence during fix | Weekly |
| Patch released (High/Critical) | 30 days |
| Patch released (Medium) | 60 days |
| Patch released (Low) | 90 days |
| Public disclosure | After patch ships, coordinated with reporter |

## Hall of Fame

We publicly recognise researchers who responsibly disclose vulnerabilities in UltraCoS. To be listed:

- Be the first reporter of a previously unknown, valid vulnerability.
- Follow the coordinated disclosure timeline above.
- Opt in to public credit (anonymous credit also available on request).

| Researcher | Date | Severity | Issue |
|---|---|---|---|

(No disclosures yet.)
