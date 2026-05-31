# ultracos-stats

Display lifetime and session UltraCoS compaction savings.

## Usage

```
/ultracos-stats
ultracos stats
show ultracos stats
```

## Output

Emits a markdown table with:
- **Session tokens saved**: compaction in this session (or last 1h if CLAUDE_SESSION_ID unavailable)
- **Lifetime tokens saved**: all recorded compaction across all sessions
- **Estimated USD**: $3/M input rate estimate
- **Top 5 tools**: tools that saved the most tokens, descending

The command blocks the normal user prompt (decision: block) so the stats appear inline without further processing.

## --share

Emit a single-line shareable version suitable for social media.

## Example

```
/ultracos-stats
```

Outputs:

```
| Metric | Tokens | USD |
|--------|--------|-----|
| Session | 1,547 | $0.0046 |
| Lifetime | 28,340 | $0.0850 |
```
